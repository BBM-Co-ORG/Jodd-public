//! "What's on my TV": the Tauri commands over `tv_link`, `tv_now` and the
//! Lounge client (spec 2026-11-02).
//!
//! Gotcha #15: `list_tvs` reads the flag file only. The keychain is read only
//! inside a user-pressed command (`tv_now_playing`, `refresh_tv`), and always
//! in `spawn_blocking`. The screen ID never leaves this module: it is read,
//! handed to the Lounge client, and dropped. No return value, error or log
//! line carries it; log lines name the command and the `tv_id` only.
//!
//! The command bodies are thin: each calls an inner function that takes the
//! `TvLinks` plus the network call as an injected closure, so the tests drive
//! the whole orchestration with a temp dir, an in-memory keychain and fakes.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use tauri::State;
use tokio_util::sync::CancellationToken;

use crate::ingest::tv_lounge::{self, Deadlines, LoungeEndpoints, PairError, Pairing, Sighting, TvError};
use crate::ingest::FetchPolicy;
use crate::log;
use crate::tv_link::{LinkError, TvHealth, TvLinks, TvRow};
use crate::tv_now::{self, NowError, Outcome, PerTv};
use crate::{AiRequestCleanup, AppState};

// ── errors and shapes the frontend sees ────────────────────────────────────

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum TvCmdError {
    BadCode,
    TooMany,
    BelongsToOther { label: String },
    NotFound,
    Network,
    Store,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct LinkResult {
    pub row: TvRow,
    pub already_linked: bool,
}

/// A `LinkError` as the frontend sees it. A `Store` message can carry an OS
/// error and a path, so it is logged here and never returned.
fn map_link(cmd: &str, e: LinkError) -> TvCmdError {
    match e {
        LinkError::TooMany => TvCmdError::TooMany,
        LinkError::BelongsToOther { label } => TvCmdError::BelongsToOther { label },
        LinkError::NotFound => TvCmdError::NotFound,
        LinkError::Store(msg) => {
            log!("{cmd}: store error: {msg}");
            TvCmdError::Store
        }
    }
}

fn map_pair(e: PairError) -> TvCmdError {
    match e {
        PairError::BadCode => TvCmdError::BadCode,
        PairError::Network(_) => TvCmdError::Network,
    }
}

// ── the one TvLinks ────────────────────────────────────────────────────────

static LINKS: OnceLock<Arc<TvLinks>> = OnceLock::new();

/// Built on first use. A failure is not cached, so the next press retries.
fn links() -> Result<Arc<TvLinks>, String> {
    if let Some(l) = LINKS.get() {
        return Ok(l.clone());
    }
    let built = Arc::new(TvLinks::app()?);
    Ok(LINKS.get_or_init(|| built).clone())
}

/// [`links`] for an async command: the first build creates the config dir, so
/// it runs off the async runtime (R16). Once built it is a plain read.
async fn links_async() -> Result<Arc<TvLinks>, String> {
    if let Some(l) = LINKS.get() {
        return Ok(l.clone());
    }
    tokio::task::spawn_blocking(links).await.map_err(|_| "background task failed".to_string())?
}

/// Runs file or keychain work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|_| "background task failed".to_string())
}

fn store_failed(cmd: &str, why: &str) -> TvCmdError {
    log!("{cmd}: {why}");
    TvCmdError::Store
}

// ── asking the TVs ─────────────────────────────────────────────────────────

/// How long the default TV has to itself before the others are asked too.
/// A guess: the spike never measured how long a TV takes to answer. Tune it
/// after the spec §9 live pass.
const HEAD_START: Duration = Duration::from_secs(2);

/// The press's wall-clock limit is `overall`; every `now_playing` it starts is
/// given `overall` minus this. A `now_playing` that runs to its own deadline
/// still has to hand back what it heard, and that must land before the press's
/// timer fires, or an answer arriving on the deadline reads as "timed out"
/// (R26b).
const DEADLINE_GRACE: Duration = Duration::from_millis(250);

/// One press, one deadline (R14). `overall` is the wall-clock limit of the
/// press; the budget each ask is given is `overall` less [`DEADLINE_GRACE`], so
/// an ask that runs to its own deadline still lands before the press's.
/// `order[0]` is the default; it is asked alone with the whole budget. If it is `Ok` it wins at any moment and
/// nothing else matters. If it fails, or `head_start` passes with it still
/// running, the others are asked too, all at once, each with the budget that
/// is left (`overall` minus the time already spent). The press ends when the
/// default and every started other have resolved, or at `overall`, whichever
/// comes first.
///
/// A default `Cancelled` ends the press at once (R15). Returns the default's
/// answer and the others' in `order` order. Others that were never started
/// (the default won inside the head start) or were still running when the
/// default won are left out of the list; at the deadline a started one that has
/// not answered is `Network("timed out")`, as is the default. An empty `order`
/// yields `(None, [])`.
async fn ask_in_order<F, Fut>(
    order: Vec<(String, String)>,
    ask: F,
    overall: Duration,
    head_start: Duration,
) -> (Option<PerTv>, Vec<PerTv>)
where
    F: Fn(String, Duration) -> Fut,
    Fut: Future<Output = Result<Sighting, TvError>> + Send + 'static,
{
    use tokio::time::{sleep, sleep_until, Instant};

    let mut order = order.into_iter();
    let Some((first_id, first_label)) = order.next() else {
        return (None, Vec::new());
    };
    let rest: Vec<(String, String)> = order.collect();
    let deadline = Instant::now() + overall;
    // What every ask is held to: it ends before the press's own timer.
    let ask_deadline = deadline - overall.min(DEADLINE_GRACE);

    let mut default_fut = Box::pin(ask(first_id.clone(), ask_deadline.saturating_duration_since(Instant::now())));
    let mut default_res: Option<Result<Sighting, TvError>> = None;
    let mut set = tokio::task::JoinSet::new();
    let mut answers: Vec<Option<Result<Sighting, TvError>>> = rest.iter().map(|_| None).collect();
    let mut started = false;
    let head = sleep(head_start);
    tokio::pin!(head);
    let timeout = sleep_until(deadline);
    tokio::pin!(timeout);

    let start_others = |set: &mut tokio::task::JoinSet<(usize, Result<Sighting, TvError>)>| {
        let budget = ask_deadline.saturating_duration_since(Instant::now());
        for (i, (id, _)) in rest.iter().enumerate() {
            let fut = ask(id.clone(), budget);
            set.spawn(async move { (i, fut.await) });
        }
    };

    loop {
        tokio::select! {
            r = &mut default_fut, if default_res.is_none() => {
                let decisive = matches!(r, Ok(_) | Err(TvError::Cancelled));
                default_res = Some(r);
                if decisive {
                    break;
                }
                if !started {
                    start_others(&mut set);
                    started = true;
                }
            }
            _ = &mut head, if !started => {
                start_others(&mut set);
                started = true;
            }
            Some(joined) = set.join_next(), if !set.is_empty() => {
                if let Ok((i, result)) = joined {
                    answers[i] = Some(result);
                }
            }
            _ = &mut timeout => break,
        }
        if default_res.is_some() && started && set.is_empty() {
            break;
        }
    }

    // Dropping the set aborts whatever is still running.
    drop(set);
    let decided = matches!(default_res, Some(Ok(_)) | Some(Err(TvError::Cancelled)));
    let first = PerTv {
        result: default_res.unwrap_or_else(|| Err(TvError::Network("timed out".into()))),
        tv_id: first_id,
        label: first_label,
    };
    let others = rest
        .into_iter()
        .zip(answers)
        .filter_map(|((tv_id, label), result)| {
            let result = match result {
                Some(r) => r,
                None if started && !decided => Err(TvError::Network("timed out".into())),
                None => return None,
            };
            Some(PerTv { tv_id, label, result })
        })
        .collect();
    (Some(first), others)
}

/// One press. `ask(screen_id, budget)` is the network call; everything else
/// (the order, the cancel and missing-secret rules, the marking, `pick`)
/// happens here.
async fn press<A, AFut>(
    links: Arc<TvLinks>,
    only: Option<String>,
    cancel: CancellationToken,
    overall: Duration,
    head_start: Duration,
    ask: A,
) -> Result<Outcome, NowError>
where
    A: Fn(String, Duration) -> AFut + Send + Sync + 'static,
    AFut: Future<Output = Result<(Sighting, Option<String>), TvError>> + Send + 'static,
{
    let rows = {
        let l = links.clone();
        blocking(move || l.list()).await.map_err(|why| {
            log!("tv_now_playing: {why}");
            NowError::Network
        })?
    };
    let explicit = only.is_some();
    let order: Vec<(String, String)> = match &only {
        Some(id) => rows.iter().filter(|r| &r.tv_id == id).map(|r| (r.tv_id.clone(), r.label.clone())).collect(),
        None => rows.iter().map(|r| (r.tv_id.clone(), r.label.clone())).collect(),
    };
    if order.is_empty() {
        return Err(NowError::NoTvs);
    }

    let ask = Arc::new(ask);
    let models: Arc<Mutex<HashMap<String, Option<String>>>> = Arc::default();
    let (default, others) = {
        let (links, models, cancel) = (links.clone(), models.clone(), cancel.clone());
        ask_in_order(
            order,
            move |tv_id: String, budget: Duration| {
                let (links, ask, models, cancel) = (links.clone(), ask.clone(), models.clone(), cancel.clone());
                async move {
                    // R15: a cancelled press reads nothing from the keychain.
                    if cancel.is_cancelled() {
                        return Err(TvError::Cancelled);
                    }
                    let t0 = tokio::time::Instant::now();
                    let (l, id) = (links.clone(), tv_id.clone());
                    // The blocking thread may outlive this race; its result is then dropped.
                    let read = tokio::task::spawn_blocking(move || l.screen_id(&id));
                    let read = tokio::select! {
                        r = read => r,
                        _ = cancel.cancelled() => return Err(TvError::Cancelled),
                        _ = tokio::time::sleep(budget) => return Err(TvError::Network("timed out".into())),
                    };
                    let screen_id = match read {
                        Ok(Ok(Some(s))) => s,
                        // The secret is gone: that TV needs a new code, and the network cannot help.
                        Ok(Ok(None)) => return Err(TvError::Unlinked),
                        _ => return Err(TvError::Network("could not read the saved TV".into())),
                    };
                    let (sighting, model) = ask(screen_id, budget.saturating_sub(t0.elapsed())).await?;
                    models.lock().unwrap().insert(tv_id, model);
                    Ok(sighting)
                }
            },
            overall,
            head_start,
        )
        .await
    };

    // Pick consumes its input: collect what the marking needs first.
    let all: Vec<&PerTv> = default.iter().chain(others.iter()).collect();
    let unlinked = tv_now::unlinked_ids(&all);
    let answered: Vec<(String, Option<String>)> = {
        let models = models.lock().unwrap();
        all.iter()
            .filter_map(|t| match &t.result {
                Ok(_) => Some((t.tv_id.clone(), models.get(&t.tv_id).cloned().flatten())),
                // Idle is still reachable and linked: Online, with no model to
                // record (R26c). Network and Cancelled say nothing about the TV.
                Err(TvError::NothingPlaying) => Some((t.tv_id.clone(), None)),
                Err(_) => None,
            })
            .collect()
    };
    if !unlinked.is_empty() || !answered.is_empty() {
        let l = links.clone();
        let marked = blocking(move || {
            for id in &unlinked {
                note_mark("tv_now_playing", id, l.mark(id, TvHealth::NeedsRepair, None));
            }
            for (id, model) in &answered {
                note_mark("tv_now_playing", id, l.mark(id, TvHealth::Online, model.as_deref()));
            }
        })
        .await;
        if marked.is_err() {
            log!("tv_now_playing: marking the TVs did not finish");
        }
    }

    if explicit {
        let one: Vec<PerTv> = default.into_iter().chain(others).collect();
        tv_now::pick(None, one)
    } else {
        tv_now::pick(default, others)
    }
}

/// A TV unpaired between the ask and the mark is simply gone (`NotFound`).
fn note_mark(cmd: &str, tv_id: &str, r: Result<TvRow, LinkError>) {
    match r {
        Ok(_) | Err(LinkError::NotFound) => {}
        Err(e) => log!("{cmd}: could not record the check for tv_id={tv_id}: {e:?}"),
    }
}

// ── linking ────────────────────────────────────────────────────────────────

async fn device_id_of(links: &Arc<TvLinks>, cmd: &str) -> Result<String, TvCmdError> {
    let l = links.clone();
    match blocking(move || l.device_id()).await {
        Ok(Ok(id)) => Ok(id),
        Ok(Err(why)) | Err(why) => Err(store_failed(cmd, &why)),
    }
}

/// Pair, then one `check` for the model (its failure leaves the model `None`).
async fn pair_and_check<P, PFut, C, CFut>(
    links: &Arc<TvLinks>,
    cmd: &str,
    pair: P,
    check: C,
) -> Result<(Pairing, Option<String>), TvCmdError>
where
    P: FnOnce() -> PFut,
    PFut: Future<Output = Result<Pairing, PairError>>,
    C: FnOnce(String, String) -> CFut,
    CFut: Future<Output = Result<Option<String>, TvError>>,
{
    let device_id = device_id_of(links, cmd).await?;
    let pairing = pair().await.map_err(map_pair)?;
    let model = check(pairing.screen_id.clone(), device_id).await.unwrap_or_default();
    Ok((pairing, model))
}

async fn link_with<P, PFut, C, CFut>(links: Arc<TvLinks>, pair: P, check: C) -> Result<LinkResult, TvCmdError>
where
    P: FnOnce() -> PFut,
    PFut: Future<Output = Result<Pairing, PairError>>,
    C: FnOnce(String, String) -> CFut,
    CFut: Future<Output = Result<Option<String>, TvError>>,
{
    let (pairing, model) = pair_and_check(&links, "link_tv", pair, check).await?;
    let l = links.clone();
    let saved = blocking(move || l.link(&pairing, model.as_deref())).await.map_err(|why| store_failed("link_tv", &why))?;
    let (row, already_linked) = saved.map_err(|e| map_link("link_tv", e))?;
    Ok(LinkResult { row, already_linked })
}

async fn repair_with<P, PFut, C, CFut>(links: Arc<TvLinks>, tv_id: String, pair: P, check: C) -> Result<TvRow, TvCmdError>
where
    P: FnOnce() -> PFut,
    PFut: Future<Output = Result<Pairing, PairError>>,
    C: FnOnce(String, String) -> CFut,
    CFut: Future<Output = Result<Option<String>, TvError>>,
{
    // Do not spend a pairing code on a TV that is no longer in the list.
    let l = links.clone();
    let rows = blocking(move || l.list()).await.map_err(|why| store_failed("repair_tv", &why))?;
    if !rows.iter().any(|r| r.tv_id == tv_id) {
        return Err(TvCmdError::NotFound);
    }
    let (pairing, model) = pair_and_check(&links, "repair_tv", pair, check).await?;
    let l = links.clone();
    let saved = blocking(move || l.repair(&tv_id, &pairing, model.as_deref())).await.map_err(|why| store_failed("repair_tv", &why))?;
    saved.map_err(|e| map_link("repair_tv", e))
}

/// `Ok` -> Online, `Unlinked` (or a missing secret) -> NeedsRepair, anything
/// else -> Unreachable. `check(screen_id, device_id)` is the network call.
async fn refresh_with<C, CFut>(links: Arc<TvLinks>, tv_id: String, check: C) -> Result<TvRow, TvCmdError>
where
    C: FnOnce(String, String) -> CFut,
    CFut: Future<Output = Result<Option<String>, TvError>>,
{
    let l = links.clone();
    let rows = blocking(move || l.list()).await.map_err(|why| store_failed("refresh_tv", &why))?;
    if !rows.iter().any(|r| r.tv_id == tv_id) {
        return Err(TvCmdError::NotFound);
    }
    let (l, id) = (links.clone(), tv_id.clone());
    let secret = blocking(move || l.screen_id(&id)).await.map_err(|why| store_failed("refresh_tv", &why))?;
    let (health, model) = match secret {
        Err(why) => return Err(store_failed("refresh_tv", &why)),
        Ok(None) => (TvHealth::NeedsRepair, None),
        Ok(Some(screen_id)) => {
            let device_id = device_id_of(&links, "refresh_tv").await?;
            match check(screen_id, device_id).await {
                Ok(model) => (TvHealth::Online, model),
                Err(TvError::Unlinked) => (TvHealth::NeedsRepair, None),
                Err(_) => (TvHealth::Unreachable, None),
            }
        }
    };
    let l = links.clone();
    let marked = blocking(move || l.mark(&tv_id, health, model.as_deref())).await.map_err(|why| store_failed("refresh_tv", &why))?;
    marked.map_err(|e| map_link("refresh_tv", e))
}

// ── the commands ───────────────────────────────────────────────────────────

async fn links_or_store(cmd: &str) -> Result<Arc<TvLinks>, TvCmdError> {
    links_async().await.map_err(|why| store_failed(cmd, &why))
}

/// Reads the flag file only (gotcha #15).
#[tauri::command]
pub async fn list_tvs() -> Vec<TvRow> {
    let Ok(links) = links_async().await else {
        log!("list_tvs: the TV list is unavailable");
        return Vec::new();
    };
    blocking(move || links.list()).await.unwrap_or_default()
}

#[tauri::command]
pub async fn link_tv(code: String) -> Result<LinkResult, TvCmdError> {
    let links = links_or_store("link_tv").await?;
    let cancel = CancellationToken::new();
    let (ep, cancel2) = (LoungeEndpoints::default(), cancel.clone());
    let result = link_with(
        links,
        || async move { tv_lounge::pair(&ep, FetchPolicy::default(), &code, &cancel2).await },
        |screen_id, device_id| async move {
            tv_lounge::check(&LoungeEndpoints::default(), FetchPolicy::default(), &screen_id, &device_id, &cancel).await
        },
    )
    .await;
    log!("link_tv: {}", if result.is_ok() { "linked" } else { "failed" });
    result
}

#[tauri::command]
pub async fn repair_tv(tv_id: String, code: String) -> Result<TvRow, TvCmdError> {
    let links = links_or_store("repair_tv").await?;
    let cancel = CancellationToken::new();
    let (ep, cancel2) = (LoungeEndpoints::default(), cancel.clone());
    let result = repair_with(
        links,
        tv_id.clone(),
        || async move { tv_lounge::pair(&ep, FetchPolicy::default(), &code, &cancel2).await },
        |screen_id, device_id| async move {
            tv_lounge::check(&LoungeEndpoints::default(), FetchPolicy::default(), &screen_id, &device_id, &cancel).await
        },
    )
    .await;
    log!("repair_tv: tv_id={tv_id} {}", if result.is_ok() { "repaired" } else { "failed" });
    result
}

#[tauri::command]
pub async fn refresh_tv(tv_id: String) -> Result<TvRow, TvCmdError> {
    let links = links_or_store("refresh_tv").await?;
    let cancel = CancellationToken::new();
    let result = refresh_with(links, tv_id.clone(), |screen_id, device_id| async move {
        tv_lounge::check(&LoungeEndpoints::default(), FetchPolicy::default(), &screen_id, &device_id, &cancel).await
    })
    .await;
    log!("refresh_tv: tv_id={tv_id} {}", if result.is_ok() { "checked" } else { "failed" });
    result
}

#[tauri::command]
pub async fn rename_tv(tv_id: String, label: String) -> Result<TvRow, TvCmdError> {
    let links = links_or_store("rename_tv").await?;
    let id = tv_id.clone();
    let saved = blocking(move || links.rename(&id, &label)).await.map_err(|why| store_failed("rename_tv", &why))?;
    log!("rename_tv: tv_id={tv_id}");
    saved.map_err(|e| map_link("rename_tv", e))
}

#[tauri::command]
pub async fn set_default_tv(tv_id: String) -> Result<(), TvCmdError> {
    let links = links_or_store("set_default_tv").await?;
    let id = tv_id.clone();
    let saved = blocking(move || links.set_default(&id)).await.map_err(|why| store_failed("set_default_tv", &why))?;
    log!("set_default_tv: tv_id={tv_id}");
    saved.map_err(|e| map_link("set_default_tv", e))
}

#[tauri::command]
pub async fn unpair_tv(tv_id: String) -> Result<(), TvCmdError> {
    let links = links_or_store("unpair_tv").await?;
    let id = tv_id.clone();
    let saved = blocking(move || links.unpair(&id)).await.map_err(|why| store_failed("unpair_tv", &why))?;
    log!("unpair_tv: tv_id={tv_id}");
    saved.map_err(|e| map_link("unpair_tv", e))
}

/// "What's on my TV?" The token goes into `in_flight_extracts` under
/// `request_id`, so the existing `cancel_extraction` stops a press; the guard
/// removes it on every exit path.
#[tauri::command]
pub async fn tv_now_playing(request_id: String, tv_id: Option<String>, state: State<'_, AppState>) -> Result<Outcome, NowError> {
    let ep = LoungeEndpoints::default();
    now_playing_command(
        request_id,
        tv_id,
        &state.in_flight_extracts,
        links_async(),
        Deadlines::default().overall,
        HEAD_START,
        move |device_id: String, cancel: CancellationToken| {
            move |screen_id: String, budget: Duration| {
                let (ep, device_id, cancel) = (ep.clone(), device_id.clone(), cancel.clone());
                async move {
                    let deadlines = Deadlines { overall: budget, after_video: Deadlines::default().after_video };
                    tv_lounge::now_playing(&ep, FetchPolicy::default(), &screen_id, &device_id, deadlines, &cancel).await
                }
            }
        },
    )
    .await
}

/// The body of [`tv_now_playing`], with the TV list and the network call
/// injected so a test can drive it.
///
/// The cancel token is registered and its cleanup guard made before the first
/// await (R26a). `links` can take a while on a first press (it builds the
/// config dir off the runtime); a `cancel_extraction` that arrives meanwhile
/// must find the request, or the press would run to its deadline unstopped.
async fn now_playing_command<LFut, MakeAsk, A, AFut>(
    request_id: String,
    tv_id: Option<String>,
    requests: &Mutex<HashMap<String, CancellationToken>>,
    links: LFut,
    overall: Duration,
    head_start: Duration,
    make_ask: MakeAsk,
) -> Result<Outcome, NowError>
where
    LFut: Future<Output = Result<Arc<TvLinks>, String>>,
    MakeAsk: FnOnce(String, CancellationToken) -> A,
    A: Fn(String, Duration) -> AFut + Send + Sync + 'static,
    AFut: Future<Output = Result<(Sighting, Option<String>), TvError>> + Send + 'static,
{
    let cancel = CancellationToken::new();
    requests.lock().unwrap().insert(request_id.clone(), cancel.clone());
    let _cleanup = AiRequestCleanup { requests, id: request_id };

    let links = links.await.map_err(|why| {
        log!("tv_now_playing: {why}");
        NowError::Network
    })?;
    let device_id = {
        let l = links.clone();
        match blocking(move || l.device_id()).await {
            Ok(Ok(id)) => id,
            Ok(Err(why)) | Err(why) => {
                log!("tv_now_playing: {why}");
                return Err(NowError::Network);
            }
        }
    };
    let ask = make_ask(device_id, cancel.clone());
    let result = press(links, tv_id.clone(), cancel, overall, head_start, ask).await;
    log!("tv_now_playing: tv_id={tv_id:?} {}", if result.is_ok() { "answered" } else { "no answer" });
    result
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tv_link::TvSecrets;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::future::Future;
    use std::time::{Duration, Instant};

    /// An in-memory keychain; cloning shares the storage.
    #[derive(Clone, Default)]
    struct Mem {
        map: Arc<Mutex<HashMap<String, String>>>,
        fail_get: Arc<std::sync::atomic::AtomicBool>,
        /// How many times the keychain was read.
        gets: Arc<AtomicUsize>,
        /// A slow keychain: `get` sleeps this long (on the blocking thread).
        get_delay_ms: Arc<std::sync::atomic::AtomicU64>,
    }

    impl TvSecrets for Mem {
        fn get(&self, tv_id: &str) -> Result<Option<String>, String> {
            self.gets.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(self.get_delay_ms.load(Ordering::SeqCst)));
            if self.fail_get.load(Ordering::SeqCst) {
                return Err("keychain locked".into());
            }
            Ok(self.map.lock().unwrap().get(tv_id).cloned())
        }
        fn set(&self, tv_id: &str, screen_id: &str) -> Result<(), String> {
            self.map.lock().unwrap().insert(tv_id.into(), screen_id.into());
            Ok(())
        }
        fn delete(&self, tv_id: &str) -> Result<(), String> {
            self.map.lock().unwrap().remove(tv_id);
            Ok(())
        }
    }

    fn setup() -> (tempfile::TempDir, Mem, Arc<TvLinks>) {
        let dir = tempfile::tempdir().unwrap();
        let mem = Mem::default();
        let links = Arc::new(TvLinks::new(dir.path().to_path_buf(), Box::new(mem.clone())));
        (dir, mem, links)
    }

    fn pairing(screen: &str) -> Pairing {
        Pairing { screen_id: screen.into(), screen_name: "YouTube on TV".into() }
    }

    /// Links screens "screen-a", "screen-b", ... labelled "A", "B", ...; the
    /// first is the default. Returns the tv_ids in that order.
    fn link_all(links: &TvLinks, names: &[&str]) -> Vec<String> {
        names
            .iter()
            .map(|n| links.link(&pairing(&format!("screen-{}", n.to_lowercase())), Some(n)).unwrap().0.tv_id)
            .collect()
    }

    fn sighting(video: &str) -> Sighting {
        Sighting { video_id: video.into(), seconds: Some(72), duration: None, state: Some("1".into()) }
    }

    fn row_of(links: &TvLinks, tv_id: &str) -> TvRow {
        links.list().into_iter().find(|r| r.tv_id == tv_id).unwrap()
    }

    // ── ask_in_order ──

    const OVERALL: Duration = Duration::from_secs(5);
    const HEAD: Duration = Duration::from_millis(100);
    /// For tests that do not exercise the head start: a slow runner cannot trip it.
    const LONG_HEAD: Duration = Duration::from_secs(2);

    fn trio() -> Vec<(String, String)> {
        vec![("a".to_string(), "A".to_string()), ("b".into(), "B".into()), ("c".into(), "C".into())]
    }

    #[tokio::test]
    async fn default_answering_skips_others() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (default, others) = ask_in_order(
            trio(),
            move |_id, _budget| {
                c.fetch_add(1, Ordering::SeqCst);
                async { Ok(sighting("vid")) }
            },
            OVERALL,
            HEAD,
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(default.unwrap().tv_id, "a");
        assert!(others.is_empty());
    }

    #[tokio::test]
    async fn default_ok_inside_the_head_start_asks_nobody_else() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (default, others) = ask_in_order(
            trio(),
            move |_id, _budget| {
                c.fetch_add(1, Ordering::SeqCst);
                async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(sighting("vid"))
                }
            },
            OVERALL,
            Duration::from_millis(400),
        )
        .await;
        // Give a wrongly-started task time to show itself.
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(default.unwrap().result.is_ok() && others.is_empty());
    }

    #[tokio::test]
    async fn default_empty_asks_rest_concurrently() {
        let started = Instant::now();
        // A head start of 5 s proves the others start at once, not after it.
        let (default, others) = ask_in_order(
            trio(),
            |id, _budget| async move {
                if id == "a" {
                    return Err(TvError::NothingPlaying);
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
                Ok(sighting(&id))
            },
            OVERALL,
            Duration::from_secs(5),
        )
        .await;
        // b and c together: sequential would be 600 ms.
        assert!(started.elapsed() < Duration::from_millis(550), "took {:?}", started.elapsed());
        assert!(default.unwrap().result.is_err());
        assert_eq!(others.iter().map(|t| t.tv_id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
        assert!(others.iter().all(|t| t.result.is_ok()));
    }

    #[tokio::test]
    async fn slow_default_loses_nothing_when_the_others_start_after_the_head_start() {
        let started = Instant::now();
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let a = asked.clone();
        // The default is still running at the head start, fails at 300 ms.
        let (default, others) = ask_in_order(
            trio(),
            move |id, _budget| {
                a.lock().unwrap().push(id.clone());
                async move {
                    if id == "a" {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        return Err(TvError::NothingPlaying);
                    }
                    Ok(sighting(&id))
                }
            },
            OVERALL,
            HEAD,
        )
        .await;
        assert!(started.elapsed() < Duration::from_millis(700), "took {:?}", started.elapsed());
        assert_eq!(*asked.lock().unwrap(), ["a", "b", "c"]);
        assert!(default.unwrap().result.is_err());
        assert_eq!(others.len(), 2);
    }

    #[tokio::test]
    async fn rest_keeps_input_order_whatever_finishes_first() {
        let (_d, others) = ask_in_order(
            trio(),
            |id, _budget| async move {
                let wait = if id == "b" { 150 } else { 10 };
                tokio::time::sleep(Duration::from_millis(wait)).await;
                if id == "a" { Err(TvError::NothingPlaying) } else { Ok(sighting(&id)) }
            },
            OVERALL,
            HEAD,
        )
        .await;
        assert_eq!(others.iter().map(|t| t.tv_id.as_str()).collect::<Vec<_>>(), ["b", "c"]);
    }

    #[tokio::test]
    async fn no_tvs_asks_nobody() {
        let (d, o) = ask_in_order(vec![], |_, _| async { Err(TvError::NothingPlaying) }, OVERALL, HEAD).await;
        assert!(d.is_none() && o.is_empty());
    }

    #[tokio::test]
    async fn the_others_get_what_is_left_of_one_overall_deadline() {
        let budgets = Arc::new(Mutex::new(HashMap::<String, Duration>::new()));
        let b = budgets.clone();
        let started = Instant::now();
        // Nobody ever answers: the press must end at the one 600 ms deadline.
        let (default, others) = ask_in_order(
            trio(),
            move |id, budget| {
                b.lock().unwrap().insert(id, budget);
                async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Err(TvError::NothingPlaying)
                }
            },
            Duration::from_millis(600),
            Duration::from_millis(100),
        )
        .await;
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(550) && took < Duration::from_millis(900), "took {took:?}");
        let budgets = budgets.lock().unwrap();
        // The default gets the whole budget: the press's 600 ms less the grace.
        assert!(budgets["a"] > Duration::from_millis(300) && budgets["a"] <= Duration::from_millis(600) - DEADLINE_GRACE, "a: {:?}", budgets["a"]);
        for id in ["b", "c"] {
            assert!(budgets[id] > Duration::ZERO && budgets[id] <= Duration::from_millis(250), "{id}: {:?}", budgets[id]);
        }
        assert_eq!(default.unwrap().result, Err(TvError::Network("timed out".into())));
        assert_eq!(others.len(), 2);
        assert!(others.iter().all(|t| t.result == Err(TvError::Network("timed out".into()))));
    }

    /// R26b: an ask that runs to its own deadline and then takes a moment to
    /// hand back what it heard must not lose to the press's timer.
    #[tokio::test]
    async fn an_answer_landing_just_after_the_asks_own_deadline_is_still_an_answer() {
        let (default, others) = ask_in_order(
            trio(),
            |id, budget| async move {
                // its own deadline passes, then 50 ms to wrap up
                tokio::time::sleep(budget + Duration::from_millis(50)).await;
                Ok(sighting(&id))
            },
            Duration::from_millis(600),
            LONG_HEAD,
        )
        .await;
        assert_eq!(default.unwrap().result, Ok(sighting("a")));
        assert!(others.is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_default_ends_the_press_at_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let (default, others) = ask_in_order(
            trio(),
            move |_id, _budget| {
                c.fetch_add(1, Ordering::SeqCst);
                async { Err(TvError::Cancelled) }
            },
            OVERALL,
            HEAD,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(default.unwrap().result, Err(TvError::Cancelled));
        assert!(others.is_empty());
    }

    // ── press ──

    type Answer = Result<(Sighting, Option<String>), TvError>;

    /// An `ask` that records each screen ID it is called with and answers per screen.
    fn recording_ask(
        seen: Arc<Mutex<Vec<String>>>,
        answer: impl Fn(&str) -> Answer + Send + Sync + 'static,
    ) -> impl Fn(String, Duration) -> std::future::Ready<Answer> + Send + Sync + 'static {
        move |screen_id, _budget| {
            seen.lock().unwrap().push(screen_id.clone());
            std::future::ready(answer(&screen_id))
        }
    }

    /// `press` with a live token and the test deadlines.
    async fn go<A, F>(links: Arc<TvLinks>, only: Option<String>, ask: A) -> Result<Outcome, NowError>
    where
        A: Fn(String, Duration) -> F + Send + Sync + 'static,
        F: Future<Output = Answer> + Send + 'static,
    {
        press(links, only, CancellationToken::new(), OVERALL, HEAD, ask).await
    }

    #[tokio::test]
    async fn unlinked_tvs_get_marked() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        let seen = Arc::new(Mutex::new(vec![]));
        let ask = recording_ask(seen, |s| match s {
            "screen-a" => Err(TvError::NothingPlaying),
            _ => Err(TvError::Unlinked),
        });
        let out = go(links.clone(), None, ask).await;
        assert_eq!(out, Err(NowError::NeedsRepair { label: "B".into() }));
        assert!(row_of(&links, &ids[1]).needs_repair);
        assert!(!row_of(&links, &ids[0]).needs_repair);
    }

    #[tokio::test]
    async fn missing_secret_counts_as_needs_repair() {
        let (_d, mem, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        mem.map.lock().unwrap().remove(&ids[1]);
        let seen = Arc::new(Mutex::new(vec![]));
        let ask = recording_ask(seen.clone(), |_| Err(TvError::NothingPlaying));
        let out = go(links.clone(), None, ask).await;
        assert_eq!(out, Err(NowError::NeedsRepair { label: "B".into() }));
        assert!(row_of(&links, &ids[1]).needs_repair);
        // B has no secret, so only A reached the network.
        assert_eq!(*seen.lock().unwrap(), ["screen-a"]);
    }

    #[tokio::test]
    async fn keychain_failure_is_a_network_result_not_a_mark() {
        let (_d, mem, links) = setup();
        let ids = link_all(&links, &["A"]);
        mem.fail_get.store(true, Ordering::SeqCst);
        let seen = Arc::new(Mutex::new(vec![]));
        let out = go(links.clone(), None, recording_ask(seen.clone(), |_| Ok((sighting("v"), None)))).await;
        assert_eq!(out, Err(NowError::Network));
        assert!(seen.lock().unwrap().is_empty());
        assert!(!row_of(&links, &ids[0]).needs_repair);
    }

    #[tokio::test]
    async fn unpaired_mid_press_is_skipped() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        let (l, b) = (links.clone(), ids[1].clone());
        // A has nothing, so B is asked; B is unpaired after its secret was read
        // and before the mark, then answers Unlinked.
        let ask = move |screen_id: String, _budget: Duration| {
            if screen_id == "screen-b" {
                l.unpair(&b).unwrap();
                return std::future::ready(Err(TvError::Unlinked));
            }
            std::future::ready(Err(TvError::NothingPlaying))
        };
        let out = go(links.clone(), None, ask).await;
        assert_eq!(out, Err(NowError::NeedsRepair { label: "B".into() }), "the outcome is still produced");
        let left = links.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].tv_id, ids[0]);
    }

    #[tokio::test]
    async fn answering_tv_is_marked_online_with_its_model() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A"]);
        links.mark(&ids[0], TvHealth::NeedsRepair, None).unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let out = go(links.clone(), None, recording_ask(seen, |_| Ok((sighting("vid"), Some("Hisense HiSmartTV A4".into()))))).await.unwrap();
        assert!(matches!(out, Outcome::Found { via_default: true, .. }));
        let row = row_of(&links, &ids[0]);
        assert!(!row.needs_repair);
        assert_eq!(row.last_status, Some(TvHealth::Online));
        assert_eq!(row.model.as_deref(), Some("Hisense HiSmartTV A4"));
    }

    /// R26c: a TV that answered "nothing playing" is reachable and linked.
    #[tokio::test]
    async fn an_idle_tv_is_marked_online_and_a_network_failure_is_not() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        links.mark(&ids[0], TvHealth::NeedsRepair, Some("Hisense HiSmartTV A4")).unwrap();
        let ask = recording_ask(Arc::default(), |s| match s {
            "screen-a" => Err(TvError::NothingPlaying),
            _ => Err(TvError::Network("timed out".into())),
        });
        let _ = go(links.clone(), None, ask).await;
        let a = row_of(&links, &ids[0]);
        assert_eq!(a.last_status, Some(TvHealth::Online));
        assert!(!a.needs_repair);
        assert_eq!(a.model.as_deref(), Some("Hisense HiSmartTV A4"), "no answer, so no new model — and the old one stays");
        let b = row_of(&links, &ids[1]);
        assert_eq!(b.last_status, None, "a failed ask says nothing about the TV");
    }

    #[tokio::test]
    async fn explicit_tv_id_asks_only_that_tv() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B", "C"]); // A is the default
        let seen = Arc::new(Mutex::new(vec![]));
        let out = go(links.clone(), Some(ids[1].clone()), recording_ask(seen.clone(), |_| Ok((sighting("vid-b"), None)))).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["screen-b"]);
        match out {
            Outcome::Found { tv_id, via_default, label, .. } => {
                assert_eq!(tv_id, ids[1]);
                assert_eq!(label, "B");
                assert!(!via_default, "R3: an explicit TV is labelled by name");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_tv_id_and_empty_list_are_no_tvs() {
        let (_d, _m, links) = setup();
        let never = |_: String, _: Duration| -> std::future::Ready<Answer> { panic!("asked the network") };
        assert_eq!(go(links.clone(), None, never).await, Err(NowError::NoTvs));
        link_all(&links, &["A"]);
        assert_eq!(go(links.clone(), Some("nope".into()), never).await, Err(NowError::NoTvs));
    }

    #[tokio::test]
    async fn default_is_asked_first_whatever_the_link_order() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B", "C"]);
        links.set_default(&ids[2]).unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        // A long head start: this test is about who is asked first, not about the head start.
        let ask = recording_ask(seen.clone(), |_| Ok((sighting("v"), None)));
        let out = press(links.clone(), None, CancellationToken::new(), OVERALL, LONG_HEAD, ask).await.unwrap();
        assert_eq!(*seen.lock().unwrap(), ["screen-c"]);
        assert!(matches!(out, Outcome::Found { via_default: true, ref tv_id, .. } if *tv_id == ids[2]));
    }

    #[tokio::test]
    async fn default_empty_then_others_are_picked_from() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        let seen = Arc::new(Mutex::new(vec![]));
        let ask = recording_ask(seen, |s| if s == "screen-a" { Err(TvError::NothingPlaying) } else { Ok((sighting("vid-b"), None)) });
        let out = go(links.clone(), None, ask).await.unwrap();
        assert!(matches!(out, Outcome::Found { via_default: false, ref tv_id, .. } if *tv_id == ids[1]));
    }

    #[tokio::test]
    async fn slow_default_that_answers_ok_wins_and_the_others_are_dropped() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let ask = move |screen_id: String, _budget: Duration| {
            s2.lock().unwrap().push(screen_id.clone());
            async move {
                if screen_id == "screen-a" {
                    // Well past the head start (100 ms), so B is asked meanwhile.
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    Ok((sighting("vid-a"), None))
                } else {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    Ok((sighting("vid-b"), None))
                }
            }
        };
        let started = Instant::now();
        let out = go(links.clone(), None, ask).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "waited for the others: {:?}", started.elapsed());
        assert_eq!(*seen.lock().unwrap(), ["screen-a", "screen-b"], "B was started after the head start");
        assert!(matches!(out, Outcome::Found { via_default: true, ref tv_id, .. } if *tv_id == ids[0]));
    }

    // ── cancel (R15) ──

    #[tokio::test]
    async fn a_cancelled_default_asks_no_other_tv_and_reads_no_other_secret() {
        let (_d, mem, links) = setup();
        link_all(&links, &["A", "B", "C"]);
        let cancel = CancellationToken::new();
        let (seen, c2) = (Arc::new(Mutex::new(Vec::<String>::new())), cancel.clone());
        let s2 = seen.clone();
        let ask = move |screen_id: String, _budget: Duration| {
            s2.lock().unwrap().push(screen_id);
            c2.cancel();
            std::future::ready(Err(TvError::Cancelled))
        };
        let out = press(links.clone(), None, cancel, OVERALL, LONG_HEAD, ask).await;
        assert_eq!(out, Err(NowError::Cancelled));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(*seen.lock().unwrap(), ["screen-a"]);
        assert_eq!(mem.gets.load(Ordering::SeqCst), 1, "only the default's secret was read");
    }

    #[tokio::test]
    async fn a_pre_cancelled_press_reads_no_keychain() {
        let (_d, mem, links) = setup();
        link_all(&links, &["A", "B"]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let never = |_: String, _: Duration| -> std::future::Ready<Answer> { panic!("asked the network") };
        let out = press(links.clone(), None, cancel, OVERALL, HEAD, never).await;
        assert_eq!(out, Err(NowError::Cancelled));
        // A read that slipped past the pre-check would run on a blocking thread: give it time to count.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(mem.gets.load(Ordering::SeqCst), 0);
    }

    /// R26a: the press is findable by `cancel_extraction` from the first
    /// moment, even while the TV list is still being built.
    #[tokio::test]
    async fn a_cancel_that_lands_while_the_tv_list_is_loading_stops_the_press() {
        let (_d, mem, links) = setup();
        link_all(&links, &["A", "B"]);
        let requests: Arc<Mutex<HashMap<String, CancellationToken>>> = Arc::default();
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let (r2, l2) = (requests.clone(), links.clone());
        let call = tokio::spawn(async move {
            now_playing_command(
                "req-1".into(),
                None,
                &r2,
                async move {
                    gate.await.ok();
                    Ok(l2)
                },
                OVERALL,
                HEAD,
                |_device_id, _cancel| |_: String, _: Duration| -> std::future::Ready<Answer> { panic!("asked the network") },
            )
            .await
        });
        // The call is parked on the TV list; the request must already be registered.
        let mut found = None;
        for _ in 0..200 {
            found = requests.lock().unwrap().get("req-1").cloned();
            if found.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        found.expect("the request was not registered before the TV list resolved").cancel();
        release.send(()).unwrap();
        assert_eq!(call.await.unwrap(), Err(NowError::Cancelled));
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(mem.gets.load(Ordering::SeqCst), 0, "a cancelled press reads no keychain");
        assert!(requests.lock().unwrap().is_empty(), "the guard removed the request");
    }

    #[tokio::test]
    async fn cancel_wins_over_a_slow_keychain_read() {
        let (_d, mem, links) = setup();
        link_all(&links, &["A"]);
        mem.get_delay_ms.store(1500, Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c2.cancel();
        });
        let never = |_: String, _: Duration| -> std::future::Ready<Answer> { panic!("asked the network") };
        let started = Instant::now();
        let out = press(links.clone(), None, cancel, OVERALL, HEAD, never).await;
        assert_eq!(out, Err(NowError::Cancelled));
        assert!(started.elapsed() < Duration::from_millis(700), "waited for the keychain: {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn the_deadline_wins_over_a_slow_keychain_read() {
        let (_d, mem, links) = setup();
        link_all(&links, &["A"]);
        mem.get_delay_ms.store(1500, Ordering::SeqCst);
        let never = |_: String, _: Duration| -> std::future::Ready<Answer> { panic!("asked the network") };
        let started = Instant::now();
        let out = press(links.clone(), None, CancellationToken::new(), Duration::from_millis(200), HEAD, never).await;
        assert_eq!(out, Err(NowError::Network));
        assert!(started.elapsed() < Duration::from_millis(900), "{:?}", started.elapsed());
    }

    // ── link / repair / refresh ──

    fn paired(screen: &'static str) -> impl FnOnce() -> std::future::Ready<Result<Pairing, PairError>> {
        move || std::future::ready(Ok(pairing(screen)))
    }
    fn checked(model: Option<&'static str>) -> impl FnOnce(String, String) -> std::future::Ready<Result<Option<String>, TvError>> {
        move |_s, _d| std::future::ready(Ok(model.map(str::to_string)))
    }

    #[tokio::test]
    async fn link_maps_pair_errors() {
        let (_d, _m, links) = setup();
        let bad = link_with(links.clone(), || std::future::ready(Err(PairError::BadCode)), checked(None)).await;
        assert_eq!(bad, Err(TvCmdError::BadCode));
        let net = link_with(links.clone(), || std::future::ready(Err(PairError::Network("HTTP 500".into()))), checked(None)).await;
        assert_eq!(net, Err(TvCmdError::Network));
        assert!(links.list().is_empty());
    }

    #[tokio::test]
    async fn link_takes_the_model_and_survives_a_failed_check() {
        let (_d, _m, links) = setup();
        let ok = link_with(links.clone(), paired("screen-a"), checked(Some("Hisense"))).await.unwrap();
        assert!(!ok.already_linked);
        assert_eq!(ok.row.model.as_deref(), Some("Hisense"));

        let failed_check = |_s: String, _d: String| std::future::ready(Err(TvError::Network("x".into())));
        let second = link_with(links.clone(), paired("screen-b"), failed_check).await.unwrap();
        assert_eq!(second.row.model, None);

        let again = link_with(links.clone(), paired("screen-a"), checked(None)).await.unwrap();
        assert!(again.already_linked);
        assert_eq!(links.list().len(), 2);
    }

    #[tokio::test]
    async fn link_past_the_limit_is_too_many() {
        let (_d, _m, links) = setup();
        link_all(&links, &["A", "B", "C", "D", "E"]);
        let out = link_with(links.clone(), paired("screen-new"), checked(None)).await;
        assert_eq!(out, Err(TvCmdError::TooMany));
    }

    #[tokio::test]
    async fn repair_maps_belongs_to_other_and_not_found() {
        let (_d, _m, links) = setup();
        let ids = link_all(&links, &["A", "B"]);
        let out = repair_with(links.clone(), ids[1].clone(), paired("screen-a"), checked(None)).await;
        assert_eq!(out, Err(TvCmdError::BelongsToOther { label: "A".into() }));
        let gone = repair_with(links.clone(), "nope".into(), paired("screen-z"), checked(None)).await;
        assert_eq!(gone, Err(TvCmdError::NotFound));

        links.mark(&ids[1], TvHealth::NeedsRepair, None).unwrap();
        let fixed = repair_with(links.clone(), ids[1].clone(), paired("screen-b2"), checked(Some("Sony"))).await.unwrap();
        assert!(!fixed.needs_repair);
        assert_eq!(fixed.model.as_deref(), Some("Sony"));
        assert_eq!(fixed.label, "B");
    }

    #[tokio::test]
    async fn repair_does_not_spend_a_code_on_an_unknown_tv() {
        let (_d, _m, links) = setup();
        let pair = || -> std::future::Ready<Result<Pairing, PairError>> { panic!("paired") };
        assert_eq!(repair_with(links, "nope".into(), pair, checked(None)).await, Err(TvCmdError::NotFound));
    }

    #[tokio::test]
    async fn refresh_maps_check_to_health() {
        let (_d, mem, links) = setup();
        let ids = link_all(&links, &["A"]);
        let online = refresh_with(links.clone(), ids[0].clone(), checked(Some("Hisense"))).await.unwrap();
        assert_eq!(online.last_status, Some(TvHealth::Online));
        assert_eq!(online.model.as_deref(), Some("Hisense"));

        let unlinked = refresh_with(links.clone(), ids[0].clone(), |_s, _d| std::future::ready(Err(TvError::Unlinked))).await.unwrap();
        assert!(unlinked.needs_repair);
        assert_eq!(unlinked.last_status, Some(TvHealth::NeedsRepair));

        let down = refresh_with(links.clone(), ids[0].clone(), |_s, _d| std::future::ready(Err(TvError::Network("x".into())))).await.unwrap();
        assert_eq!(down.last_status, Some(TvHealth::Unreachable));
        assert!(down.needs_repair, "Unreachable leaves the flag alone (R2)");

        mem.map.lock().unwrap().clear();
        let never = |_s: String, _d: String| -> std::future::Ready<Result<Option<String>, TvError>> { panic!("asked the network") };
        let missing = refresh_with(links.clone(), ids[0].clone(), never).await.unwrap();
        assert_eq!(missing.last_status, Some(TvHealth::NeedsRepair));

        assert_eq!(refresh_with(links, "nope".into(), checked(None)).await, Err(TvCmdError::NotFound));
    }

    // ── the JSON the frontend relies on ──

    #[test]
    fn tv_cmd_error_shapes() {
        use serde_json::json;
        assert_eq!(serde_json::to_value(TvCmdError::BadCode).unwrap(), json!({"code": "bad_code"}));
        assert_eq!(serde_json::to_value(TvCmdError::TooMany).unwrap(), json!({"code": "too_many"}));
        assert_eq!(serde_json::to_value(TvCmdError::NotFound).unwrap(), json!({"code": "not_found"}));
        assert_eq!(serde_json::to_value(TvCmdError::Network).unwrap(), json!({"code": "network"}));
        assert_eq!(serde_json::to_value(TvCmdError::Store).unwrap(), json!({"code": "store"}));
        assert_eq!(
            serde_json::to_value(TvCmdError::BelongsToOther { label: "Bedroom".into() }).unwrap(),
            json!({"code": "belongs_to_other", "label": "Bedroom"})
        );
    }

    #[test]
    fn link_result_shape() {
        use serde_json::json;
        let result = LinkResult {
            row: TvRow {
                tv_id: "t1".into(),
                label: "Lounge".into(),
                model: Some("Hisense".into()),
                is_default: true,
                needs_repair: false,
                last_status: Some(TvHealth::Online),
            },
            already_linked: true,
        };
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!({"row": {"tv_id": "t1", "label": "Lounge", "model": "Hisense", "is_default": true, "needs_repair": false, "last_status": "online"}, "already_linked": true})
        );
    }

    #[test]
    fn row_and_outcome_never_carry_a_screen_id() {
        let (_d, _m, links) = setup();
        link_all(&links, &["A"]);
        let text = serde_json::to_string(&links.list()).unwrap();
        assert!(!text.contains("screen-a") && !text.contains("screen_id") && !text.contains("hash"), "{text}");
    }
}
