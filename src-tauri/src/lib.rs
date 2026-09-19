pub mod accounts;
pub mod applog;
pub mod app_llm_config;
pub mod ask;
pub mod auth;
pub mod auth_ms;
pub mod backend;
pub mod db;
pub mod db_crypto;
pub mod folder_label;
pub mod folder_scope;
pub mod sync_schedule;
mod reconcile;
mod note_mutations;
mod note_commands;
use note_commands::*;
mod sync_worker;
use reconcile::*;
use note_mutations::*;
use sync_worker::*;
pub mod icloud_auth;
pub mod ingest;
pub mod llm;
pub mod mime822;
pub mod oauth_config;
pub mod paths;
pub mod secrets;
pub mod shell_path;
#[cfg(test)]
mod test_support;

use crate::backend::gmail::wire as gmail;
use accounts::{Account, AccountId, AccountState};
use crate::backend::{Capabilities, Vertical, SidecarKind};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

// Constant-time byte-slice equality. Used to compare the OAuth `state`
// callback parameter against the value we stashed when we built the auth URL,
// without leaking byte positions through timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// Timestamped log: prints `[jodd 2026-08-18T07:14:02.721+07:00] ...` to stderr,
// and — when file logging is enabled (default on, see applog.rs) — appends the
// same line to the persistent log file so it survives past the current process.
//
// **Full RFC 3339, not the wall-clock `HH:MM:SS.mmm` this used to emit.** The
// log file is one appended stream that only rotates at 20 MB (applog.rs), so a
// single file routinely spans months — and with no date, any time-window filter
// silently mixes sessions from different days. That is not hypothetical: while
// diagnosing keychain reads on 2026-08-18, lines from July matched a `07:1x`
// grep for "this launch" and produced a confident, wrong conclusion that two
// copies of the app were running at once.
//
// The `%:z` offset is the other half. `Local::now()` was always local time, but
// without the offset recorded, a DST change or a flight makes two genuinely
// different instants print identically — and this project is developed in
// +07:00 while CI runs in UTC.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let line = format!(
            "[jodd {}] {}",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
            format_args!($($arg)*)
        );
        eprintln!("{}", line);
        $crate::applog::write_line(&line);
    }};
}

pub struct AppState {
    /// One shared zone read per iCloud account.
    ///
    /// CloudKit has no per-folder endpoint, so every read walks the whole zone
    /// — and a `Vertical` is built per operation. Without sharing, the UI's
    /// 2500 ms folder sweep would walk the entire account once per folder: 102
    /// whole-zone reads in four minutes on a real account. Lives here rather
    /// than in the vertical for exactly the reason `AppState` exists at all:
    /// it must outlive the thing that uses it.
    pub icloud_scans: Mutex<HashMap<AccountId, Arc<backend::icloud::AccountCache>>>,
    /// The app itself, so a command can reach a webview.
    ///
    /// Only iCloud needs it, and it needs it structurally rather than as a
    /// convenience: that backend's credential is a live `WKWebView` cookie jar
    /// (gotcha #19), so `vertical_for` cannot build an iCloud transport out of
    /// `AppState` alone the way it builds the other three out of a token. The
    /// alternative — threading an `AppHandle` parameter through `vertical_for`
    /// — would touch every one of its callers to serve one backend.
    pub app_handle: tauri::AppHandle,
    // Persisted list of accounts (loaded from accounts.json on startup).
    pub accounts: Mutex<Vec<Account>>,
    // In-memory per-account state (access tokens, label cache).
    // Populated lazily — entries appear when an account is first used.
    pub account_states: Mutex<HashMap<AccountId, AccountState>>,
    // PKCE verifier for the currently-in-progress Add Account flow.
    // Single-slot because only one OAuth flow can be in progress at a time.
    pub pending_pkce: Mutex<Option<auth::PkcePair>>,
    /// Which provider the in-progress Add Account flow is talking to. Single
    /// slot for the same reason `pending_pkce` is, and set together with it.
    ///
    /// Deliberately NOT persisted alongside the PKCE pair: that persistence
    /// exists so an Android process killed mid-consent can still complete the
    /// exchange, and Android is Gmail-only (Microsoft's flow is loopback, which
    /// is `cfg`-ed out there). A cold-started callback therefore reads the
    /// `Default` — Gmail — which is exactly right on the only platform that can
    /// produce one.
    pub pending_backend: Mutex<accounts::BackendKind>,
    // Local working replica (SQLite). Reads/writes flow through here first;
    // sync layer reconciles with Gmail. None during startup before DB is
    // opened — should always be Some by the time any command runs.
    pub db: Arc<db::Db>,
    // UUIDs whose sync worker push is currently in flight to Gmail. Used by
    // reconcile_one to suppress false conflict detection: during the ~1-2s
    // window between gmail::save_note creating a new message id and our
    // mark_pushed updating the cache, a concurrent poll would see "remote
    // changed" (new id) while the cache still has the old remote_version —
    // and would falsely flag a conflict on our own push. Entries are scoped
    // by (account_id, uuid) since the same uuid CAN legitimately exist in
    // two accounts.
    pub pushing: Mutex<std::collections::HashSet<(String, String)>>,
    // Latest observed duplicate-message summary per account, written by
    // list_notes after each pass. The frontend reads this via get_dup_stats
    // to show a passive "N duplicate(s)" pill in the sidebar so the user
    // has a signal when cleanup_orphans is worth running. Replace semantics
    // (not accumulate): each list_notes call is a complete observation.
    pub dup_stats: Mutex<HashMap<AccountId, gmail::DedupSummary>>,
    // CancellationTokens for in-flight extract_note calls, keyed by the
    // request_id that extract_note returns to the frontend at call start.
    // Frontend triggers abort by invoking cancel_extraction(request_id), which
    // cancels the token; extract_note's tokio::select! sees that and the
    // provider unwinds (HTTP: drops the in-flight reqwest future; Claude CLI:
    // kills the child process). Entries clean themselves up via a guard in
    // extract_note so a normal completion or error path doesn't leak.
    pub in_flight_extracts: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    /// Cancellation tokens for in-flight Ask Jodd turns, keyed by request_id.
    /// Mirrors in_flight_extracts exactly — both LLM calls in a turn observe
    /// the same token, so cancelling during selection never reaches the
    /// answer call.
    pub in_flight_asks: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    pub ai_policy: llm::policy::Runtime,
    /// Cancels the loopback listener of an in-flight sign-in. A single slot,
    /// because only one OAuth flow can be in progress — and because the second
    /// attempt has to be able to reclaim port 8080 from an abandoned first one.
    pub oauth_cancel: Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// Shared admission for periodic/flush worker rounds and immediate LocalFS writes.
    pub sync_schedule: sync_schedule::Scheduler,

    /// When each account last ran the incremental change detector.
    ///
    /// The worker ticks every 5 s, which is the right cadence for draining a
    /// local queue and far too fast for a request to somebody's private API.
    /// See `PULL_INTERVAL`.
    pub last_pull: Mutex<HashMap<String, std::time::Instant>>,
}

const LABEL_MAP_TTL: std::time::Duration = std::time::Duration::from_secs(300);

// ─── Account helpers ─────────────────────────────────────────────────────────

// Computes the deadline for a freshly-issued access token. We subtract a safety
// margin so refresh fires BEFORE the actual expiry — covers clock skew and the
// time it takes the refresh round-trip to complete.
fn token_deadline_from_expires_in(expires_in: Option<i64>) -> std::time::SystemTime {
    let secs = expires_in.unwrap_or(3600).max(60) as u64;
    let safety_margin = 60u64.min(secs / 2);
    std::time::SystemTime::now() + std::time::Duration::from_secs(secs - safety_margin)
}

// Recognize errors from gmail.rs that indicate the bearer token Google
// received is no longer valid. The retry layer uses this to decide whether
// a force-refresh + retry will help; non-auth errors (network, 5xx, etc.)
// pass straight through unchanged.
fn is_unauthorized_error(err: &str) -> bool {
    err.contains(" 401") || err.contains("UNAUTHENTICATED") || err.contains("Invalid Credentials")
}

/// The whole of the inactive gate, as a free function so it can be tested
/// without a Tauri State.
///
/// Refuses `Inactive` only. `Draining` is deliberately admitted: it is the
/// state in which the worker is still flushing the outbound queue, and a
/// blanket refusal here would deadlock every deactivation at Draining.
fn refuse_if_inactive(account_id: &str, status: accounts::AccountStatus) -> Result<(), String> {
    if status == accounts::AccountStatus::Inactive {
        return Err(format!("account {account_id} is inactive"));
    }
    Ok(())
}

/// Whether an account, found to have `queue_empty` outbound queues, should
/// flip to `Inactive` right now. Isolated as a free function so the
/// end-of-tick flip has one source of truth to re-check against under the
/// lock, instead of re-deriving the condition inline where it could drift.
///
/// Only `(Draining, true)` admits. In particular `Active` must refuse even
/// when `queue_empty` is true: the tick snapshots which accounts are
/// Draining, then does the (possibly slow) `has_pending_pushes` check, then
/// re-locks to write — a reactivate (`Inactive -> Active`, Task 5) can land
/// in that window, and flipping unconditionally on stale status would
/// clobber the user's freshly-Active account back to Inactive.
fn should_flip_to_inactive(current: accounts::AccountStatus, queue_empty: bool) -> bool {
    current == accounts::AccountStatus::Draining && queue_empty
}

/// May this account be removed right now — i.e. is it safe to run
/// `perform_account_removal` against it immediately?
///
/// `perform_account_removal` deletes the refresh token before anything else,
/// so a draining account would lose its credential while still pushing.
/// `remove_account` no longer refuses that case outright: it sets
/// `pending_removal` and queues the removal for `sync_worker_tick` to finish
/// once the queue genuinely empties (roadmap #0d). The UI's "Stop waiting"
/// button is still the instant, cost-naming alternative for a user who
/// doesn't want to wait even that long — it force-flips to `Inactive`, after
/// which removal is safe because `Inactive` is unconditionally admitted here.
fn removal_allowed(status: accounts::AccountStatus) -> bool {
    status != accounts::AccountStatus::Draining
}

/// Sets `pending_removal` on the account with this id, if it exists. Pure
/// data mutation, no I/O — so the gentler branch `remove_account` takes for a
/// `Draining` account (queue instead of refuse) is testable without a live
/// `State`. Returns whether the account was found.
fn queue_removal(accounts: &mut [accounts::Account], account_id: &str) -> bool {
    match accounts.iter_mut().find(|a| a.id == account_id) {
        Some(a) => {
            a.pending_removal = true;
            true
        }
        None => false,
    }
}

/// Whether an account whose `pending_removal` flag was set while it was
/// `Draining` should have its removal actually performed now.
///
/// Mirrors `should_flip_to_inactive`'s shape: isolated so the worker's
/// end-of-tick completion loop has one source of truth instead of an inline
/// condition. Gated on `Inactive` alone, not on a fresh `has_pending_pushes`
/// read of its own — `Inactive` already means "queue confirmed empty"
/// (gotcha #2), however the account got there. That includes the "Stop
/// waiting" force-flip: once a user has forced `Inactive`, `removal_allowed`
/// admits it unconditionally too, so completing a pending removal at the same
/// gate is consistent, not a second, looser check.
fn should_complete_pending_removal(status: accounts::AccountStatus, pending_removal: bool) -> bool {
    status == accounts::AccountStatus::Inactive && pending_removal
}

/// The state machine, as data.
///
/// `Active -> Inactive` is absent deliberately: it would skip the drain and
/// silently strand every queued edit. The only way to reach Inactive with work
/// outstanding is Draining -> Inactive, which the UI surfaces as "Stop
/// waiting" and which states what it costs.
fn transition_allowed(from: accounts::AccountStatus, to: accounts::AccountStatus) -> bool {
    use accounts::AccountStatus::*;
    if from == to {
        return true;
    }
    matches!((from, to), (Active, Draining) | (Draining, Inactive) | (Inactive, Active))
}

/// Current lifecycle state of an account, or None if it no longer exists.
fn account_status_of(state: &State<'_, AppState>, account_id: &str) -> Option<accounts::AccountStatus> {
    state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.status)
}

/// The shared zone-read cache for one iCloud account, created on first use.
///
/// Keyed by account so two Apple IDs could never share a scan — a limit that
/// does not exist today (one account per install) but that this map should not
/// be the thing preventing.
fn icloud_scan_cache(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Arc<backend::icloud::AccountCache> {
    state
        .icloud_scans
        .lock()
        .unwrap()
        .entry(account_id.to_string())
        .or_default()
        .clone()
}

/// Which backend an account belongs to, or None if it no longer exists.
fn account_backend_kind(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Option<accounts::BackendKind> {
    state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind)
}

/// Accounts the user has dismissed — Draining as well as Inactive. Both are
/// gone from the user's view the moment the button is pressed; a note that
/// kept appearing in search for another thirty seconds would read as a bug.
fn hidden_account_ids(state: &State<'_, AppState>) -> Vec<String> {
    state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| !a.is_active())
        .map(|a| a.id.clone())
        .collect()
}

/// Construct the backend vertical via dynamic dispatch. Dispatches on the
/// account's `backend_kind`:
///
/// - **Gmail** — fetch a token + label_map, return a `GmailVertical`.
/// - **LocalFs** — resolve `root_dir`, return a `LocalFsVertical`. No token.
/// - **Microsoft** — fetch a token, return a `MicrosoftVertical`. No label map:
///   Exchange has no label concept, and its folders are not enumerable up front
///   (gotcha #12) — the vertical derives them from a message scan instead, so
///   there is nothing here to cache alongside the token.
///
/// All call sites are independent of the concrete type.
async fn vertical_for(state: &State<'_, AppState>, account_id: &str) -> Result<Box<dyn Vertical>, String> {
    let (kind, root_dir, meta_label, status) = {
        let list = state.accounts.lock().unwrap();
        let a = list.iter().find(|a| a.id == account_id)
            .ok_or_else(|| format!("account {} not found", account_id))?;
        (a.backend_kind, a.root_dir.clone(), a.effective_meta_label().to_string(), a.status)
    };
    refuse_if_inactive(account_id, status)?;
    match kind {
        accounts::BackendKind::LocalFs => {
            let root = root_dir.ok_or_else(|| format!("local account {} missing root_dir", account_id))?;
            Ok(Box::new(backend::localfs::LocalFsVertical::new(std::path::PathBuf::from(root), account_id.to_string())))
        }
        accounts::BackendKind::Gmail => {
            let token = ensure_token(state, account_id).await?;
            let label_map = cached_label_map(state, account_id, &token).await?;
            Ok(Box::new(backend::gmail::GmailVertical::new(token, label_map, account_id.to_string(), meta_label)))
        }
        accounts::BackendKind::Microsoft => {
            // No label map: Exchange has no label concept at all, and its
            // folders cannot be enumerated up front (gotcha #12) — the vertical
            // derives them from a message scan instead, so there is nothing to
            // fetch or cache here beyond the bearer token.
            //
            // What it DOES get is the path→folder-id map from the local
            // `folders` table, where a previous full scan stored the Exchange
            // folder id in `label_id`. Without it every scoped folder read
            // would have to paginate the whole mailbox, and the UI sweeps one
            // folder every 2500 ms — see `wire::folder_read_plan`. Reading
            // SQLite, not the network, so this stays local-first.
            let token = ensure_token(state, account_id).await?;
            let folder_ids = microsoft_folder_ids(state, account_id);
            Ok(Box::new(backend::microsoft::MicrosoftVertical::new(
                token,
                account_id.to_string(),
                folder_ids,
            )))
        }
        accounts::BackendKind::ICloud => {
            #[cfg(not(icloud_webview))]
            {
                // iOS only, as of 2026-09-10 — every other target now has a
                // cookie source (`icloud_auth::cookie_source`), so this is no
                // longer a desktop-vs-mobile line. See `gate_the_icloud_webview`
                // in build.rs for what excludes iOS specifically.
                return Err(format!(
                    "iCloud accounts are not available on this platform (account {account_id})"
                ));
            }
            #[cfg(icloud_webview)]
            {
                Ok(Box::new(icloud_vertical_for(state, account_id).await?))
            }
        }
    }
}

/// The iCloud vertical, unboxed.
///
/// Split out of `vertical_for` so a caller that needs something this backend
/// has and the `Vertical` trait does not — the write census — can reach it
/// without widening a trait three other backends would then carry a stub for.
/// One constructor, two callers: `vertical_for` boxes it, `icloud_census` uses
/// it directly.
#[cfg(icloud_webview)]
async fn icloud_vertical_for(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Result<backend::icloud::ICloudVertical, String> {
    // **No token, and nothing read from disk.** This backend's credential is a
    // live cookie jar the app never copies (gotcha #19), so the session is
    // re-bootstrapped through `/validate` on every construction rather than
    // persisted — which is also the only way to learn the `p<N>-ckdatabasews`
    // partition host, since it cannot be guessed and differs per Apple ID.
    //
    // `LiveSession` resolves the webview per harvest and opens the hidden one
    // if the visible sign-in window is gone (B4), so a vertical built hours
    // after sign-in works without asking the user for anything.
    let cookies = icloud_auth::cookie_source(&state.app_handle);
    // Through the cache, not `establish` directly: a vertical is built per
    // operation, so the folder sweep would otherwise `POST /validate` every
    // 2500 ms — about a hundred calls during one sweep, on top of the zone
    // reads the same cache already collapses.
    let cache = icloud_scan_cache(state, account_id);
    let session = cache
        .session(cookies.as_ref())
        .await
        .map_err(|e| format!("iCloud session unavailable: {e}"))?;
    // Minted lazily, on the first vertical built for this account after this
    // change — `ensure_icloud_replica_id` mints once and is stable forever
    // after (Task 1 of the M2.5 plan). `None` only if the account vanished
    // from the list between `vertical_for`'s own lookup and here, which
    // cannot happen on this call path (the account row is what got us here).
    let replica_id = ensure_icloud_replica_id(state, account_id)
        .ok_or_else(|| format!("{account_id} not found"))?;
    Ok(backend::icloud::ICloudVertical::new(
        session,
        cookies,
        account_id.to_string(),
        cache,
        replica_id,
    ))
}

/// Jodd folder path → Exchange folder id, read out of the local `folders`
/// table.
///
/// `folders.label_id` holds the Gmail label id on a Gmail account and the
/// Exchange folder id on a Microsoft one (see `FolderSource` and the Task 11
/// regression test that pins the contrast). Rows with no id — a folder that has
/// never been seen remotely — are omitted rather than mapped to an empty
/// string, so `folder_read_plan` sees a clean miss.
///
/// A DB error yields an empty map, not a failure: an empty map only costs the
/// vertical its scoped-read optimisation (it falls back to one cached mailbox
/// scan), whereas failing here would make the whole account unreadable.
fn microsoft_folder_ids(
    state: &State<'_, AppState>,
    account_id: &str,
) -> std::collections::HashMap<String, String> {
    match state.db.list_folders(account_id) {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|f| {
                f.label_id
                    .filter(|id| !id.trim().is_empty())
                    .map(|id| (f.path, id))
            })
            .collect(),
        Err(e) => {
            log!("microsoft_folder_ids: reading cached folders failed: {} — scoped folder reads will fall back to a mailbox scan", e);
            std::collections::HashMap::new()
        }
    }
}

/// Build the vertical from already-fetched Gmail parts (token + label_map),
/// resolving meta_label internally. For call sites that already hold token +
/// label_map for other use and shouldn't re-fetch. Returns Box<dyn Vertical>
/// so all dispatch is uniform.
fn vertical_from_parts(
    state: &State<'_, AppState>,
    account_id: &str,
    token: String,
    label_map: std::collections::HashMap<String, String>,
) -> Result<Box<dyn Vertical>, String> {
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    Ok(Box::new(backend::gmail::GmailVertical::new(token, label_map, account_id.to_string(), meta_label)))
}

// Ensures the AccountState for account_id has a valid access_token, refreshing
// from the keychain-stored refresh token if expired or missing.
//
// `force_refresh=true` skips the fast path — used by the 401-retry wrapper to
// recover from a token Google has invalidated for reasons other than expiry
// (revoke, password change, scope change). The fast-path freshness check is
// wall-clock-based, so it already correctly invalidates after laptop sleep.
async fn ensure_token(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Result<String, String> {
    ensure_token_inner(state, account_id, false).await
}

async fn ensure_token_inner(
    state: &State<'_, AppState>,
    account_id: &str,
    force_refresh: bool,
) -> Result<String, String> {
    // Fast path: in-memory token, still fresh. Skipped on force_refresh.
    if !force_refresh {
        let states = state.account_states.lock().unwrap();
        if let Some(s) = states.get(account_id) {
            if let (Some(t), Some(exp)) = (s.access_token.as_ref(), s.token_expires_at) {
                if exp > std::time::SystemTime::now() {
                    return Ok(t.clone());
                }
                log!("ensure_token: {} access token expired, refreshing", account_id);
            } else if s.access_token.is_some() {
                // Have a token but no expiry tracked (e.g. from legacy migration).
                // Treat as unknown freshness — refresh defensively.
                log!("ensure_token: {} has token but no expiry — refreshing", account_id);
            }
        }
    } else {
        log!("ensure_token: {} force-refresh requested (401 recovery)", account_id);
    }

    // Slow path: refresh from keychain, against whichever identity provider
    // issued the token. Google wants `client_id` + (usually) `client_secret`;
    // Microsoft is a public client that has no secret to give — sending one at
    // all is a registration error there. Both go through
    // `auth::refresh_access_token_at`, so a failure produces the same
    // `refresh failed: {status} — {body}` string either way and
    // `is_unauthorized_error` keeps classifying it identically.
    let rt = accounts::load_refresh_token(account_id)
        .ok_or_else(|| format!("no refresh token in keychain for {}", account_id))?;
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    let token_data = match backend_kind {
        accounts::BackendKind::Microsoft => auth_ms::refresh_access_token(&rt).await?,
        // LocalFs never reaches here (it has no token), but a keychain entry
        // for one would be a Google credential by construction.
        _ => auth::refresh_access_token(&rt).await?,
    };
    let access = token_data.access_token.clone();
    let deadline = token_deadline_from_expires_in(token_data.expires_in);

    {
        let mut states = state.account_states.lock().unwrap();
        let entry = states.entry(account_id.to_string()).or_default();
        entry.access_token = Some(access.clone());
        entry.token_expires_at = Some(deadline);
    }
    if let Some(new_rt) = token_data.refresh_token {
        let _ = accounts::save_refresh_token(account_id, &new_rt);
    }
    Ok(access)
}

// Read the label_map for this account from cache; otherwise fetch + update cache.
//
// Concurrency: uses a per-account async refresh lock to coalesce simultaneous
// refreshes. Without it, two callers finding the cache stale at the same time
// would both fire gmail::get_label_map and their writes would race — the
// later one clobbering the earlier (with potentially stale data, if Apple
// Notes added/removed a label between the two fetches). With the lock, one
// task fetches and the other awaits its result via the post-lock cache
// re-check (double-check pattern).
async fn cached_label_map(
    state: &State<'_, AppState>,
    account_id: &str,
    token: &str,
) -> Result<HashMap<String, String>, String> {
    // Fast path: cache fresh, no lock needed beyond the brief std::Mutex
    // for the read.
    {
        let states = state.account_states.lock().unwrap();
        if let Some(s) = states.get(account_id) {
            if let Some((map, at)) = s.label_map_cache.as_ref() {
                if at.elapsed() < LABEL_MAP_TTL {
                    return Ok(map.clone());
                }
            }
        }
    }

    // Slow path: cache miss or expired. Acquire the per-account refresh lock
    // so only one task fetches at a time. Clone the Arc out from under the
    // std::Mutex before awaiting — never hold a std::Mutex across an await.
    let refresh_lock = {
        let mut states = state.account_states.lock().unwrap();
        states.entry(account_id.to_string()).or_default().label_map_refresh.clone()
    };
    let _guard = refresh_lock.lock().await;

    // Double-check: another task may have refreshed while we were waiting on
    // the lock. If so, return its result without making a redundant request.
    {
        let states = state.account_states.lock().unwrap();
        if let Some(s) = states.get(account_id) {
            if let Some((map, at)) = s.label_map_cache.as_ref() {
                if at.elapsed() < LABEL_MAP_TTL {
                    return Ok(map.clone());
                }
            }
        }
    }

    // We hold the refresh lock and the cache is still stale. Fetch and cache.
    //
    // 401 self-heal: if Google rejects the bearer (token revoked, clock skew,
    // post-sleep edge case the wall-clock fix doesn't cover), force-refresh
    // the access token from the keychain refresh_token and retry once. Any
    // other failure mode passes straight through.
    let fresh = match gmail::get_label_map(token).await {
        Ok(m) => m,
        Err(e) if is_unauthorized_error(&e) => {
            log!(
                "cached_label_map: {} got 401 from labels.list — forcing token refresh and retrying",
                account_id
            );
            let fresh_token = ensure_token_inner(state, account_id, true).await?;
            gmail::get_label_map(&fresh_token).await?
        }
        Err(e) => return Err(e),
    };
    {
        let mut states = state.account_states.lock().unwrap();
        let entry = states.entry(account_id.to_string()).or_default();
        entry.label_map_cache = Some((fresh.clone(), std::time::Instant::now()));
    }
    Ok(fresh)
}

/// Reconcile the local `folders` cache against a remote label set. Upserts
/// every `Notes` / `Notes/*` label as a clean folder row (the db layer skips
/// rows in pending states), and — when `prune` is set — drops clean rows whose
/// path is no longer present remotely (folder deleted externally).
///
/// Shared by two callers:
///   - the cold-start index pass (`index_account`, upsert-only) so EMPTY
///     folders are visible immediately; pruning is left to list_notes because
///     the cold-start path shouldn't delete on a possibly-partial view, and
///   - the `list_notes` pull (upsert + prune), the authoritative folder sync.
///
/// Before this, the folders cache was populated only by list_notes, which does
/// not run on cold start — so empty labels (e.g. `Notes/play2`) stayed
/// invisible until the user navigated. Folders that contained a note still
/// appeared because the sidebar infers their path from note labels.
fn reconcile_folders_from_labels(
    db: &db::Db,
    account_id: &str,
    label_map: &HashMap<String, String>,
    prune: bool,
) {
    let remote_folder_paths: Vec<String> = label_map
        .iter()
        .filter_map(|(id, name)| {
            if name == "Notes" || name.starts_with("Notes/") {
                Some((id.clone(), name.clone()))
            } else {
                None
            }
        })
        .map(|(id, name)| {
            if let Err(e) = db.upsert_folder_from_remote(account_id, &name, &id) {
                log!("reconcile_folders: upsert failed for '{}': {}", name, e);
            }
            name
        })
        .collect();
    if prune {
        match db.prune_clean_folders(account_id, &remote_folder_paths) {
            Ok(n) if n > 0 => log!(
                "reconcile_folders: pruned {} clean folder row(s) no longer on remote",
                n
            ),
            Ok(_) => {}
            Err(e) => log!("reconcile_folders: prune folders failed: {}", e),
        }
    }
}

/// Reconcile the local `folders` cache from a set of path strings (e.g.
/// from a filesystem scan). Upserts each path as a `clean` folder row via
/// `upsert_folder_from_remote` using an empty label_id string (LocalFs
/// accounts don't have Gmail label ids). When `prune` is set, removes
/// clean cache rows not present in `paths`.
///
/// Mirrors `reconcile_folders_from_labels` but takes folder path strings
/// instead of a Gmail label_map — used by the LocalFs backend paths
/// (`list_notes`, `index_account`) where the filesystem is the source of
/// truth and there is no label_map.
fn reconcile_folders_from_paths(
    db: &db::Db,
    account_id: &str,
    paths: &[String],
    prune: bool,
) {
    for path in paths {
        // For LocalFs, label_id = path (the filesystem path is the stable folder
        // identifier, analogous to Gmail's Label_12345). Passing "" here was the
        // original bug: push_one_folder called v.rename_folder("", new_path) →
        // folder_path("") → notes_dir() → std::fs::rename(Notes/, Notes/Movies/)
        // which fails with EINVAL, silently leaving the disk rename un-applied.
        if let Err(e) = db.upsert_folder_from_remote(account_id, path, path) {
            log!("reconcile_folders_from_paths: upsert failed for '{}': {}", path, e);
        }
    }
    if prune {
        let keep: Vec<String> = paths.to_vec();
        match db.prune_clean_folders(account_id, &keep) {
            Ok(n) if n > 0 => log!(
                "reconcile_folders_from_paths: pruned {} clean folder row(s) not on filesystem",
                n
            ),
            Ok(_) => {}
            Err(e) => log!("reconcile_folders_from_paths: prune folders failed: {}", e),
        }
    }
}

/// Reconcile the local `folders` cache from a backend that reports its own
/// folders, ids included.
///
/// The third sibling of `reconcile_folders_from_labels` (Gmail's label map) and
/// `reconcile_folders_from_paths` (LocalFs's filesystem scan), and the first one
/// where the id is not recoverable from anything else. Gmail can re-derive a
/// label id from the label map at will and LocalFs's id *is* its path, but an
/// Exchange folder id comes only from `parentFolderId` on a message
/// (gotcha #12): `/me/mailFolders` cannot list the Notes tree and
/// `GET /mailFolders/{id}` 404s. Drop the id here and there is no second route
/// to it — which is why `reconcile_folders_from_paths` (which stores the path
/// as the id) is the wrong helper for this backend even though the paths would
/// look right in the sidebar.
///
/// Generic rather than `&dyn Vertical` so a test can supply a bare `Transport`
/// stub; `dyn Vertical` implements its own supertrait, so `&*boxed_vertical`
/// still binds.
///
/// `prune` follows the same rule as the siblings: cold-start index passes leave
/// removal to the authoritative full pull.
async fn reconcile_folders_from_vertical<T: backend::Transport + ?Sized>(
    db: &db::Db,
    account_id: &str,
    vertical: &T,
    prune: bool,
) -> Result<(), String> {
    let folders = vertical.list_folders().await.map_err(|e| e.to_string())?;
    for f in &folders {
        if let Err(e) = db.upsert_folder_from_remote(account_id, &f.path, &f.id) {
            log!("reconcile_folders_from_vertical: upsert failed for '{}': {}", f.path, e);
        }
    }
    if prune {
        let keep: Vec<String> = folders.iter().map(|f| f.path.clone()).collect();
        match db.prune_clean_folders(account_id, &keep) {
            Ok(n) if n > 0 => log!(
                "reconcile_folders_from_vertical: pruned {} clean folder row(s) no longer on remote",
                n
            ),
            Ok(_) => {}
            Err(e) => log!("reconcile_folders_from_vertical: prune folders failed: {}", e),
        }
    }
    Ok(())
}

/// Which of the three folder-reconciliation helpers a backend uses.
///
/// Split out from [`FolderSource`] — which carries the payload each helper needs
/// and can only be built after a network fetch — so that the *choice* is a pure
/// function of the backend kind and therefore testable on its own. That matters
/// because the choice is exactly where this went wrong once: Microsoft used to
/// fall through to the LocalFs arm, whose helper stores the path as the
/// `label_id`. That is correct on LocalFs (a path IS the identifier) and
/// destroys the Exchange folder id, whose only source in the whole API surface
/// is `parentFolderId` on a message (gotcha #12). The result looked right in
/// the sidebar and was unrecoverable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FolderSourceKind {
    /// `reconcile_folders_from_labels`.
    Labels,
    /// `reconcile_folders_from_paths`.
    Paths,
    /// `reconcile_folders_from_vertical`.
    Vertical,
}

/// The only place a backend is mapped to a folder-reconciliation helper. Both
/// `list_notes` and `index_account` dispatch through it, so re-pointing a
/// backend at the wrong helper requires editing this function — which
/// `each_backend_reconciles_from_the_only_source_that_carries_its_ids` guards.
fn folder_source_kind(backend: accounts::BackendKind) -> FolderSourceKind {
    match backend {
        accounts::BackendKind::Gmail => FolderSourceKind::Labels,
        accounts::BackendKind::LocalFs => FolderSourceKind::Paths,
        accounts::BackendKind::Microsoft => FolderSourceKind::Vertical,
        // Same shape as Microsoft for the same reason: folders come out of a
        // record scan the vertical performs, and only it holds the ids. The
        // resemblance stops there — CloudKit reports real nesting.
        accounts::BackendKind::ICloud => FolderSourceKind::Vertical,
    }
}

/// How `list_notes`'s tail reconciles the `folders` table, with the payload the
/// chosen helper needs. Built by the branch that fetched the notes, because only
/// that branch holds the label map / path list the helper consumes.
///
/// Replaces an earlier `(String, Vec<String>, HashMap<..>)` tuple whose leading
/// `"gmail"` / `"localfs"` tag was matched by string comparison and whose
/// `Vec<String>` was silently unused on the Gmail side.
enum FolderSource {
    /// Gmail: the label map is authoritative and carries the label ids.
    Labels(HashMap<String, String>),
    /// LocalFs: the filesystem is authoritative and a path IS the folder id.
    Paths(Vec<String>),
    /// Microsoft: only the vertical can report folders, and only it has the ids.
    Vertical,
}

impl FolderSource {
    /// Which helper this payload is for. Lets a test assert that the branch
    /// which built the payload agrees with `folder_source_kind`'s choice.
    fn kind(&self) -> FolderSourceKind {
        match self {
            FolderSource::Labels(_) => FolderSourceKind::Labels,
            FolderSource::Paths(_) => FolderSourceKind::Paths,
            FolderSource::Vertical => FolderSourceKind::Vertical,
        }
    }
}

// ─── Auth / Add Account ──────────────────────────────────────────────────────

/// Extract `(code, state)` from an OAuth redirect URL. Scheme-agnostic on
/// purpose — only the query string matters, so it has survived all three
/// redirect shapes this project has used (loopback, the retired custom scheme,
/// and the Android App Links https URL). Returns `None` if either parameter is
/// absent —
/// including the consent-denied case, where Google sends `error=` and no
/// code, and the missing-state case, where the CSRF check could not be done.
// Its only production caller is the `#[cfg(target_os = "android")]` deep-link
// handler in `run()`'s `.setup()`, so non-Android builds see this as unused
// outside `#[cfg(test)]`.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn parse_oauth_callback(url: &str) -> Option<(String, String)> {
    let query = url.split('?').nth(1)?;
    let param = |key: &str| -> Option<String> {
        query
            .split('&')
            .find(|p| p.starts_with(&format!("{}=", key)))
            .and_then(|p| p.split_once('='))
            .map(|(_, v)| {
                urlencoding::decode(v)
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|_| v.to_string())
            })
    };
    Some((param("code")?, param("state")?))
}

/// Which OAuth provider an Add Account flow should target.
///
/// `None` — what every existing frontend call site sends, since the parameter
/// did not exist before — means Gmail, so no caller has to change to keep
/// working. Anything unrecognised is refused rather than silently treated as
/// Gmail: a typo'd backend that quietly signs the user into the wrong provider
/// and persists an account under it is far worse than an error string.
///
/// No platform parameter any more. Until 2026-09-09 this took an `android`
/// flag and refused Microsoft there, because that flow was loopback-only and
/// `auth::wait_for_callback_blocking` is `cfg`-ed out on Android (it binds
/// `0.0.0.0:8080`, an unauthenticated server every other app on a phone can
/// reach) — the browser would have opened, the user consented, and nothing
/// ever came back. `auth_ms::redirect_uri` now takes Gmail's App Links URL on
/// Android (gotcha #8), so the platform no longer decides which providers
/// exist.
fn backend_kind_for_signin(backend: Option<&str>) -> Result<accounts::BackendKind, String> {
    match backend {
        None | Some("") | Some("gmail") => Ok(accounts::BackendKind::Gmail),
        // No platform arm any more: Microsoft used to be refused on Android
        // because its only redirect was the loopback listener; it now takes
        // the same App Links URL as Gmail (`auth_ms::redirect_uri`).
        Some("microsoft") => Ok(accounts::BackendKind::Microsoft),
        // LocalFs is deliberately absent: it has no OAuth flow at all
        // (`add_local_account` is its entry point). **ICloud is absent for
        // the same reason and not by oversight** — there is no OAuth on that
        // backend either: no client id, no PKCE pair, no redirect, no token.
        // Sign-in is a real browser session in a webview, so it has its own
        // entry point (`icloud_sign_in`). Routing it through here would need
        // a fake OAuth shape for a flow that shares none of one.
        Some(other) => Err(format!("unknown sign-in backend '{other}'")),
    }
}

/// Refuse a sign-in whose OAuth client id is unset, **before the browser opens**.
///
/// An empty client id puts a bare `client_id=` into the auth URL. Microsoft
/// then renders AADSTS900144 in the browser and never redirects — so the
/// loopback listener waits out its full five-minute timeout while the frontend
/// shows nothing at all. Naming the variable here turns a silent hang into a
/// fixable error.
///
/// Both providers now resolve through compile-time-embedded values with a
/// runtime env override (`auth::embedded_or_runtime`; Google additionally has a
/// config-file tier above both), so **neither is normally empty in a release
/// build** — this fires in a fresh checkout with no `.env`, or in a build whose
/// CI secret was missing. Before 2026-08-17 `MS_CLIENT_ID` had no embedded tier
/// and this check was the entire Microsoft story in a downloaded binary: it
/// always fired.
///
/// The message names `.env` and a restart deliberately, and that is still the
/// correct advice — the runtime tier is read on every call, so nothing has to
/// be rebuilt to supply a client id.
fn refuse_missing_client_id(kind: accounts::BackendKind) -> Result<(), String> {
    let (id, var) = match kind {
        accounts::BackendKind::Microsoft => (auth_ms::client_id(), "MS_CLIENT_ID"),
        accounts::BackendKind::Gmail => (auth::client_id(), "GOOGLE_CLIENT_ID"),
        // No OAuth flow — never reached via `backend_kind_for_signin`.
        // iCloud has no OAuth client of any kind: sign-in is a real webview
        // against Apple's own pages, so there is no id that could be missing.
        accounts::BackendKind::LocalFs | accounts::BackendKind::ICloud => return Ok(()),
    };
    refuse_empty_client_id(&id, var)
}

/// The decision half of [`refuse_missing_client_id`], split out so it is
/// testable without depending on the build environment.
///
/// This is not gratuitous indirection. The test for this refusal used to read
/// the real `auth_ms::client_id()` and skip its assertions when the id happened
/// to be present — safe while `MS_CLIENT_ID` was runtime-only (tests never set
/// it, so the branch always ran), but the moment build.rs began embedding the
/// id from `.env` or a CI secret, the guard went false and the test passed
/// while asserting nothing. Taking the id as an argument keeps the check under
/// test on the machines that actually have credentials, which is every machine
/// that builds a release.
fn refuse_empty_client_id(id: &str, var: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!(
            "{var} is not set, so this build cannot start a sign-in. Add it to .env \
             (see .env.example) and restart Jodd."
        ));
    }
    Ok(())
}

/// The `oauth-error` event's payload.
///
/// Was a bare string until 2026-08-17. It carries a second field now because
/// one failure has an action attached to it — a refusal by a Microsoft tenant
/// that requires admin approval — and a string cannot hand the UI a URL to put
/// behind a Copy button. Every other failure sets `admin_consent_url` to
/// `None` and reads exactly as before.
#[derive(Clone, serde::Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct OauthError {
    message: String,
    admin_consent_url: Option<String>,
}

impl OauthError {
    /// A failure with nothing to offer but its message.
    fn plain(message: impl Into<String>) -> Self {
        Self { message: message.into(), admin_consent_url: None }
    }
}

fn emit_oauth_error(app: &AppHandle, message: impl Into<String>) {
    let _ = app.emit("oauth-error", OauthError::plain(message));
}

/// Turn an authorization server's refusal into something worth showing.
///
/// **The Microsoft branch deliberately does not classify, and that is a
/// measured constraint rather than laziness.** On 2026-08-17 a non-admin user
/// in an outside Microsoft 365 tenant (`jodd@renny.co.th`) was shown "Need
/// admin approval" and took the only route back that page offers. The entire
/// redirect was:
///
/// ```text
/// ?error=access_denied&error_subcode=cancel&state=QljKk8On4aTpcb0O
/// ```
///
/// which is exactly what the same user pressing Cancel produces — no
/// `error_description`, no AADSTS code, nothing that differs. So Jodd cannot
/// know which happened, and the honest move is to name both causes and always
/// offer the admin-consent link rather than guess and be confidently wrong
/// half the time. **Do not "improve" this into a classifier**; see
/// `auth::CallbackDenial` for why the field a classifier would need is
/// absent.
///
/// The prose the user reads lives in the frontend (`SignInBlocked.svelte`),
/// which has the structure for it; what crosses the IPC boundary is this
/// one-sentence summary plus the URL that makes the panel actionable.
#[cfg(not(target_os = "android"))]
fn signin_denial(kind: accounts::BackendKind, denial: &auth::CallbackDenial) -> OauthError {
    let mut message = "Sign-in was not completed.".to_string();
    // Absent on the measured Microsoft refusal, but Google and some Microsoft
    // flows do send one, and it is the only provider-side detail a support
    // conversation has to work with.
    if let Some(d) = &denial.description {
        message.push_str(&format!(" The provider said: {d}"));
    }
    let admin_consent_url = match kind {
        accounts::BackendKind::Microsoft => Some(auth_ms::admin_consent_url()),
        // Google's equivalent is an entirely different mechanism (Workspace
        // app allowlisting, configured in the Admin console, with no per-app
        // URL to hand out), so there is nothing actionable to attach.
        // iCloud has no tenant, no consent screen and no app registration to
        // approve — the user either signs in to Apple or does not.
        accounts::BackendKind::Gmail
        | accounts::BackendKind::LocalFs
        | accounts::BackendKind::ICloud => None,
    };
    OauthError { message, admin_consent_url }
}

#[tauri::command]
async fn get_auth_url(
    backend: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let kind = backend_kind_for_signin(backend.as_deref())?;
    refuse_missing_client_id(kind)?;
    let pair = auth::PkcePair::generate();
    let url = match kind {
        accounts::BackendKind::Microsoft => auth_ms::get_auth_url(&pair),
        _ => auth::get_auth_url(&pair),
    };
    *state.pending_backend.lock().unwrap() = kind;
    // Persist alongside the in-memory slot: on Android the process that
    // handles the redirect may not be the process that called this command
    // (the OS can evict Jodd while the user is still on Google's consent
    // screen, then cold-launch a fresh process from the redirect Intent),
    // and a fresh process's `pending_pkce` is always `None`. See
    // `secrets::save_pending_signin` for the full reasoning. The backend
    // rides along for the same reason the pair does: a fresh process has a
    // DEFAULT `pending_backend`, which is the wrong provider for every flow
    // but Gmail's. Best-effort: log and continue rather than fail the whole
    // flow on a write error — the in-memory slots alone are already
    // sufficient on desktop and on any Android launch that isn't evicted
    // mid-flow.
    let pending = secrets::PendingSignIn { pkce: pair.clone(), backend: kind };
    if let Err(e) = secrets::save_pending_signin(&pending) {
        log!("get_auth_url: failed to persist the pending sign-in: {}", e);
    }
    *state.pending_pkce.lock().unwrap() = Some(pair);
    Ok(url)
}

/// Which PKCE pair — and which provider — a callback completes.
///
/// The in-memory pair and `pending_backend` are set together by
/// `get_auth_url`, so when the pair is present the two agree and win. When
/// it is absent this is a process the redirect Intent cold-started, whose
/// in-memory backend is a DEFAULT that says nothing about the flow the user
/// began; the persisted entry carries both halves for exactly that case.
/// Pure so the cold-start branch is testable without an `AppHandle`.
fn resolve_pending_signin(
    in_mem_pkce: Option<auth::PkcePair>,
    in_mem_backend: accounts::BackendKind,
    persisted: Option<secrets::PendingSignIn>,
) -> Option<(auth::PkcePair, accounts::BackendKind)> {
    match in_mem_pkce {
        Some(pkce) => Some((pkce, in_mem_backend)),
        None => persisted.map(|p| (p.pkce, p.backend)),
    }
}

/// Finish an OAuth flow given the `code` and `state` returned by the
/// authorization server. Shared by the desktop loopback listener and the
/// Android deep-link handler — the trigger differs, the security checks and
/// account persistence must not.
async fn complete_oauth(app: AppHandle, code: String, cb_state: String) {
    log!("complete_oauth: received auth code (len={})", code.len());
    let state = app.state::<AppState>();
    // Both in-memory slots are TAKEN (not peeked), whichever source ends up
    // completing the flow, so an abandoned Microsoft attempt cannot leave
    // the backend set for a later Gmail sign-in — one-shot discipline for
    // the pair and its provider alike.
    let in_mem_pkce = state.pending_pkce.lock().unwrap().take();
    let in_mem_backend = std::mem::take(&mut *state.pending_backend.lock().unwrap());
    // Fall back to the persisted copy when the in-memory slot is empty —
    // this may be a fresh process that cold-launched from the redirect
    // Intent and never ran `get_auth_url` itself (see that function's
    // comment). Whichever source it came from, clear the persisted copy
    // unconditionally right here, before any check below can short-circuit
    // out: every subsequent return path (missing entirely, state mismatch,
    // exchange failure, success) must leave nothing behind for a stray retry
    // to replay. A leftover pair is worse than none.
    let persisted = secrets::load_pending_signin();
    secrets::clear_pending_signin();
    let Some((pkce, backend_kind)) = resolve_pending_signin(in_mem_pkce, in_mem_backend, persisted)
    else {
        log!("complete_oauth: PKCE verifier MISSING");
        emit_oauth_error(&app, "PKCE verifier missing");
        return;
    };
    // OAuth `state` CSRF check (RFC 6749 §10.12). Constant-time compare so a
    // timing oracle can't be used to fish out the expected value byte-by-byte.
    if !constant_time_eq(cb_state.as_bytes(), pkce.state.as_bytes()) {
        log!("complete_oauth: state mismatch — possible CSRF, aborting");
        emit_oauth_error(&app, "OAuth state mismatch — request rejected");
        return;
    }
    let token_data = match backend_kind {
        accounts::BackendKind::Microsoft => auth_ms::exchange_code(&code, &pkce.verifier).await,
        _ => auth::exchange_code(&code, &pkce.verifier).await,
    };
    let token_data = match token_data {
        Ok(td) => td,
        Err(e) => {
            log!("complete_oauth: token exchange FAILED: {}", e);
            emit_oauth_error(&app, e);
            return;
        }
    };
    log!(
        "complete_oauth: token exchange OK (refresh_token present={})",
        token_data.refresh_token.is_some()
    );

    // Look up the user's email so we can persist this account. Accounts are
    // keyed by address on every backend; only the endpoint that reveals it
    // differs (Gmail getProfile vs Graph /me).
    let email = match backend_kind {
        accounts::BackendKind::Microsoft => auth_ms::get_user_email(&token_data.access_token).await,
        _ => gmail::get_user_email(&token_data.access_token).await,
    };
    let email = match email {
        Ok(e) => e,
        Err(e) => {
            log!("complete_oauth: getProfile failed: {}", e);
            emit_oauth_error(&app, format!("get user profile: {}", e));
            return;
        }
    };
    log!("complete_oauth: resolved account email = {}", email);

    // Compute the qualified id once, before the token save, so the
    // credential key and the account record can never disagree.
    let account_id = accounts::account_id_for(backend_kind, &email);

    // Persist refresh token to keychain under per-account key.
    if let Some(rt) = token_data.refresh_token.as_ref() {
        if let Err(e) = accounts::save_refresh_token(&account_id, rt) {
            log!("complete_oauth: keychain write failed: {}", e);
        } else {
            log!("complete_oauth: refresh token saved for {}", email);
        }
    }

    // Add or update the account in the persisted list.
    {
        let mut list = state.accounts.lock().unwrap();
        if !list.iter().any(|a| a.id == account_id) {
            list.push(Account {
                id: account_id.clone(),
                email: email.clone(),
                added_at: chrono::Utc::now().to_rfc3339(),
                // Leave label config unset — effective_*_label
                // resolves to DEFAULT_* until the user customizes.
                notes_label: None,
                meta_label: None,
                llm: Default::default(),
                backend_kind,
                root_dir: None,
                // OAuth path only — iCloud never arrives here.
                icloud_session_established: false,
                blocked_reason: None,
                sync_cursor: None,
                icloud_replica_id: None,
        status: accounts::AccountStatus::Active,
                pending_removal: false,
            });
            if let Err(e) = accounts::save_accounts(&list) {
                log!("complete_oauth: save_accounts failed: {}", e);
            }
        }
    }

    // Cache the access token in this account's state. Keyed by account_id,
    // like every other reader of this map (ensure_token_inner etc.) — not by
    // bare email, or the fast-path cache lookup after sign-in always misses.
    {
        let mut states = state.account_states.lock().unwrap();
        let entry = states.entry(account_id.clone()).or_default();
        entry.access_token = Some(token_data.access_token);
        entry.token_expires_at = Some(token_deadline_from_expires_in(token_data.expires_in));
    }

    log!("complete_oauth: emitting oauth-success");
    let _ = app.emit("oauth-success", email);
}

#[tauri::command]
async fn open_auth_url(app: AppHandle, url: String) -> Result<(), String> {
    // Open via the opener plugin (OS shell-open API), NOT a child process.
    // The old Windows path `cmd /c start <url>` truncated the URL at the first
    // `&` because cmd treats `&` as a command separator — Google then received
    // an auth request missing redirect_uri/scope/response_type and rejected it
    // with `Error 400: invalid_request`. macOS `open` was unaffected. The
    // opener plugin passes the full URL to the OS handler on every platform.
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())?;

    // Android returns here and waits for nothing: the auth code arrives as an
    // App Links Intent, handled by the `on_open_url` / `get_current` pair
    // registered in `.setup()`. That Intent may well *start* the process, which
    // is the entire point — an S23 FE running Android 16 killed Jodd while the
    // user was on Google's consent screen, and a listener inside a dead process
    // hears nothing. The PKCE verifier survives that death in the keychain
    // (`secrets::save_pending_signin`), which is what makes a cold-start callback
    // completable at all.
    #[cfg(target_os = "android")]
    {
        log!("open_auth_url: browser launched, awaiting the App Links Intent");
    }

    #[cfg(not(target_os = "android"))]
    {
        // Retire any listener still holding port 8080 from an abandoned attempt,
        // and wait for it to actually let go — cancelling only asks; the port is
        // released when that task returns and drops its Server. Without this the
        // second sign-in of a session fails with "Address already in use".
        let cancel = tokio_util::sync::CancellationToken::new();
        if let Some(prev) = app
            .state::<AppState>()
            .oauth_cancel
            .lock()
            .unwrap()
            .replace(cancel.clone())
        {
            log!("open_auth_url: cancelling the previous sign-in's listener");
            prev.cancel();
        }

        log!("open_auth_url: browser launched, waiting for callback on :8080");
        let app_clone = app.clone();
        tokio::spawn(async move {
            // `wait_for_callback_blocking` parks a thread on the socket, so it
            // belongs on the blocking pool rather than a runtime worker.
            let waited = tokio::task::spawn_blocking(move || {
                // Give a cancelled predecessor a moment to unbind before we try.
                for _ in 0..20 {
                    match auth::wait_for_callback_blocking(8080, &cancel) {
                        Err(e) if e.contains("Address already in use") => {
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                        other => return other,
                    }
                }
                Err("port 8080 is still held by a previous sign-in".to_string())
            })
            .await;

            match waited {
                Ok(Ok(auth::CallbackOutcome::Code(cb))) => {
                    complete_oauth(app_clone, cb.code, cb.state).await
                }
                Ok(Ok(auth::CallbackOutcome::Denied(d))) => {
                    // Logged in full because this is the only record of what
                    // the provider actually sent — and the measured Microsoft
                    // refusal sends so little that a support conversation
                    // needs every byte of it.
                    log!(
                        "open_auth_url: authorization refused: error={} subcode={:?} description={:?}",
                        d.error,
                        d.subcode,
                        d.description
                    );
                    let state = app_clone.state::<AppState>();
                    // Taken, not peeked — the same one-shot discipline
                    // `complete_oauth` applies on the success path, so an
                    // abandoned Microsoft attempt cannot leave the slot set
                    // for a later Gmail sign-in.
                    let kind = std::mem::take(&mut *state.pending_backend.lock().unwrap());
                    // This flow is over. Clear the verifier from both the
                    // in-memory slot and the keychain, for the reason spelled
                    // out in `complete_oauth`: a leftover pair a stray retry
                    // could replay is worse than none.
                    let _ = state.pending_pkce.lock().unwrap().take();
                    secrets::clear_pending_signin();
                    let _ = app_clone.emit("oauth-error", signin_denial(kind, &d));
                }
                Ok(Err(e)) => {
                    log!("open_auth_url: wait_for_callback FAILED: {}", e);
                    emit_oauth_error(&app_clone, e.to_string());
                }
                Err(e) => {
                    log!("open_auth_url: listener task panicked: {}", e);
                    emit_oauth_error(&app_clone, "sign-in listener crashed");
                }
            }
        });
    }

    Ok(())
}

/// Which OS this build is running on. The frontend uses it to hide features
/// the platform cannot provide — see src/lib/stores/platform.ts.
#[tauri::command]
fn platform_name() -> String {
    std::env::consts::OS.to_string()
}

// ─── Account management ──────────────────────────────────────────────────────

#[tauri::command]
fn list_accounts(state: State<'_, AppState>) -> Vec<Account> {
    state.accounts.lock().unwrap().clone()
}

/// The actual teardown: delete the credential, forget an iCloud webview
/// session, drop the account row, and wipe every piece of in-memory/cached
/// state that outlives it.
///
/// No lifecycle check of its own — every caller has already established this
/// is safe: `remove_account`'s immediate path (`removal_allowed`), or the
/// worker's `pending_removal` completion loop (`should_complete_pending_
/// removal`, gated on `status == Inactive`).
async fn perform_account_removal(state: &State<'_, AppState>, account_id: &str) -> Result<(), String> {
    accounts::delete_refresh_token(account_id);
    // iCloud keeps its credential nowhere this function has touched: the
    // session is a live cookie jar in the webview's own data store. Left
    // behind, the NEXT sign-in finds it valid and `/validate` answers with the
    // Apple ID that was just removed — an account silently created for the
    // wrong person. Runs before the account row goes, so a failure here still
    // leaves something to retry against.
    #[cfg(icloud_webview)]
    if account_backend_kind(state, account_id) == Some(accounts::BackendKind::ICloud) {
        icloud_auth::forget_session(&state.app_handle).await;
    }
    {
        let _gate = state.ai_policy.gate.lock().unwrap();
        invalidate_ai(state, &state.app_handle);
        let mut list = state.accounts.lock().unwrap();
        list.retain(|a| a.id != account_id);
        accounts::save_accounts(&list)?;
    }
    state
        .account_states
        .lock()
        .unwrap()
        .remove(account_id);
    // Drop any (account_id, uuid) entries from in-flight push tracking. If a
    // push was mid-await when remove fired, line 1163 of the worker already
    // cleans up after the await returns — but if the await never returns
    // (process kill, panic) the entry would leak. Re-adding the same email
    // later would then see stale `pushing` entries and suppress real remote
    // edits as "our own push". This explicit wipe closes that window.
    state
        .pushing
        .lock()
        .unwrap()
        .retain(|(aid, _)| aid != account_id);
    // Drop any stale dup_stats so the sidebar pill doesn't linger after sign-out.
    state.dup_stats.lock().unwrap().remove(account_id);
    // Nothing else drops this, and a re-added account with the same id would
    // otherwise read the removed account's notes out of a cache entry that
    // outlived it.
    state.icloud_scans.lock().unwrap().remove(account_id);
    // Wipe the local replica for this account. Keeping rows around after
    // remove would (a) leak note bodies on disk for an account the user
    // thinks they signed out of, and (b) confuse any sync worker that
    // wakes up while the keychain entry is gone.
    match state.db.delete_account(account_id) {
        Ok((n, f)) => log!(
            "remove_account: wiped {} note row(s) and {} folder row(s) for {}",
            n, f, account_id
        ),
        Err(e) => log!("remove_account: cache wipe failed for {}: {}", account_id, e),
    }
    Ok(())
}

/// What `remove_account` actually did: an immediate removal, or a request
/// recorded on a still-`Draining` account for `sync_worker_tick` to finish
/// once its queue genuinely empties (`pending_removal`, roadmap #0d).
///
/// Serializes as the bare string `"removed"` / `"queued"` so the frontend can
/// branch on server truth instead of inferring the outcome from the account's
/// status at click time — which can race the worker's own Draining -> Inactive
/// flip between the frontend's read and this command's response.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveAccountOutcome {
    Removed,
    Queued,
}

#[tauri::command]
async fn remove_account(
    account_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<RemoveAccountOutcome, String> {
    {
        let _gate = state.ai_policy.gate.lock().unwrap();
        invalidate_ai(&state, &app);
    }
    if let Some(status) = account_status_of(&state, &account_id) {
        if !removal_allowed(status) {
            // Draining: the queue isn't empty yet, so deleting the credential
            // now would strand the drain. Rather than refusing outright (the
            // old behavior — the only ways forward were waiting indefinitely
            // or "Stop waiting", which strands the queue), queue the request:
            // `sync_worker_tick` finishes it, unattended, the moment
            // `has_pending_pushes` genuinely returns false and the account
            // reaches `Inactive` on its own.
            let mut list = state.accounts.lock().unwrap();
            queue_removal(&mut list, &account_id);
            accounts::save_accounts(&list)?;
            log!(
                "remove_account: {} still draining — queued for removal (pending_removal)",
                account_id
            );
            return Ok(RemoveAccountOutcome::Queued);
        }
    }
    let _lease = state.sync_schedule.exclusive(&account_id).await;
    // A status change may have landed while waiting for the in-flight round.
    if account_status_of(&state, &account_id).is_some_and(|status| !removal_allowed(status)) {
        let mut list = state.accounts.lock().unwrap();
        queue_removal(&mut list, &account_id);
        accounts::save_accounts(&list)?;
        return Ok(RemoveAccountOutcome::Queued);
    }
    perform_account_removal(&state, &account_id).await?;
    Ok(RemoveAccountOutcome::Removed)
}

/// Return the user-facing settings for one account. Resolves the
/// Option<String> fields in `Account` to concrete strings — the frontend
/// sees the effective label names, not the "unset = use default" rule.
#[tauri::command]
fn get_account_settings(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<accounts::AccountSettings, String> {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id)
        .map(|a| a.settings())
        .ok_or_else(|| format!("Account not found: {}", account_id))
}

/// What the frontend needs from `Capabilities`: whether the sidebar should
/// offer a Trash/"Recently Deleted" entry, and what this account can be
/// written to, per area (`backend::Writes`).
#[derive(serde::Serialize)]
pub struct CapabilitiesDto {
    pub has_trash: bool,
    pub writes: backend::Writes,
}

/// Static per-backend capabilities for one account, keyed off its stored
/// `backend_kind` alone.
///
/// Deliberately does NOT go through `vertical_for`: that would fetch a live
/// token for a Microsoft account on every call — a network round trip on a
/// plain account-switch navigation, which the local-first doctrine treats as
/// a bug — and `vertical_for` refuses accounts with status `Inactive`
/// (gotcha #2), which would leave the UI with nothing to render for one.
/// `Capabilities::for_backend` is the single source of truth this command
/// and every vertical's `new()` both read from.
#[tauri::command]
fn backend_capabilities(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<CapabilitiesDto, String> {
    let list = state.accounts.lock().unwrap();
    let kind = list
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind)
        .ok_or_else(|| format!("Account not found: {}", account_id))?;
    let caps = Capabilities::for_backend(kind);
    Ok(CapabilitiesDto { has_trash: caps.has_trash, writes: caps.writes })
}

/// Refuse a write this backend cannot perform.
///
/// **Must run before anything touches SQLite.** See `backend::Writes` for the
/// cost of a leak.
fn refuse_write(state: &State<'_, AppState>, account_id: &str, w: backend::Write)
    -> Result<(), String>
{
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).map(|a| a.backend_kind)
    };
    write_refusal_for(kind, w).map(|m| Err(m.to_string())).unwrap_or(Ok(()))
}

/// Backend-neutral, like its two siblings below, and deliberately so.
///
/// It used to name Microsoft and Milestone 4. M4 shipped (2026-08-16) and
/// turned `writes.sidecars` on for Microsoft, which left this string
/// unreachable on every backend at once — dead text that still read as
/// current. `BackendKind::ICloud` makes it reachable again, and a message
/// naming Microsoft would then be wrong in both halves: the backend, and the
/// promise that "editing notes and folders works normally" when on iCloud
/// nothing is writable at all.
///
/// Naming no backend is also what makes the advice true everywhere it can now
/// fire. On iCloud it is not a consolation prize: `IsPinned` is a real field
/// on the CloudKit record, so a pin set in Apple Notes genuinely does show up
/// here on the next refresh.
const SIDECARS_UNAVAILABLE_MSG: &str =
    "This account can't accept pins in Jodd. Pin the note in Apple Notes and \
     it will show up here.";

const NOTES_UNAVAILABLE_MSG: &str =
    "This account can't accept note edits in Jodd yet. Edit the note in Apple \
     Notes and it will sync back here.";

const FOLDERS_UNAVAILABLE_MSG: &str =
    "This account can't accept folder changes in Jodd yet. Create or rename the \
     folder in Apple Notes and it will sync back here.";

/// Named separately from `NOTES_UNAVAILABLE_MSG` because the two can now
/// disagree — iCloud is `notes: false, relocate: true`, so a refusal here
/// must not tell the user to go edit in Apple Notes when moving/deleting is
/// exactly what Jodd can do.
const RELOCATE_UNAVAILABLE_MSG: &str =
    "This account can't move or delete notes in Jodd yet. Do it in Apple Notes \
     and it will sync back here.";

/// The decision `refuse_write` makes, without the `State` around it.
/// `None` for an unknown account — every command has its own not-found path.
fn write_refusal_for(kind: Option<accounts::BackendKind>, w: backend::Write)
    -> Option<&'static str>
{
    let k = kind?;
    if Capabilities::for_backend(k).writes.allows(w) { return None; }
    Some(match w {
        backend::Write::Notes => NOTES_UNAVAILABLE_MSG,
        backend::Write::Relocate => RELOCATE_UNAVAILABLE_MSG,
        backend::Write::Folders => FOLDERS_UNAVAILABLE_MSG,
        backend::Write::Sidecars => SIDECARS_UNAVAILABLE_MSG,
    })
}

/// Whether this backend's sidecar store exists at all — the same decision
/// `refuse_write`/`write_refusal_for` make for `Write::Sidecars`, reused
/// here (not re-derived) so the command-layer gate and the worker's own
/// gate below can never drift. `None` (unknown account) reads as supported,
/// matching `write_refusal_for`'s "defer to the caller's own not-found
/// path" convention — in practice this is only called from the sync
/// worker's sidecar drains, which already filtered to accounts present in
/// `state.accounts` before reaching here.
///
/// This exists because `refuse_write` only guards the COMMANDS that set
/// `pin_dirty` (`set_pin`, `set_pin_batch`). It does not guard the sync
/// worker's drain of that flag. A row minted `pin_dirty = 1` on a backend
/// whose `put_sidecar` refuses (Microsoft pre-M4; iCloud, where the pin is
/// Apple's own on another record type and a sidecar could never win)
/// would otherwise retry forever, `has_pending_pushes` would never clear, a
/// Draining account would never reach Inactive, and `remove_account` would
/// refuse it permanently — the exact wedge the whole capability split
/// exists to prevent, reopened one layer below where `refuse_write` looks.
fn sidecars_supported(kind: Option<accounts::BackendKind>) -> bool {
    write_refusal_for(kind, backend::Write::Sidecars).is_none()
}

/// Whether orphan cleanup/preview applies to this backend at all — see
/// `safe_cleanup_orphans_for_account`'s comment for the mechanism reasoning
/// (Gmail's insert-new+trash-old save dance is the only thing that produces
/// the transient duplicate messages this feature targets). Written as an
/// exhaustive match rather than an `== LocalFs`/`== Gmail` equality check so
/// a future `BackendKind` variant fails closed — excluded by default — rather
/// than silently falling through to `gmail::get_label_map`/
/// `gmail::find_all_duplicate_ids`, which are hardcoded to
/// `gmail.googleapis.com` and would be handed a token for a different host
/// entirely. `Microsoft` was exactly this gap: `LocalFs` had a guard,
/// `Microsoft` did not, and nothing caught the omission until this fix.
fn orphan_cleanup_supported(kind: accounts::BackendKind) -> bool {
    match kind {
        accounts::BackendKind::Gmail => true,
        accounts::BackendKind::LocalFs
        | accounts::BackendKind::Microsoft
        // Nothing to clean up and nothing that could: cleanup trashes
        // duplicate remote messages, which is a write, and this backend has
        // none.
        | accounts::BackendKind::ICloud => false,
    }
}

#[cfg(test)]
mod restore_kind_tests {
    use super::*;

    /// `restore_note` used to be `if Gmail { … } else { …LocalFs… }` — true
    /// while exactly two backends had a trash, and a live defect the moment a
    /// third did: the `else` ran LocalFs's trash-filename decode over a
    /// CloudKit `recordName`.
    #[test]
    fn every_backend_states_how_it_restores() {
        use accounts::BackendKind::*;
        assert_eq!(restore_kind(Gmail), Some(RestoreKind::UntrashThenMove));
        assert_eq!(restore_kind(LocalFs), Some(RestoreKind::UntrashToEncodedPath));
        assert_eq!(restore_kind(ICloud), Some(RestoreKind::MoveOutOfTrash));
        assert_eq!(
            restore_kind(Microsoft),
            None,
            "measured: an Apple-side delete leaves nothing in Deleted Items"
        );
    }

    /// The two facts must agree. A backend that reports a trash the UI shows,
    /// and then has no way to restore from it, is the always-refusing button
    /// the capability was introduced to prevent.
    #[test]
    fn a_backend_with_a_trash_can_restore_from_it_and_one_without_cannot() {
        for kind in accounts::ALL_BACKENDS {
            assert_eq!(
                backend::Capabilities::for_backend(kind).has_trash,
                restore_kind(kind).is_some(),
                "{kind:?} disagrees with itself about having a Recently Deleted"
            );
        }
    }
}

#[cfg(test)]
mod incremental_pull_tests {
    use super::*;

    /// A fresh install, or a restart that lost accounts.json, has no cursor —
    /// so the backend hands back the whole account and there is nothing it
    /// changed *since*. Reporting that as "everything just changed" would drop
    /// the cached read on every first tick, which is the opposite of what a
    /// resume token is for.
    #[test]
    fn a_first_run_establishes_a_cursor_without_claiming_anything_changed() {
        assert_eq!(pull_outcome(None, 0), PullOutcome::Primed);
        assert_eq!(pull_outcome(None, 776), PullOutcome::Primed);
    }

    #[test]
    fn a_quiet_account_is_quiet_and_a_changed_one_says_how_many() {
        assert_eq!(pull_outcome(Some("tok"), 0), PullOutcome::Quiet);
        assert_eq!(pull_outcome(Some("tok"), 3), PullOutcome::Changed(3));
    }

    /// `accounts.json` also holds the user's settings, and the backend hands
    /// back a fresh token on every call — so writing each one would churn that
    /// file once a minute, for the life of the process, to record that nothing
    /// happened. The cursor already in hand is still valid: re-sending it asks
    /// the same question and gets the same empty answer.
    #[test]
    fn a_quiet_run_writes_nothing_to_disk() {
        assert!(!cursor_write_needed(PullOutcome::Quiet));
        assert!(cursor_write_needed(PullOutcome::Primed));
        assert!(cursor_write_needed(PullOutcome::Changed(1)));
    }

    /// The two predicates over `PullOutcome` disagree on exactly one arm, and
    /// that disagreement is the point: `Primed` writes a cursor and must NOT
    /// wake the frontend. Priming hands back the whole account on every fresh
    /// install, so waking on it would make the app re-read everything at
    /// startup to learn that nothing changed — the same mistake, one layer up,
    /// that `PullOutcome::Primed` exists to keep out of the cursor.
    #[test]
    fn priming_writes_a_cursor_but_does_not_wake_the_frontend() {
        assert!(cursor_write_needed(PullOutcome::Primed));
        assert!(!frontend_wakeup_needed(PullOutcome::Primed));

        assert!(!frontend_wakeup_needed(PullOutcome::Quiet));
        assert!(frontend_wakeup_needed(PullOutcome::Changed(1)));
    }

    /// Exhaustive, so a fifth backend fails closed rather than inheriting a
    /// poll against an endpoint it does not have. Gmail and Microsoft return an
    /// inert cursor from `changes_since`, so a detector there would report
    /// "nothing changed" forever — worse than not running at all.
    #[test]
    fn only_a_backend_with_a_real_incremental_read_is_polled() {
        assert!(incremental_pull_supported(accounts::BackendKind::ICloud));
        for k in [
            accounts::BackendKind::Gmail,
            accounts::BackendKind::Microsoft,
            accounts::BackendKind::LocalFs,
        ] {
            assert!(!incremental_pull_supported(k), "{k:?} has no real cursor to resume from");
        }
    }

    /// The cursor is opaque bytes to the core and must stay that way — the
    /// round trip through `accounts.json` is UTF-8 in and UTF-8 out, and
    /// nothing between parses it.
    #[test]
    fn a_cursor_survives_the_round_trip_through_the_account_record() {
        let mut a = crate::accounts::Account {
            id: "icloud:someone@me.com".into(),
            email: "someone@me.com".into(),
            added_at: String::new(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: accounts::BackendKind::ICloud,
            root_dir: None,
            icloud_session_established: true,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: accounts::AccountStatus::Active,
            pending_removal: false,
        };
        a.sync_cursor = Some("AQAAAAA/opaque+token=".into());
        let json = serde_json::to_string(&a).unwrap();
        let back: crate::accounts::Account = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sync_cursor.as_deref(), Some("AQAAAAA/opaque+token="));
    }

    /// Every accounts.json written before this field existed must still parse,
    /// with no cursor — the same `#[serde(default)]` discipline every other
    /// field on this record follows.
    #[test]
    fn an_accounts_file_written_before_this_field_still_parses() {
        let old = serde_json::json!({
            "id": "gmail:a@b.com", "email": "a@b.com", "added_at": "2026-01-01T00:00:00Z"
        });
        let a: crate::accounts::Account = serde_json::from_value(old).unwrap();
        assert_eq!(a.sync_cursor, None);
    }
}

#[cfg(test)]
mod orphan_cleanup_supported_tests {
    use super::*;

    #[test]
    fn gmail_supports_orphan_cleanup() {
        assert!(orphan_cleanup_supported(accounts::BackendKind::Gmail));
    }

    #[test]
    fn localfs_does_not_support_orphan_cleanup() {
        assert!(!orphan_cleanup_supported(accounts::BackendKind::LocalFs),
            "one file per uuid — there is structurally nothing to clean up");
    }

    /// The regression this guards: before this fix, only LocalFs was
    /// excluded, and a Microsoft account would fall through to
    /// `gmail::get_label_map`/`gmail::find_all_duplicate_ids` — Gmail-only
    /// APIs hardcoded to `gmail.googleapis.com` — with a Microsoft Graph
    /// token in hand.
    #[test]
    fn microsoft_does_not_support_orphan_cleanup() {
        assert!(!orphan_cleanup_supported(accounts::BackendKind::Microsoft),
            "Exchange PATCHes in place — a uuid never has two live messages to dedup");
    }
}

/// Refuse a label change while relabelling is still unsafe.
///
/// Both labels define the remote scope every reconciler compares the cache
/// against, and every reconciler reads "absent from the listing" as "deleted".
/// Changing a label silently moves that scope, so the next sweep reads the
/// move as a mass deletion:
///
/// * `notes_label` — `list_notes` calls `Db::prune_clean`, which drops every
///   `clean` row for the account that the fetch did not return. Point the
///   account at a label with nothing in it and the cache empties.
/// * `meta_label` — `clear_pins_not_in` does the same to pin state (the only
///   remaining sidecar; tags round-trip via the body instead). It is guarded
///   by `list_sidecars` returning `None` while the label does not exist yet,
///   but the first pin push calls `ensure_label` and creates it, so the
///   guard lapses on the tick after.
///
/// Saving with the values unchanged must still succeed: the settings modal
/// submits both fields together, so a user who opens it and presses Save
/// without editing anything is not asking for a change.
fn refuse_unsafe_label_change(
    current: &accounts::AccountSettings,
    next_notes: &str,
    next_meta: &str,
) -> Result<(), String> {
    if next_notes != current.notes_label {
        return Err(format!(
            "Changing the Notes label is disabled for now. Jodd would read every \
             note under \"{}\" as deleted on the next sync and clear it from this \
             device. Nothing is removed from Gmail — but the local copy and \
             everything derived from it would have to be rebuilt. Safe relabelling \
             is planned; until then keep \"{}\".",
            current.notes_label, current.notes_label
        ));
    }
    if next_meta != current.meta_label {
        return Err(format!(
            "Changing the Meta label is disabled for now. Pin and tag state lives in \
             sidecar messages under \"{}\"; pointing Jodd at another label clears \
             pins and tags from this device once that label exists in Gmail. The \
             sidecars themselves are left alone. Safe relabelling is planned; until \
             then keep \"{}\".",
            current.meta_label, current.meta_label
        ));
    }
    Ok(())
}

#[cfg(test)]
mod label_change_guard_tests {
    use super::*;

    fn settings(notes: &str, meta: &str) -> accounts::AccountSettings {
        accounts::AccountSettings {
            notes_label: notes.to_string(),
            meta_label: meta.to_string(),
        }
    }

    #[test]
    fn allows_a_save_that_changes_neither_label() {
        let s = settings("Notes", "Notes-Meta");
        assert!(refuse_unsafe_label_change(&s, "Notes", "Notes-Meta").is_ok());
    }

    #[test]
    fn refuses_a_notes_label_change() {
        let s = settings("Notes", "Notes-Meta");
        let err = refuse_unsafe_label_change(&s, "MyNotes", "Notes-Meta")
            .expect_err("changing notes_label must be refused");
        assert!(err.contains("Notes label"), "unhelpful message: {err}");
    }

    #[test]
    fn refuses_a_meta_label_change() {
        let s = settings("Notes", "Notes-Meta");
        let err = refuse_unsafe_label_change(&s, "Notes", "MyMeta")
            .expect_err("changing meta_label must be refused");
        assert!(err.contains("Meta label"), "unhelpful message: {err}");
    }

    /// The refusal has to say what would happen, not just "no". A user who is
    /// told only that it is disabled will look for a way around it.
    #[test]
    fn the_refusal_explains_the_consequence() {
        let s = settings("Notes", "Notes-Meta");
        let err = refuse_unsafe_label_change(&s, "MyNotes", "Notes-Meta").unwrap_err();
        assert!(
            err.to_lowercase().contains("gmail"),
            "message should say the notes remain in Gmail: {err}"
        );
    }
}

/// Persist per-account label settings. Validates the supplied strings:
/// nonempty, no leading/trailing whitespace, no embedded control chars,
/// length cap (Gmail tops out around 225 chars; we use 200 to leave room).
/// Empty strings reset to defaults so the user can "clear back to default"
/// via the UI without us needing a separate command.
#[tauri::command]
async fn update_account_settings(
    account_id: String,
    notes_label: String,
    meta_label: String,
    state: State<'_, AppState>,
) -> Result<accounts::AccountSettings, String> {
    fn normalize(raw: String) -> Result<Option<String>, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None); // empty = clear to default
        }
        if trimmed.chars().any(|c| c.is_control()) {
            return Err("Label cannot contain control characters".into());
        }
        if trimmed.len() > 200 {
            return Err("Label is too long".into());
        }
        Ok(Some(trimmed.to_string()))
    }
    let notes = normalize(notes_label)?;
    let meta = normalize(meta_label)?;
    let updated = {
        let mut list = state.accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("Account not found: {}", account_id))?;
        // Compare what the account would EFFECTIVELY end up with, not the raw
        // Option: clearing the field to "" resolves to the default, so blanking
        // a box already holding "Notes" is not a change, while blanking one
        // holding "MyNotes" very much is.
        refuse_unsafe_label_change(
            &acct.settings(),
            notes.as_deref().unwrap_or(accounts::DEFAULT_NOTES_LABEL),
            meta.as_deref().unwrap_or(accounts::DEFAULT_META_LABEL),
        )?;
        acct.notes_label = notes;
        acct.meta_label = meta;
        let snap = acct.settings();
        accounts::save_accounts(&list)?;
        snap
    };
    // Settings changes can affect what the worker pushes against (e.g.,
    // a different meta_label means dirty_pin rows now target a different
    // Gmail label). Invalidate the label map so the next push refetches.
    invalidate_label_cache(&state, &account_id);
    log!(
        "update_account_settings: {} notes_label={:?} meta_label={:?}",
        account_id, updated.notes_label, updated.meta_label
    );
    Ok(updated)
}

/// Add a LocalFS account backed by a local directory. The directory must
/// already exist. The account id is a `localfs:<uuid>` string so it is
/// disjoint from Gmail account ids (which are email addresses). After
/// persisting the account, cold-starts an index so the account's folders
/// and notes populate the cache immediately (visible in the sidebar on the
/// next `list_folders` / `list_cached_notes` call from the frontend).
///
/// Returns the new `Account` so the frontend can add it to its account list
/// without a round-trip to `list_accounts`.
/// Mint a LocalFs account id — `localfs:{uuid}`.
///
/// **The suffix is a fresh UUID, never the vault's display name, and migration
/// #19 depends on that.** A LocalFs `Account.email` holds an arbitrary label
/// ("My vault"), not an address. So if a *bare* LocalFs id ever reached
/// `migrate_account_ids_with`, it would call
/// `account_id_for(LocalFs, &acct.email)` and mint `localfs:My vault` — a
/// shape nothing else in the system produces.
///
/// That is unreachable only because every id minted here is already qualified,
/// so `is_qualified` makes the migration skip it. This function exists to give
/// that invariant a name and a test; before it, the assumption was load-bearing
/// and pinned by nothing.
fn localfs_account_id() -> accounts::AccountId {
    accounts::account_id_for(
        accounts::BackendKind::LocalFs,
        &crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()),
    )
}

/// Sign in to iCloud and, if the account is readable, create it.
///
/// **The whole flow, in the order Component B1 fixes** — and the order is the
/// design, not an implementation detail:
///
/// 1. Refuse a second Apple ID before anything opens. One `WKWebView` data
///    store means one cookie jar per install today.
/// 2. Apple's own pages handle password / 2FA / CAPTCHA in a visible webview.
///    Jodd never touches the form.
/// 3. **Read one account's worth of notes and check they decode, BEFORE
///    persisting anything.** With Advanced Data Protection on, the bodies are
///    genuinely end-to-end encrypted and no amount of retrying helps. Creating
///    the account first and discovering that after would leave the user with a
///    permanently broken entry to find and clean up; refusing now leaves
///    nothing behind.
/// 4. Only then persist — with a marker, never a credential. `accounts.json`
///    holds `icloud_session_established: bool` and nothing else about the
///    session, because the session is a live cookie jar the app must not copy
///    (gotcha #19).
///
/// Returns the Apple ID as Apple reports it, matching what `complete_oauth`
/// emits for the other backends so the frontend's existing success path works
/// unchanged.
/// The M2 write census, run against the account's own live session.
///
/// **A diagnostic with no UI, on purpose.** `examples/icloud_probe` reads
/// icloud-md's stored cookie jar — scaffolding from before this project had an
/// iCloud session of its own — so measuring an account Jodd is already signed
/// into meant refreshing a third-party tool's expired session through a HAR
/// capture. This runs the same `census::report` over a zone read made with the
/// session the app is holding.
///
/// Invoke it from the app's devtools console:
///
/// ```js
/// await __TAURI__.core.invoke('icloud_census', { accountId: 'icloud:you@me.com' })
/// ```
///
/// Read-only: it walks the zone and sends nothing. Costs one whole-zone read,
/// which is the price every explicit refresh on this backend already pays.
#[tauri::command]
async fn icloud_census(account_id: String, state: State<'_, AppState>) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    // Built directly rather than through `vertical_for`'s `Box<dyn Vertical>`:
    // the census is not part of any trait, because it is a diagnostic on this
    // one backend and widening a shared trait to carry it would put a stub in
    // front of three backends that have nothing to report.
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .write_census()
        .await
        .map_err(|e| e.to_string())?;
    // Logged as well as returned: the console shows it to whoever ran it, the
    // log is what gets pasted into a milestone note.
    log!("icloud_census[{account_id}]:\n{report}");
    Ok(report)
}

/// The live write self-test, against the account's own session.
///
/// The counterpart of `icloud_census`, and the only thing that can confirm the
/// one part of M2 no test in this repo can: whether Apple accepts the
/// `records/modify` request shape, and whether the server really re-checks a
/// `recordChangeTag`.
///
/// **It writes.** `folder` must name a folder that already exists — make an
/// empty one in Apple Notes first — and the scratch note's `recordName` is
/// minted inside the vertical, never taken from here, so nothing a caller
/// passes can point this at a real note.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_write_selftest',
///   { accountId: 'icloud:you@me.com', folder: 'Notes/FolderforJoddTesting' })
/// ```
#[tauri::command]
async fn icloud_write_selftest(
    account_id: String,
    folder: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if folder.trim().is_empty() {
        return Err("a destination folder is required — this writes to a real account, and \
                    a run with no destination would land a note in the user's root"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .write_selftest(&folder)
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_write_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// The live **relocation** self-test — the write questions the census did not
/// close.
///
/// `icloud_census` measured the note DOCUMENT and answered no: `WRITABLE:
/// 0/776`, because Apple's per-character CRDT identity is on every note and
/// `compose` cannot mint the `CharID`s an insertion needs. Move, delete and
/// folder writes are a different question — none of them sends a document at
/// all — and nothing has measured them.
///
/// **It writes, and one thing it tests could destroy content**: whether
/// CloudKit's `update` really changes only the fields it is given, or replaces
/// the record. The vertical captures the note's exact document bytes first and
/// puts them back if a move disturbs them, but the containment that matters is
/// the argument: `folder` must name a folder holding **exactly one note**, and
/// that note is the subject. Prepare a throwaway folder with a throwaway note.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_relocation_selftest',
///   { accountId: 'icloud:you@me.com', folder: 'Notes/Jodd M2 Test Folder' })
/// ```
#[tauri::command]
async fn icloud_relocation_selftest(
    account_id: String,
    folder: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if folder.trim().is_empty() {
        return Err("a folder is required — it names the one note this test relocates, and \
                    there is no default that would be safe"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .relocation_selftest(folder.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_relocation_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// **The content self-test (M2.5)** — proves the CRDT text-edit engine
/// against a REAL note that already carries CRDT identity, not a scratch
/// one Jodd minted (which never carries `substring`/`timestamp` — see
/// `ICloudVertical::content_write_selftest`'s own doc comment for why no
/// scratch equivalent exists). Appends a marker, verifies it landed
/// byte-exact, reverts to the original text, verifies that too. Net effect
/// on the note: none — but two real writes happen.
///
/// Mints/persists this account's CRDT replica id on first use
/// (`ensure_icloud_replica_id`) — the same durable identity every future
/// CRDT write on this account will reuse.
///
/// **Point this at a note you chose deliberately** — `recordName` is not
/// validated beyond "exists and is CRDT-writable"; there is no scratch-folder
/// containment the way `relocation_selftest` has, because there is no way to
/// manufacture a CRDT-carrying scratch note to contain it to.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_content_write_selftest',
///   { accountId: 'icloud:you@me.com', recordName: 'test-m2-5-note-a51636f7' })
/// ```
#[tauri::command]
async fn icloud_content_write_selftest(
    account_id: String,
    record_name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if record_name.trim().is_empty() {
        return Err("a recordName is required — this writes to a real, existing note, and \
                    there is no default that would be safe"
            .to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .content_write_selftest(record_name.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_content_write_selftest[{account_id}]:\n{report}");
    Ok(report)
}

/// **Forensics for one disagreement** — three independent surfaces (Mac
/// Notes.app, icloud.com, iPhone) agree with each other and disagree with
/// Jodd's own zone walk about where a note lives. That rules out staleness on
/// Apple's side; what is left to look at is the raw record this backend
/// itself read, not another summary of it.
///
/// `titleContains` filters by a substring because the caller has no
/// `recordName` to hand — this reads its own account, walking the whole zone
/// like the census does, and prints identifiers and counts only.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_debug_note_history',
///   { accountId: 'icloud:you@me.com', titleContains: 'M2 Note' })
/// ```
#[tauri::command]
async fn icloud_debug_note_history(
    account_id: String,
    title_contains: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if title_contains.trim().is_empty() {
        return Err("a title substring is required".to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .debug_note_history(title_contains.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_debug_note_history[{account_id}]:\n{report}");
    Ok(report)
}

/// **A direct point read, not another walk of the change feed.** Every other
/// diagnostic on this backend — including `icloud_debug_note_history` above —
/// reads through `changes/zone`, a change feed whose own staleness is
/// gotcha #22's subject. This calls `records/lookup` instead: the per-record
/// endpoint icloud-md uses and this codebase never had. It exists to answer,
/// rather than assume, whether a feed/client disagreement is the whole
/// endpoint behind or just the feed — pass the `recordName` a prior
/// `icloud_debug_note_history` run already printed.
///
/// ```js
/// await __TAURI__.core.invoke('icloud_debug_record_lookup',
///   { accountId: 'icloud:you@me.com', recordName: 'E4513CCE-...' })
/// ```
#[tauri::command]
async fn icloud_debug_record_lookup(
    account_id: String,
    record_name: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if account_backend_kind(&state, &account_id) != Some(accounts::BackendKind::ICloud) {
        return Err(format!("{account_id} is not an iCloud account"));
    }
    if record_name.trim().is_empty() {
        return Err("a recordName is required".to_string());
    }
    #[cfg(not(icloud_webview))]
    let report = return Err("iCloud needs a desktop webview to hold the session".to_string());
    #[cfg(icloud_webview)]
    let report = icloud_vertical_for(&state, &account_id)
        .await?
        .debug_record_lookup(record_name.trim())
        .await
        .map_err(|e| e.to_string())?;
    log!("icloud_debug_record_lookup[{account_id}]:\n{report}");
    Ok(report)
}

#[tauri::command]
async fn icloud_sign_in(state: State<'_, AppState>) -> Result<String, String> {
    #[cfg(not(icloud_webview))]
    {
        let _ = &state;
        return Err("iCloud sign-in is not available on this platform.".to_string());
    }
    #[cfg(icloud_webview)]
    {
        // [1] Before the window opens, so the user is not asked to sign in to
        // an account that will then be refused.
        {
            let list = state.accounts.lock().unwrap();
            icloud_auth::refuse_second_icloud_account(&list)?;
        }

        let app = state.app_handle.clone();

        // [2] Apple's flow, start to finish. Blocks until the session is
        // complete, the user closes the window, or it times out.
        let session = icloud_auth::sign_in(&app).await?;
        log!("icloud_sign_in: session established for {}", session.apple_id);

        let account_id = accounts::account_id_for(accounts::BackendKind::ICloud, &session.apple_id);

        // [3] The ADP gate. Nothing has been written yet and nothing will be
        // if this comes back blocked.
        let cookies = icloud_auth::cookie_source(&app);
        // The SAME cache `vertical_for` will hand the indexing pass below, so
        // the gate's zone walk is the one that hydrates the account — one read
        // at sign-in, not two.
        // Placeholder replica id: this vertical only ever calls
        // `adp_verdict()` below (read-only), and the account row does not
        // exist yet to mint a real one from — the whole point of the ADP
        // gate is refusing before anything is persisted. A real id is minted
        // the first time a vertical is built for this account AFTER it
        // exists (`icloud_vertical_for`, via `ensure_icloud_replica_id`).
        let vertical = backend::icloud::ICloudVertical::new(
            session.clone(),
            cookies,
            account_id.clone(),
            icloud_scan_cache(&state, &account_id),
            [0u8; 16],
        );
        let verdict = vertical.adp_verdict().await;
        // The sign-in window stays open through the check above and closes
        // here, whatever the outcome. `sign_in` used to close it on success,
        // which left the check to cold-start the hidden webview and harvest
        // from a page that had not reached Apple yet — measured as a harvest
        // failing 98 ms after the session was established, reported to the
        // user as "could not read this iCloud account".
        //
        // `close_signin_window` has an Android arm (Task 4) that just closes
        // the window — no `open_session_window` hand-off, since the jar is the
        // process's on that platform and there is no hidden webview to open.
        icloud_auth::close_signin_window(&app);

        let verdict = verdict.map_err(|e| {
            format!(
                "could not read this iCloud account ({e}). The session was established, so this \
                 is a read failure rather than a sign-in one — the app log names the step."
            )
        })?;
        if let Some(reason) = verdict.blocked_reason() {
            log!("icloud_sign_in: refusing {} — {}", session.apple_id, reason);
            return Err(reason);
        }
        log!("icloud_sign_in: {} passed the ADP check ({verdict:?})", session.apple_id);

        // [4] Persist. A marker, not a secret.
        {
            let mut list = state.accounts.lock().unwrap();
            // Re-checked under the same lock the push happens under: step [1]
            // released it while the user spent minutes in Apple's flow, and a
            // second sign-in started in that window would otherwise land two
            // accounts sharing one cookie jar.
            icloud_auth::refuse_second_icloud_account(&list)?;
            if !list.iter().any(|a| a.id == account_id) {
                list.push(Account {
                    id: account_id.clone(),
                    email: session.apple_id.clone(),
                    added_at: chrono::Utc::now().to_rfc3339(),
                    notes_label: None,
                    meta_label: None,
                    llm: Default::default(),
                    backend_kind: accounts::BackendKind::ICloud,
                    root_dir: None,
                    icloud_session_established: true,
                    blocked_reason: None,
                    sync_cursor: None,
                    icloud_replica_id: None,
        status: accounts::AccountStatus::Active,
                    pending_removal: false,
                });
                accounts::save_accounts(&list)?;
            }
        }

        // Cold-start index, exactly as `add_local_account` does, so folders and
        // notes populate the cache before the user looks at the sidebar.
        index_account(account_id, state).await?;

        let _ = app.emit("oauth-success", session.apple_id.clone());
        Ok(session.apple_id)
    }
}

/// Re-establishes an iCloud session for an account that already exists,
/// without the remove-then-re-add round trip.
///
/// **Reuses [`icloud_auth::sign_in`] unchanged** — the webview flow, the
/// `/validate` poll and the visible-window UX are identical to the add path.
/// What differs is everything around it: this command takes an existing
/// `account_id` instead of minting one, never calls
/// [`icloud_auth::refuse_second_icloud_account`] (there is nothing to refuse
/// — the account already occupies the one iCloud slot this install allows),
/// and updates the existing [`Account`] row in place instead of pushing a
/// new one.
///
/// Two things [`remove_account`] tears down are deliberately left alone
/// here: `state.icloud_scans[account_id]` (invalidated, not dropped — the
/// zone-walk cache and lock are still the right ones for this account_id)
/// and the local SQLite cache (nothing about a dead cookie jar makes the
/// notes already cached wrong).
#[tauri::command]
async fn icloud_reauthenticate(account_id: String, state: State<'_, AppState>) -> Result<String, String> {
    #[cfg(not(icloud_webview))]
    {
        let _ = (&account_id, &state);
        return Err(
            "iCloud sign-in needs a desktop webview to hold the session. Reauthenticate on \
             macOS."
                .to_string(),
        );
    }
    #[cfg(icloud_webview)]
    {
        let existing_email = {
            let list = state.accounts.lock().unwrap();
            let acct = list
                .iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| format!("account {account_id} not found"))?;
            if acct.backend_kind != accounts::BackendKind::ICloud {
                return Err(format!("{account_id} is not an iCloud account"));
            }
            acct.email.clone()
        };

        let app = state.app_handle.clone();

        // Apple's flow, start to finish — identical to `icloud_sign_in`.
        let session = icloud_auth::sign_in(&app).await?;
        log!("icloud_reauthenticate: session established for {}", session.apple_id);

        // The user may have signed into a DIFFERENT Apple ID than the one
        // being repaired. Refusing here, before touching the cache or the
        // account row, is the same read-before-persist discipline
        // `icloud_sign_in` applies to a brand-new account — a mismatched
        // reauth must leave the broken account exactly as broken as it was,
        // not silently rename it out from under the user.
        let fresh_id = accounts::account_id_for(accounts::BackendKind::ICloud, &session.apple_id);
        if fresh_id != account_id {
            icloud_auth::close_signin_window(&app);
            return Err(format!(
                "You signed in as {}, but this account is {}. Sign in with {} to \
                 reauthenticate it, or remove it and add {} as a separate account.",
                session.apple_id, existing_email, existing_email, session.apple_id
            ));
        }

        // The stale pre-reauth session/scan are worse than nothing to build
        // on — the whole reason this command exists is that they stopped
        // working. Explicit refresh always invalidates first (the ⟳ button's
        // rule, `AccountCache::invalidate`'s doc comment), and a reauth is
        // exactly that kind of explicit action.
        let cache = icloud_scan_cache(&state, &account_id);
        cache.invalidate().await;

        // The ADP gate, same as sign-in: re-confirm the account is still
        // readable before declaring it fixed. Advanced Data Protection can
        // have been switched on in the interim.
        let cookies = icloud_auth::cookie_source(&app);
        // The account row already exists (this is a reauth, not a fresh
        // sign-in), so a real replica id is available — same lazy-mint path
        // `icloud_vertical_for` uses.
        let replica_id = ensure_icloud_replica_id(&state, &account_id).unwrap_or([0u8; 16]);
        let vertical = backend::icloud::ICloudVertical::new(
            session.clone(), cookies, account_id.clone(), cache, replica_id,
        );
        let verdict = vertical.adp_verdict().await;
        icloud_auth::close_signin_window(&app);

        let verdict = verdict.map_err(|e| {
            format!(
                "could not read this iCloud account after reauthenticating ({e}). The session \
                 was established, so this is a read failure rather than a sign-in one."
            )
        })?;
        if let Some(reason) = verdict.blocked_reason() {
            log!(
                "icloud_reauthenticate: {} re-signed in but is blocked — {}",
                session.apple_id,
                reason
            );
            return Err(reason);
        }
        log!("icloud_reauthenticate: {} passed the ADP check ({verdict:?})", session.apple_id);

        {
            let mut list = state.accounts.lock().unwrap();
            let Some(acct) = list.iter_mut().find(|a| a.id == account_id) else {
                // Removed while the sign-in window was open. Nothing to flip.
                return Err(format!("{account_id} was removed while reauthenticating"));
            };
            acct.icloud_session_established = true;
            accounts::save_accounts(&list)?;
        }

        let _ = app.emit("oauth-success", session.apple_id.clone());
        Ok(session.apple_id)
    }
}

#[tauri::command]
async fn add_local_account(
    path: String,
    name: Option<String>,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    #[cfg(target_os = "android")]
    return Err("Local vaults need arbitrary filesystem access, which Android does not provide.".to_string());

    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(format!("not a directory: {}", path));
    }
    let basename = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    // Use the provided name (trimmed, non-empty) or fall back to the folder basename.
    let name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(basename);
    let id = localfs_account_id();
    let account = accounts::Account {
        id: id.clone(),
        email: name,
        added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None,
        meta_label: None,
        llm: accounts::LlmConfig::default(),
        backend_kind: accounts::BackendKind::LocalFs,
        root_dir: Some(path.clone()),
        icloud_session_established: false,
                blocked_reason: None,
        sync_cursor: None,
        icloud_replica_id: None,
        status: accounts::AccountStatus::Active,
        pending_removal: false,
    };
    {
        let mut list = state.accounts.lock().unwrap();
        // Guard against adding the same directory twice.
        if list.iter().any(|a| a.root_dir.as_deref() == Some(path.as_str())) {
            return Err(format!("a local account for {} already exists", path));
        }
        // Guard against a duplicate vault name (case-insensitive). Names must be
        // unique among local vaults so they stay distinguishable in the UI —
        // every spot shows `localfs:<name>`, so two same-named vaults would be
        // indistinguishable. (No need to compare against Gmail: the `localfs:`
        // prefix already separates the namespaces.)
        if list.iter().any(|a| {
            a.backend_kind == accounts::BackendKind::LocalFs
                && a.email.eq_ignore_ascii_case(&account.email)
        }) {
            return Err(format!(
                "a local vault named \"{}\" already exists — choose a different name",
                account.email
            ));
        }
        list.push(account.clone());
        accounts::save_accounts(&list)?;
    }
    // Cold-start index this account so its folders/notes populate the cache.
    // index_account guards the Gmail-specific token/label_map steps for LocalFs.
    index_account(id, state).await?;
    Ok(account)
}

/// The `.setup()` hook must read `accounts.json` **after** the database is
/// opened, never before.
///
/// `Db::open` runs migration #19, which rewrites `accounts.json` itself from
/// bare emails to `{backend}:{email}`. A list loaded before that call is stale
/// for the entire session — and nothing re-reads the file, because
/// `list_accounts` returns `AppState.accounts` verbatim. The damage is silent
/// and total: every command carries a bare `account_id` and reads zero rows,
/// `index_account` re-inserts the whole cache under the bare id, the worker
/// skips every pre-existing dirty note (so `has_pending_pushes` reports false
/// while unsent edits sit in the cache — gotcha #2's guarantee), and the first
/// of the eight `save_accounts` call sites writes the bare list back over the
/// qualified file, permanently splitting the install.
///
/// This is gotcha #6 in its migration form: state changed underneath the
/// in-memory copy with no route back. A happy-path test cannot catch it —
/// on a fresh profile the file is already qualified and the ordering is
/// invisible — so the ordering itself is what gets pinned, in the same
/// source-scanning shape as `paths.rs`'s
/// `setup_hook_uses_the_unsuffixed_dirs_not_the_bundle_id_ones`.
///
/// Comments are stripped first, exactly as that test does and for the same
/// reason: the call site is wrapped in comments that name both
/// `load_accounts()` and `Db::open`, so an unfiltered scan anchors on the
/// prose instead of the code.
#[cfg(test)]
mod setup_account_load_ordering_tests {
    fn setup_hook_code() -> String {
        let src = include_str!("lib.rs");
        // Assembled at compile time rather than written out, because this
        // scan reads the very file it lives in: a literal here would BE the
        // first match, and the slice would start at this line instead of at
        // the hook (measured — it silently swallowed the whole test module,
        // which then satisfied the assertions with its own source text).
        let needle = concat!(".setup(", "|app|");
        let setup = src
            .split_once(needle)
            .expect("lib.rs must still have a .setup hook")
            .1;
        setup
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn setup_loads_accounts_after_the_database_is_opened() {
        let code = setup_hook_code();
        let open_at = code
            .find("db::Db::open(")
            .expect("setup must open the cache with db::Db::open");
        let load_at = code
            .find("accounts::load_accounts()")
            .expect("setup must load the account list");
        assert!(
            load_at > open_at,
            "the setup hook reads accounts.json BEFORE db::Db::open runs migration #19, \
             which rewrites that same file — the whole first session after upgrade would \
             then run on stale bare account ids"
        );
    }

    /// Migration #19's fail-closed abort must not be handled by the temp-dir
    /// fallback. That fallback is right for "the cache is unusable"; this
    /// error means the cache is intact and was *not* migrated, and answering
    /// it with a fresh empty database shows the user zero notes and zero
    /// folders on every launch with one line in the log. Source-scanned for
    /// the same reason as the ordering test above: `.setup()` needs a live
    /// `AppHandle` no unit test can build.
    #[test]
    fn setup_surfaces_a_refused_account_id_migration_instead_of_an_empty_cache() {
        let code = setup_hook_code();
        let arm = code
            .find("DbOpenError::AccountIdMigration")
            .expect("setup must handle a refused account-id migration explicitly");
        let rest = &code[arm..];
        let next_arm = rest
            .find("Err(e) =>")
            .expect("the generic DbOpenError handler must still follow");
        assert!(
            rest[..next_arm].contains("return Err("),
            "the AccountIdMigration arm must fail the launch with the reason, not fall \
             through to the temp-dir cache that hides the user's intact database"
        );
    }

    #[test]
    fn setup_loads_the_account_list_exactly_once() {
        // A second, earlier read would defeat the ordering above: whichever
        // list reaches `app.manage` is the one the session runs on, and two
        // reads make that a question of which line wins rather than a fact.
        let code = setup_hook_code();
        assert_eq!(
            code.matches("accounts::load_accounts()").count(),
            1,
            "the setup hook must read accounts.json exactly once, after Db::open"
        );
    }
}

/// Pins the contract every account-creation site (`complete_oauth`,
/// `migrate_legacy_keychain`, `add_local_account`) relies on: `account_id_for`
/// mints a `{backend}:{email}` id, and two backends sharing the same email
/// produce different ids. Task 1 already supplies `account_id_for` and
/// `is_qualified`, so these pass without any new production code — they are
/// a contract pin for the call sites above, not a RED/GREEN pair.
#[cfg(test)]
mod account_creation_id_tests {
    #[test]
    fn a_localfs_id_is_minted_from_a_uuid_and_is_already_qualified() {
        let id = super::localfs_account_id();
        assert!(
            crate::accounts::is_qualified(&id),
            "a LocalFs id must be born qualified, or migration 19 would try to \
             rewrite it: {id}"
        );
        let suffix = id
            .strip_prefix("localfs:")
            .expect("a LocalFs id must carry the localfs prefix");
        assert!(
            uuid::Uuid::parse_str(suffix).is_ok(),
            "the suffix must be a UUID, not the vault's display name. If this \
             ever becomes a name, a bare LocalFs id reaching migration 19 mints \
             localfs:{{display name}} from Account.email — the assumption that \
             makes that unreachable is exactly what this pins. Got: {suffix}"
        );
    }

    #[test]
    fn a_new_account_is_created_with_a_qualified_id() {
        let id = crate::accounts::account_id_for(crate::accounts::BackendKind::Gmail, "a@b.com");
        assert_eq!(id, "gmail:a@b.com");
        assert!(crate::accounts::is_qualified(&id));
    }

    #[test]
    fn two_accounts_may_share_an_email_across_backends() {
        let g = crate::accounts::account_id_for(crate::accounts::BackendKind::Gmail, "x@y.com");
        let m = crate::accounts::account_id_for(crate::accounts::BackendKind::Microsoft, "x@y.com");
        assert_ne!(g, m, "the whole point of this change");
    }
}

/// Rename a LocalFs account's vault display name (stored in the `email` field).
///
/// Only meaningful for LocalFs accounts (the email field is the display name there).
/// Saves accounts.json and returns the updated Account so the frontend can
/// update its store without a separate list_accounts call.
#[tauri::command]
async fn rename_local_account(
    account_id: String,
    name: String,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    #[cfg(target_os = "android")]
    return Err("Local vaults need arbitrary filesystem access, which Android does not provide.".to_string());

    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("vault name must not be empty".to_string());
    }
    let updated = {
        let mut list = state.accounts.lock().unwrap();
        // Reject a name already used by ANOTHER local vault (case-insensitive),
        // so vaults stay distinguishable (every spot shows `localfs:<name>`).
        if list.iter().any(|a| {
            a.id != account_id
                && a.backend_kind == accounts::BackendKind::LocalFs
                && a.email.eq_ignore_ascii_case(&name)
        }) {
            return Err(format!("a local vault named \"{}\" already exists", name));
        }
        // Find and mutate in place, drop the mutable borrow, then save.
        {
            let acct = list
                .iter_mut()
                .find(|a| a.id == account_id)
                .ok_or_else(|| format!("account {} not found", account_id))?;
            if acct.backend_kind != accounts::BackendKind::LocalFs {
                return Err(format!("rename_local_account: {} is not a LocalFs account", account_id));
            }
            acct.email = name;
        } // mutable borrow of `acct` ends here
        accounts::save_accounts(&list)?;
        list.iter()
            .find(|a| a.id == account_id)
            .cloned()
            .unwrap()
    };
    Ok(updated)
}

/// Re-arm every note in one account whose push was refused permanently, so the
/// worker picks them up on its next tick. Returns how many were re-armed.
///
/// The user-driven half of the block's lifecycle. A block is not a verdict on
/// the note, it is a verdict on one attempt against one backend state — the
/// Notes folder gains an id, an account is re-authorised, a folder is created
/// on the iPhone — and none of those produce an event Jodd can hook, so the
/// user pressing "Try again" is the signal. Cheap and safe to spam: if the
/// cause is unchanged the worker re-blocks the row on its very next tick with
/// the current reason.
#[tauri::command]
fn retry_blocked_pushes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let n = state
        .db
        .clear_push_blocks(&account_id)
        .map_err(|e| format!("retry_blocked_pushes: {e}"))?;
    log!("retry_blocked_pushes: re-armed {} blocked note(s) for {}", n, account_id);
    Ok(n)
}

/// Outstanding pushes for one account. Drives the Inactive group's "N left"
/// line and the Stop-waiting confirmation. No side effects.
#[tauri::command]
fn count_pending_pushes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<db::PendingPushes, String> {
    state
        .db
        .count_pending_pushes(&account_id)
        .map_err(|e| format!("count_pending_pushes: {e}"))
}

/// Move an account through its lifecycle. Rejects transitions the UI does not
/// offer rather than trusting the caller.
#[tauri::command]
fn set_account_status(
    account_id: String,
    status: accounts::AccountStatus,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<accounts::Account, String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    let updated = {
        let mut list = state.accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("Account not found: {}", account_id))?;
        if !transition_allowed(acct.status, status) {
            return Err(format!(
                "cannot move {} from {:?} to {:?}",
                account_id, acct.status, status
            ));
        }
        acct.status = status;
        let snap = acct.clone();
        accounts::save_accounts(&list)?;
        snap
    };
    log!("set_account_status: {} -> {:?}", account_id, status);
    Ok(updated)
}

/// Pure readiness decision (no I/O). An account is usable when we have a local
/// cache to serve OR credentials we could refresh — neither requires network.
/// This is the heart of design principle 5 ("readiness ≠ network").
fn account_is_usable(has_local_cache: bool, has_refreshable_creds: bool) -> bool {
    has_local_cache || has_refreshable_creds
}

#[tauri::command]
async fn is_authenticated(state: State<'_, AppState>) -> Result<bool, String> {
    // "Authenticated" means at least one account is USABLE — readiness ≠ network.
    // Previously this refreshed each account's access token here, which blocked
    // the whole app behind a Gmail round-trip on a cold start while offline
    // (in-memory tokens are empty on launch, so it always hit the network).
    // Now we only do local presence checks: a cached account stays reachable
    // offline, and a present-but-revoked token surfaces a soft re-auth on the
    // first sync attempt (handleAuthLoss), instead of locking the user out.
    let accts: Vec<accounts::Account> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    if accts.is_empty() {
        log!("is_authenticated: no accounts in store → false");
        return Ok(false);
    }
    for acct in &accts {
        // Cheap side first, and this ordering is load-bearing, not tidiness.
        // `account_is_usable` is an OR, so a cached account is usable whatever
        // the credential store says — asking it anyway was a guaranteed OS
        // credential-store read for an answer already decided. This command is
        // polled every 2 s during sign-in, which turned that waste into one
        // keychain read per tick (a dialog per tick under macOS's plain
        // "Allow"). has_cached_notes is a SELECT EXISTS on an already-open
        // connection; is_ready_local reaches the OS credential store.
        let has_cache = state.db.has_cached_notes(&acct.id).unwrap_or_else(|e| {
            log!("is_authenticated: has_cached_notes({}) failed: {} — treating as no cache", acct.id, e);
            false
        });
        // None = never consulted, because the cache already settled it.
        // is_ready_local() dispatches per BackendKind — Gmail/Microsoft check the
        // keychain for a refresh token; LocalFs checks that root_dir exists on
        // disk. Neither touches the network (data doctrine: readiness ≠ network).
        let creds: Option<bool> = if has_cache {
            None
        } else {
            Some(acct.is_ready_local())
        };
        if account_is_usable(has_cache, creds.unwrap_or(false)) {
            // Report an unevaluated credential check as "not checked", never as
            // "false". These lines are the primary diagnostic for auth problems,
            // and "creds=false" next to a usable account would describe a
            // missing token that was never looked for.
            log!(
                "is_authenticated: {} usable (cache={}, creds={}) → true",
                acct.id,
                has_cache,
                creds.map_or("not checked".to_string(), |c| c.to_string())
            );
            return Ok(true);
        }
    }
    log!("is_authenticated: no accounts usable (no cache, no creds) → false");
    Ok(false)
}

// ─── Operational commands (per-account) ──────────────────────────────────────

// Build the "by Gmail message id" cache map that gmail::list_notes uses to
// skip messages.get for already-hydrated notes. Filters out rows that don't
// have a remote id yet (local-new pending push) — we have nothing to match
// them against in the Gmail response.
fn cache_by_msg_id(state: &State<'_, AppState>, account_id: &str) -> HashMap<String, gmail::Note> {
    match state.db.list_notes(account_id) {
        Ok(rows) => rows
            .into_iter()
            .filter(|c| !c.id.is_empty())
            .map(|c| (c.id.clone(), c.to_frontend_note()))
            .collect(),
        Err(e) => {
            log!("cache_by_msg_id failed for {}: {}", account_id, e);
            HashMap::new()
        }
    }
}

/// Append the account's permanently-blocked notes to a remote-derived listing,
/// skipping any the remote already returned.
///
/// Both list paths build their result out of what the backend handed back, so
/// a note whose CREATE was refused is missing from both by construction — it
/// has no remote object to be returned. The frontend then treats "absent from
/// the fetch" as "deleted elsewhere" and drops it from the store after a 30 s
/// grace window (`loadFolderNotes`, App.svelte), so a note the toolbar had
/// just called "Saved" silently left the list while its row sat in SQLite
/// being retried forever. Measured 2026-08-17: `list_cached_notes` returned 2
/// for the account while `list_notes` returned 0, three polls in a row.
///
/// Filtered by `label` when the caller is folder-scoped, because
/// `prune_clean_in_label` and the frontend's per-folder replace both key off
/// the folder — handing a folder view a note from a different folder would put
/// it in the wrong place, not merely show it twice.
///
/// Deliberately additive and last: reconciliation, pruning and local_version
/// restamping have all already run over the remote rows by the time this is
/// called, and a blocked row must not participate in any of them (it has no
/// remote counterpart to reconcile against).
///
/// Takes `&Db` rather than the Tauri `State` for the same reason
/// `push_one_dirty_db` is split out — it is the whole of the behaviour, and a
/// `State` cannot be built in a unit test.
fn append_blocked_notes(
    db: &db::Db,
    account_id: &str,
    label: Option<&str>,
    result: &mut Vec<gmail::Note>,
) {
    let blocked = match db.list_push_blocked(account_id) {
        Ok(rows) => rows,
        Err(e) => {
            log!("append_blocked_notes: list_push_blocked failed for {}: {}", account_id, e);
            return;
        }
    };
    let present: std::collections::HashSet<String> =
        result.iter().map(|n| n.uuid.clone()).collect();
    let mut added = 0usize;
    for row in blocked {
        if present.contains(&row.uuid) {
            continue;
        }
        if label.is_some_and(|l| row.label != l) {
            continue;
        }
        result.push(row.to_frontend_note());
        added += 1;
    }
    if added > 0 {
        log!(
            "append_blocked_notes: surfaced {} unsendable note(s) for {} that the remote \
             has never seen",
            added, account_id
        );
    }
}

#[tauri::command]
async fn list_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    log!("list_notes: invoked for account {}", account_id);

    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };

    // **This command IS the refresh** — the ⟳ button, the 10-minute full poll,
    // and the authoritative pull that prunes clean rows. On iCloud the zone
    // read is shared across vertical instances so a folder sweep does not walk
    // the whole account once per folder; that sharing has to stop here, or the
    // one action whose entire purpose is "go and look again" would be answered
    // out of a cache. Every other backend re-fetches by construction.
    if backend_kind == accounts::BackendKind::ICloud {
        icloud_scan_cache(&state, &account_id).invalidate().await;
    }

    // ── Backend-specific: obtain notes + vertical ─────────────────────
    // Gmail: ensure token, get label_map, build vertical via parts, run
    //        list_all_notes, then self-heal if empty (stale label cache).
    // Microsoft: vertical_for handles the token; there is no label map and no
    //        enumerable folder tree, so folders come back through the vertical.
    // LocalFs: no token/keychain — vertical_for resolves root_dir and
    //          the filesystem vertical handles list_all_notes directly.
    //
    // Dispatched on `folder_source_kind(backend_kind)` rather than on
    // `backend_kind` directly, so the mapping from backend to reconciliation
    // helper lives in exactly one testable place — and so adding a fourth
    // backend fails to compile here until that mapping is stated.
    let (result, dedup, v, folder_source) = match folder_source_kind(backend_kind) {
        FolderSourceKind::Labels => {
            let token = ensure_token(&state, &account_id).await?;
            let label_map = cached_label_map(&state, &account_id, &token).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let v = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
            let (mut result, mut dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;

            // Self-heal: stale label cache after Apple Notes recreates labels.
            if result.is_empty() {
                log!("list_notes: zero results — checking if label cache is stale");
                if let Ok(fresh) = gmail::get_label_map(&token).await {
                    let changed = fresh != label_map;
                    {
                        let mut states = state.account_states.lock().unwrap();
                        let entry = states.entry(account_id.clone()).or_default();
                        entry.label_map_cache = Some((fresh.clone(), std::time::Instant::now()));
                    }
                    if changed {
                        log!("list_notes: label map changed, retrying");
                        let v2 = vertical_from_parts(&state, &account_id, token.clone(), fresh.clone())?;
                        let (notes, fresh_dedup) = v2.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
                        result = notes;
                        dedup = fresh_dedup;
                    }
                }
            }

            // Build the vertical again (v was consumed above) for the common tail.
            // vertical_from_parts is cheap — no network, just wraps the parts.
            let v_tail = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
            // `reconcile_folders_from_labels` re-derives the Notes/* paths from
            // the label map itself, so only the map travels to the tail.
            (result, dedup, v_tail, FolderSource::Labels(label_map))
        }
        FolderSourceKind::Vertical => {
            // Exchange: `vertical_for` does the token work, there is no label
            // map, and the folder list is only obtainable from the vertical — so
            // the tail reconciles through it rather than through a path list,
            // which would throw away the ids (gotcha #12).
            let v = vertical_for(&state, &account_id).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let (result, dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
            (result, dedup, v, FolderSource::Vertical)
        }
        FolderSourceKind::Paths => {
            // LocalFs: no token or label_map needed.
            let v = vertical_for(&state, &account_id).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let (result, dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
            // Collect folder paths from the filesystem for reconciliation.
            let fs_folders: Vec<String> = v.list_folders().await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path)
                .collect();
            (result, dedup, v, FolderSource::Paths(fs_folders))
        }
    };
    // The branch that fetched the notes must have built the payload the chosen
    // helper consumes — a mismatch would silently reconcile from the wrong
    // source. Cheap enough to assert on every call rather than only in tests.
    debug_assert_eq!(folder_source.kind(), folder_source_kind(backend_kind));

    // Surface the dedup summary so the sidebar can show a passive "N dup"
    // indicator. Replace (not accumulate) — each list_notes call is a
    // complete observation; after cleanup_orphans runs the next call will
    // report fewer duplicates.
    state.dup_stats.lock().unwrap().insert(account_id.clone(), dedup);

    // Tag each note with its account so the frontend can scope folder views.
    let mut result = result;
    for n in &mut result {
        n.account_id = Some(account_id.clone());
    }

    // Reconcile each fetched note against the cache. reconcile_one handles
    // the full state machine: insert fresh on unknown uuid, refresh clean,
    // detect conflicts when both sides changed, leave deletion-pending
    // alone, etc. See reconcile_one comments for the full decision table.
    //
    // After the per-row pass, prune clean cache rows whose uuid didn't
    // come back from remote — those notes are gone. Only safe here
    // (full sweep), not in list_notes_in_folder (scoped fetch).
    {
        for n in &result {
            reconcile_one(&state, &account_id, n);
        }
        // The other half of the pin trace. `list_cached_notes` reports what the
        // cache holds when the list paints; this reports what the fetch carried
        // and what survived reconcile. Silence from this line means list_notes
        // did not run at all, which is itself the answer.
        log!(
            "list_notes: {} reconciled {} note(s), {} pinned on the wire",
            account_id,
            result.len(),
            result.iter().filter(|n| n.pinned).count()
        );
        // Restamp local_version from the post-reconcile DB row. Every note
        // in `result` was parsed off the wire (Gmail/LocalFs) with
        // local_version: 0 baked in — not the row's real value, since
        // upsert_from_remote's ON CONFLICT DO UPDATE never touches
        // local_version. A cache MISS (remote id rotated — another device
        // pushed, or this device's own worker pushed mid-listing) means a
        // note with a real nonzero local_version still gets fresh-parsed
        // with 0 here; without this restamp that 0 reaches the frontend and
        // every subsequent save for that note CASes against a stale
        // expected value, producing a permanent false conflict.
        for n in &mut result {
            if let Ok(Some(cached)) = state.db.get(&n.uuid, &account_id) {
                n.local_version = cached.local_version;
            }
        }
        let keep: Vec<String> = result.iter().map(|n| n.uuid.clone()).collect();
        // The count that separates "the fetch lost notes" from "the cache did".
        // A live iCloud account fetched 776 and cached 188; without this line
        // the difference is invisible from the log.
        match state.db.list_notes(&account_id) {
            Ok(rows) => log!(
                "list_notes: {} note(s) fetched, {} row(s) in the cache after reconcile",
                result.len(),
                rows.len()
            ),
            Err(e) => log!("list_notes: could not count cached rows: {e}"),
        }
        match state.db.prune_clean(&account_id, &keep) {
            Ok(n) if n > 0 => log!("list_notes: pruned {} clean cache row(s) no longer on remote", n),
            Ok(_) => {}
            Err(e) => log!("list_notes: prune failed: {}", e),
        }
        // Tags are keyed by uuid; once a note is pruned its tag rows would
        // be orphans. Pre-tombstone behaviour was to hard-delete them, which
        // races with Gmail's eventual consistency: a transient omission in
        // one listing would silently destroy the user's tags. Now we move
        // them to `tag_tombstones` so a note that reappears on the next
        // sweep gets its tags restored automatically (via the restore step
        // inside upsert_from_remote). Tombstones older than TOMBSTONE_TTL_MS
        // are dropped here too — at that age the disappearance is real.
        match state.db.tombstone_orphan_tags(&account_id) {
            Ok(n) if n > 0 => log!("list_notes: tombstoned {} tag row(s) for pruned notes", n),
            Ok(_) => {}
            Err(e) => log!("list_notes: tombstone orphan tags failed: {}", e),
        }
        match state.db.sweep_old_tombstones(&account_id, TOMBSTONE_TTL_MS) {
            Ok(n) if n > 0 => log!("list_notes: swept {} expired tag tombstone(s)", n),
            Ok(_) => {}
            Err(e) => log!("list_notes: sweep tombstones failed: {}", e),
        }
    }

    // ── Pin sidecar pull reconciliation ────────────────────────────────
    //
    // List every pin sidecar via the vertical (which resolves meta_label
    // internally, returning an empty vec if the label/dir isn't present
    // yet), apply each to the cache via apply_remote_pin, then clear pin
    // on any locally-pinned row whose uuid didn't appear in the listing.
    //
    // Skipped silently on errors so a transient backend glitch doesn't
    // break the entire list_notes path — pin sync is UX-only.
    {
        match v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string()) {
            Ok(Some(sidecars)) => {
                let mut keep: Vec<String> = Vec::with_capacity(sidecars.len());
                for s in &sidecars {
                    // Existence == pinned (see the SIDECAR doc in gmail.rs).
                    let _ = state.db.apply_remote_pin(
                        &s.note_uuid, &account_id, true, &s.id,
                    );
                    keep.push(s.note_uuid.clone());
                }
                match state.db.clear_pins_not_in(&account_id, &keep) {
                    Ok(n) if n > 0 => log!(
                        "list_notes: cleared {} pin(s) absent from meta_label",
                        n
                    ),
                    Ok(_) => {}
                    Err(e) => log!("list_notes: clear_pins_not_in failed: {}", e),
                }
            }
            Ok(None) => { /* meta store not present yet — skip; do NOT clear pins */ }
            Err(e) => log!("list_notes: list_meta_sidecars failed: {}", e),
        }
    }

    // ── Folder pull reconciliation ─────────────────────────────────────
    // Gmail: label_map is authoritative — upsert + prune via label helper.
    // LocalFs: filesystem folder list is authoritative — upsert + prune
    //          via path helper (filesystem is strongly-consistent, so
    //          pruning is safe unlike Gmail cold-start).
    // Microsoft: the vertical's message-derived folder list is the only one
    //          there is, and it is the only carrier of the folder ids.
    match folder_source {
        FolderSource::Labels(label_map) => {
            reconcile_folders_from_labels(&state.db, &account_id, &label_map, true);
        }
        FolderSource::Paths(folder_paths) => {
            reconcile_folders_from_paths(&state.db, &account_id, &folder_paths, true);
        }
        FolderSource::Vertical => {
            // `prune = true` here is a deliberate ruling, not an oversight, and
            // it has a user-visible cost worth stating plainly: **a Microsoft
            // folder row is deleted the moment its last note leaves it, so a
            // folder the user emptied vanishes from their sidebar** — which
            // reads like a sync bug.
            //
            // Do not "fix" this by flipping it to false. On this backend a
            // folder is observable ONLY through a note that points at it
            // (gotcha #12: `/me/mailFolders` omits the Notes tree, `GET
            // /mailFolders/{id}` 404s, and `childFolders` always returns
            // empty). Graph therefore cannot distinguish "folder emptied" from
            // "folder deleted" — both look identical from here. Pruning matches
            // exactly what is observable, and the emptied-folder case
            // self-corrects the instant a note is filed there again. A retained
            // stale row would never self-correct, because nothing would ever
            // arrive to contradict it.
            //
            // Best-effort, like the two above: a folder-scan failure must not
            // fail the whole pull, whose notes have already been reconciled.
            if let Err(e) = reconcile_folders_from_vertical(&state.db, &account_id, &*v, true).await {
                log!("list_notes: folder reconcile failed for {}: {}", account_id, e);
            }
        }
    }

    // D8 fix: drop any uuid the cache says is deleted_pending. Gmail's
    // search index can lag the worker's trash calls by a few seconds; in
    // that window the user just told us to delete a note but Gmail still
    // returns it. Without this filter the frontend merge re-introduces
    // it as a "ghost" entry in $notes — SQLite says gone, UI shows it.
    //
    // Cheap: one indexed SELECT against the partial sync_state index.
    // Must run AFTER reconcile_one + prune_clean, because those operate
    // on the raw fetch result. The filter only shapes what we return to
    // the frontend.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if !deleted.is_empty() {
            let drop: std::collections::HashSet<String> = deleted.into_iter().collect();
            let before = result.len();
            result.retain(|n| !drop.contains(&n.uuid));
            let dropped = before - result.len();
            if dropped > 0 {
                log!("list_notes: filtered {} ghost(s) from Gmail fetch", dropped);
            }
        }
    }

    // AFTER the ghost filter, not before: that filter drops rows the user has
    // locally deleted, and a blocked note is the opposite case — present
    // locally, absent remotely, and wanted.
    append_blocked_notes(&state.db, &account_id, None, &mut result);

    log!(
        "list_notes: returning {} notes for {}",
        result.len(),
        account_id
    );
    Ok(result)
}

/// A local, account-scoped snapshot; a successful SQLite save is not a push.
#[derive(serde::Serialize)]
struct NotePersistence {
    account_id: String,
    uuid: String,
    local_version: i64,
    sync_state: db::SyncState,
    push_blocked_reason: Option<String>,
}

fn note_persistence_db(db: &db::Db, account_id: &str, uuid: &str) -> Result<Option<NotePersistence>, String> {
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| e.to_string())?;
    Ok(db.get(&uuid, account_id).map_err(|e| e.to_string())?.map(|n| NotePersistence {
        account_id: n.account_id, uuid: n.uuid, local_version: n.local_version,
        // Pin sidecars have a separate dirty bit and do not bump the content
        // version. A clean body alone must not claim the entire note synced.
        sync_state: if n.pin_dirty && n.sync_state == db::SyncState::Clean { db::SyncState::Dirty } else { n.sync_state },
        push_blocked_reason: n.push_blocked_reason,
    }))
}

/// Local-first delete. Marks the row `deleted_pending` so the frontend
/// stops showing it, then the background worker handles the Gmail trash
/// call. If the note was a brand-new local-only note (no remote_version
/// yet), the worker just removes the row — no Gmail call needed.
///
/// The frontend can still pass `id` (Gmail message id) as a fallback for
/// rows we haven't yet seen in the cache (e.g. a list-pane click on a note
/// from a freshly-fetched but uncached account). In that case we trash
/// directly. New code paths should prefer passing `uuid`.
#[tauri::command]
async fn delete_note(
    account_id: String,
    id: Option<String>,
    uuid: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let db = state.db.clone();
    if let Some(uuid) = uuid.as_deref().filter(|s| !s.is_empty() && !s.starts_with("tmp:")) {
        db.mark_deleted(uuid, &account_id).map_err(|e| e.to_string())?;
        log!("delete_note: marked deleted_pending for uuid={}", uuid);
        return Ok(());
    }
    // Fallback: trash directly by Gmail id. Used during migration period.
    if let Some(id) = id.as_deref().filter(|s| !s.is_empty()) {
        let v = vertical_for(&state, &account_id).await?;
        v.delete(id).await.map_err(|e| e.to_string())?;
        return Ok(());
    }
    Err("delete_note: neither uuid nor id provided".into())
}

/// Batch move — relabels every uuid in `uuids` to `target_label` in one
/// SQLite transaction. Each touched row goes dirty (or conflict, per the
/// state machine) and the sync worker pushes the moves to Gmail on its
/// next ticks. Returns the count of rows actually updated.
///
/// Why a batch primitive instead of looping save_note N times: the loop
/// shape lets the user see partial states (3 of 7 notes moved) while the
/// IPC awaits queued behind each other, and serializes the SQLite writes
/// one Mutex acquisition per note. The batch is atomic — either every
/// row's label moves or none does — and acquires the connection Mutex
/// once.
#[tauri::command]
async fn move_notes_batch(
    account_id: String,
    uuids: Vec<String>,
    target_label: String,
    ai_result_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let _policy_gate = ai_result_id.as_ref().map(|_| state.ai_policy.gate.lock().unwrap());
    if let Some(id) = &ai_result_id { validate_ai_result(&state, &account_id, id)?; }
    let touched = state.db
        .move_notes_batch(&account_id, &uuids, &target_label)
        .map_err(|e| e.to_string())?;
    log!(
        "move_notes_batch: account={} touched={}/{} target='{}'",
        account_id, touched, uuids.len(), target_label
    );
    Ok(touched)
}

/// Batch delete — marks every uuid in `uuids` as `deleted_pending` in one
/// SQLite transaction. The sync worker trashes them on Gmail in the
/// background. Same atomicity argument as move_notes_batch.
#[tauri::command]
async fn delete_notes_batch(
    account_id: String,
    uuids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let touched = state.db
        .delete_notes_batch(&account_id, &uuids)
        .map_err(|e| e.to_string())?;
    log!(
        "delete_notes_batch: account={} touched={}/{}",
        account_id, touched, uuids.len()
    );
    Ok(touched)
}

/// Toggle the pin column on one note. Pure local-first: a single SQLite
/// UPDATE, no Gmail involvement, no sync_state transition. The worker
/// has nothing to push because pin doesn't round-trip through the email
/// backend (Apple stores pin in iCloud metadata Jodd can't reach via
/// Gmail). Returns immediately after the row write commits.
#[derive(serde::Serialize)]
struct AttachmentDto {
    content_id: String,
    mime_type: String,
    data_uri: String,
}

/// Return a note's attachments for the editor. `data_uri` is populated only for
/// IMAGES (what renders inline); other types (PDF/zip/…) come back as cid+mime
/// with an empty data_uri — enough for the editor to (a) leave their <object>
/// placeholder and (b) detect a stale/over-stripped body (attachments exist but
/// the body references none) and self-heal. Non-image bytes never cross IPC
/// (a 33 MB zip has no inline rendering).
#[tauri::command]
async fn get_note_attachments(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<Vec<AttachmentDto>, String> {
    let atts = state
        .db
        .list_attachments(&account_id, &uuid)
        .map_err(|e| e.to_string())?;
    Ok(atts
        .into_iter()
        .map(|a| {
            let data_uri = if a.mime_type.starts_with("image/") {
                crate::mime822::data_uri(&a.mime_type, &a.data)
            } else {
                String::new()
            };
            AttachmentDto {
                data_uri,
                content_id: a.content_id,
                mime_type: a.mime_type,
            }
        })
        .collect())
}

fn rfc2822_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc2822(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// List notes in Gmail Trash for the "Recently Deleted" view. Filters out
/// edit-revisions (a trashed message whose uuid still has a LIVE cache row is
/// just an old revision from a save's insert-new+trash-old, not a user
/// deletion), then dedups by uuid keeping the newest, newest-first.
#[tauri::command]
async fn list_trashed_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::TrashedNote>, String> {
    let v = vertical_for(&state, &account_id).await?;
    let trashed = v.list_trashed().await.map_err(|e| e.to_string())?;

    let mut by_uuid: HashMap<String, gmail::TrashedNote> = HashMap::new();
    for t in trashed {
        // A trashed message is a mere edit-revision ONLY if a DIFFERENT (live)
        // message with this uuid still exists — i.e. the cache's current id for
        // the uuid points at some OTHER message. If the cache id IS this trashed
        // id (the note's live message got trashed and the cache just hasn't
        // pruned yet), it's a GENUINE deletion → show it. Earlier this checked
        // only "row exists", which wrongly hid genuinely-deleted notes whose
        // stale cache row lingered.
        let is_revision = matches!(
            state.db.get(&t.uuid, &account_id),
            Ok(Some(row)) if row.id != t.id
        );
        if is_revision {
            continue;
        }
        match by_uuid.get(&t.uuid) {
            Some(existing) if rfc2822_ms(&existing.date) >= rfc2822_ms(&t.date) => {}
            _ => {
                by_uuid.insert(t.uuid.clone(), t);
            }
        }
    }
    // Secondary key on uuid: `by_uuid.into_values()` walks a HashMap, whose
    // iteration order is randomized per construction, so ties on `date` (e.g.
    // a batch of test notes created in the same second) would otherwise
    // reorder themselves on every refresh even though nothing changed.
    let mut out: Vec<gmail::TrashedNote> = by_uuid.into_values().collect();
    out.sort_by(|a, b| {
        rfc2822_ms(&b.date).cmp(&rfc2822_ms(&a.date)).then_with(|| a.uuid.cmp(&b.uuid))
    });
    Ok(out)
}

/// Restore a trashed note: untrash the Gmail message (its Notes label is
/// retained, so it returns to its folder) and clear any local deleted_pending
/// row so the worker doesn't re-trash it. The next list reconcile re-inserts
/// the untrashed note as clean.
/// How a backend puts a trashed note back where the user wants it.
///
/// `restore_note` used to be `if Gmail { … } else { …LocalFs… }`, which was
/// true while exactly two backends had a trash and became a live defect the
/// moment a third did: the `else` ran LocalFs's filename decode over a
/// CloudKit `recordName`. Stated per backend, exhaustively, so a fifth has to
/// answer rather than inherit somebody else's mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestoreKind {
    /// Gmail: trash is a LABEL, so untrash removes it and the note reappears
    /// under the `Notes/*` label it kept the whole time; an explicit target is
    /// a move afterwards.
    UntrashThenMove,
    /// LocalFs: the trash filename encodes the original relpath, so untrash
    /// alone puts the file back exactly where it was.
    UntrashToEncodedPath,
    /// iCloud: a note's folder reference IS its location, so being in the Trash
    /// means that reference was overwritten — there is nothing to un-delete,
    /// only somewhere to move it to. And nothing measured says where it came
    /// from, so that somewhere has to come from the user
    /// (`TrashedNote::original_known` is false here, and the UI offers only
    /// "Restore to…").
    MoveOutOfTrash,
}

/// `None` = this backend has no trash to restore from — `Capabilities::
/// has_trash` is false and the UI shows no view at all.
fn restore_kind(kind: accounts::BackendKind) -> Option<RestoreKind> {
    match kind {
        accounts::BackendKind::Gmail => Some(RestoreKind::UntrashThenMove),
        accounts::BackendKind::LocalFs => Some(RestoreKind::UntrashToEncodedPath),
        accounts::BackendKind::ICloud => Some(RestoreKind::MoveOutOfTrash),
        // Measured: an Apple-side delete leaves nothing in Deleted Items, so
        // there is genuinely nothing to restore from.
        accounts::BackendKind::Microsoft => None,
    }
}

#[tauri::command]
async fn restore_note(
    account_id: String,
    uuid: String,
    id: String,
    original_label: String,
    target_label: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };

    let Some(how) = restore_kind(backend_kind) else {
        return Err(format!(
            "{account_id} has no Recently Deleted to restore from"
        ));
    };

    if how == RestoreKind::MoveOutOfTrash {
        // One write: the record's `Folder` reference is what says where the
        // note lives, so moving it out of the Trash IS the restore. A target is
        // required rather than defaulted — see `RestoreKind::MoveOutOfTrash`.
        let target = target_label.filter(|t| !t.is_empty()).ok_or_else(|| {
            "this account cannot tell which folder the note came from — choose one to \
             restore it into"
                .to_string()
        })?;
        let v = vertical_for(&state, &account_id).await?;
        v.move_note(&id, std::slice::from_ref(&target), &[])
            .await
            .map_err(|e| e.to_string())?;
        let _ = state.db.delete(&uuid, &account_id);
        log!("restore_note[{account_id}]: RESTORED id={id} uuid={uuid} into {target}");
        return Ok(());
    }

    if how == RestoreKind::UntrashThenMove {
        let token = ensure_token(&state, &account_id).await?;
        let label_map = cached_label_map(&state, &account_id, &token).await?;
        let v = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
        v.untrash(&id).await.map_err(|e| e.to_string())?;
        let id_of = |name: &str| {
            label_map
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(lid, _)| lid.clone())
        };
        // Optionally move to a chosen folder (like a restore-as-move): remove the
        // original Notes label, add the target. The picker only offers existing
        // folders for an explicit "restore to X".
        //
        // If the note's folder was deleted while it sat in Trash, untrash alone
        // leaves the message with NO Notes-tree label at all — invisible to
        // every list/index path. `original_label` can't detect this on its own:
        // list_trashed_notes' pick_notes_label already falls back to the
        // synthetic string "Notes" when no real label matched, and "Notes" always
        // resolves in id_of (the root label exists), so comparing against
        // `original_label` here would silently miss every orphaned case. Instead
        // check the message's actual current labels (ground truth, post-untrash).
        // Propagate a fetch failure rather than defaulting has_notes_label to
        // false: a false default would wrongly force even a perfectly-intact
        // restore into the root "Notes" folder instead of leaving it in place.
        let current_labels = gmail::get_message_label_ids(&token, &id).await.map_err(|e| e.to_string())?;
        let has_notes_label = current_labels.iter().any(|lid| {
            label_map
                .get(lid)
                .is_some_and(|n| n == "Notes" || n.starts_with("Notes/"))
        });
        let effective_target = target_label
            .filter(|t| *t != original_label)
            .or_else(|| (!has_notes_label).then(|| "Notes".to_string()));
        if let Some(target) = effective_target {
            let target_id = match id_of(&target) {
                Some(tid) => tid,
                None => v.ensure_folder(&target).await.map_err(|e| e.to_string())?.id,
            };
            let remove: Vec<String> = id_of(&original_label).into_iter().collect();
            v.move_note(&id, &[target_id], &remove).await.map_err(|e| e.to_string())?;
        }
    } else {
        debug_assert_eq!(how, RestoreKind::UntrashToEncodedPath);
        // LocalFs: no token/label_map needed. untrash decodes the trash
        // filename back to the original relpath and restores the file there,
        // recreating the subfolder if needed.
        let v = vertical_for(&state, &account_id).await?;
        v.untrash(&id).await.map_err(|e| e.to_string())?;

        // After untrash the note lives at its original relpath
        // (trash_decode of the trash filename == original relpath).
        let encoded_basename = std::path::Path::new(&id)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let restored_id =
            crate::backend::localfs::transport::trash_decode(&encoded_basename);

        if let Some(target) = target_label.filter(|t| *t != original_label) {
            // User chose a different destination folder: move there after restore.
            v.move_note(&restored_id, &[target], &[original_label.clone()])
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let _ = state.db.delete(&uuid, &account_id);
    log!("restore_note[{}]: UNTRASHED id={} uuid={} (from {})", account_id, id, uuid, original_label);
    Ok(())
}

/// Full-text search (SQLite FTS5, Thai-aware) over title + plain-text body.
/// Scope is caller-controlled: pass `account_id`+`label` to search one folder,
/// `account_id` only for one account, or neither (null) to search EVERY account.
/// Pure SQLite read. Returns full notes so the UI can show + open results that
/// aren't loaded in memory.
#[tauri::command]
fn search_notes(
    account_id: Option<String>,
    label: Option<String>,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = state
        .db
        .search_notes(account_id.as_deref(), label.as_deref(), &query, &hidden_account_ids(&state))
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

#[derive(serde::Serialize)]
struct NoteConnections {
    outgoing: Vec<gmail::Note>,
    backlinks: Vec<gmail::Note>,
}

/// Fact-schema edges consumer: a note's [[wikilink]] connections — `outgoing`
/// (notes it links to, resolved by title) and `backlinks` (notes that link to
/// it). Pure SQLite read over the `edges` table (derived from bodies on write).
#[tauri::command]
fn note_connections(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<NoteConnections, String> {
    let outgoing = state
        .db
        .outgoing_links(&account_id, &uuid)
        .map_err(|e| e.to_string())?;
    let backlinks = state
        .db
        .backlinks(&account_id, &uuid)
        .map_err(|e| e.to_string())?;
    Ok(NoteConnections {
        outgoing: outgoing.iter().map(|c| c.to_frontend_note()).collect(),
        backlinks: backlinks.iter().map(|c| c.to_frontend_note()).collect(),
    })
}

/// URLs this note cites (`rel='cites'` edges). Pure SQLite read.
#[tauri::command]
fn note_citations(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    state.db.citations(&account_id, &uuid).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
struct DuplicateCitation {
    url: String,
    existing_note_uuid: String,
    existing_note_title: String,
}

/// Pre-flight check for Extract's duplicate-source warning: scan `source_text`
/// for URLs and report every one that's already cited by another note in this
/// account (excluding `exclude_uuid`, the append target if any). Called by the
/// frontend BEFORE `extract_note`/`append_extract_note` — a non-empty
/// result means the modal should show a soft warning with a "Continue
/// anyway" resubmit, never a hard block.
#[tauri::command]
fn check_duplicate_citations(
    account_id: String,
    source_text: String,
    exclude_uuid: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DuplicateCitation>, String> {
    let mut out = Vec::new();
    for url in db::extract_urls(&source_text) {
        if let Some(owner) = state
            .db
            .find_citation_owner(&account_id, &url, exclude_uuid.as_deref())
            .map_err(|e| e.to_string())?
        {
            out.push(DuplicateCitation {
                url,
                existing_note_uuid: owner.uuid,
                existing_note_title: owner.title,
            });
        }
    }
    Ok(out)
}

#[derive(serde::Serialize)]
struct LinkCandidate {
    uuid: String,
    title: String,
    label: String,
    slug: String,
}

/// Autocomplete for the `[[` link picker: notes whose title matches `query`,
/// each with its ready-to-insert slug (note_slug = title-slug + uuid8).
#[tauri::command]
fn search_note_links(
    account_id: String,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<LinkCandidate>, String> {
    let rows = state
        .db
        .search_titles(&account_id, &query, 8)
        .map_err(|e| e.to_string())?;
    Ok(rows
        .into_iter()
        .map(|(uuid, title, label)| {
            let slug = db::note_slug(&title, &uuid);
            LinkCandidate { uuid, title, label, slug }
        })
        .collect())
}

#[tauri::command]
async fn set_pin(
    account_id: String,
    uuid: String,
    pinned: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    state.db.set_pin(&uuid, &account_id, pinned).map_err(|e| e.to_string())?;
    log!(
        "set_pin: account={} uuid={} pinned={}",
        account_id, uuid, pinned
    );
    Ok(())
}

/// Batch pin/unpin — flips the column on every uuid in one SQLite
/// transaction. Same atomicity argument as `move_notes_batch`. The
/// `pinned` flag is uniform across the batch; the menu decides which
/// direction by inspecting whether the selection is all-pinned or
/// all-unpinned before calling.
#[tauri::command]
async fn set_pin_batch(
    account_id: String,
    uuids: Vec<String>,
    pinned: bool,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let touched = state.db
        .set_pin_batch(&account_id, &uuids, pinned)
        .map_err(|e| e.to_string())?;
    log!(
        "set_pin_batch: account={} touched={}/{} pinned={}",
        account_id, touched, uuids.len(), pinned
    );
    Ok(touched)
}

// ─── Tags (Jodd-local, mirrors Pin wave 1) ───────────────────────────────────
//
// Tags live ONLY in SQLite (the note_tags table), never in the note body, so
// they never collide with `#` in URLs/code and never round-trip to Apple Notes
// (which has no tagging). Pure local-first: each command is a single SQLite
// write/read with no Gmail involvement and no worker path.

#[derive(serde::Serialize)]
struct TagCount {
    tag: String,
    count: i64,
}

#[derive(serde::Serialize)]
struct NoteTag {
    uuid: String,
    tag: String,
}

/// Canonical stored form of a tag, or None if it has no usable content.
/// Trims, lowercases, and drops whitespace, control chars, and every '#'.
/// Unicode-friendly on purpose: any letter/digit/mark survives (Thai, CJK,
/// etc.) — only structurally-bad chars are removed. Lowercasing prevents
/// `#Work`/`#work` fragmenting the tag cloud (no-op for scripts without case).
/// Must stay in lockstep with normalizeTagClient in NoteEditor.svelte so the
/// optimistic UI value equals what's stored.
fn normalize_tag(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control() && *c != '#')
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Add a tag to a note. Returns the normalized form so the frontend can
/// reconcile its optimistic value with what was actually stored.
#[tauri::command]
async fn add_tag(
    account_id: String,
    uuid: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let norm = normalize_tag(&tag).ok_or_else(|| format!("Invalid tag: {:?}", tag))?;
    state.db.add_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    log!("add_tag: account={} uuid={} tag={}", account_id, uuid, norm);
    Ok(norm)
}

/// Remove a tag from a note.
#[tauri::command]
async fn remove_tag(
    account_id: String,
    uuid: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let norm = normalize_tag(&tag).unwrap_or_else(|| tag.clone());
    state.db.remove_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    log!("remove_tag: account={} uuid={} tag={}", account_id, uuid, norm);
    Ok(())
}

/// Every tag for an account with its note count — drives the sidebar.
#[tauri::command]
async fn list_tags(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<TagCount>, String> {
    let rows = state.db.list_all_tags(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(tag, count)| TagCount { tag, count }).collect())
}

/// (uuid, tag) for every tagged note — the frontend folds this into a
/// uuid → tags[] map for rendering chips.
#[tauri::command]
async fn list_note_tags(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<NoteTag>, String> {
    let rows = state.db.list_all_note_tags(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(uuid, tag)| NoteTag { uuid, tag }).collect())
}

/// Cached notes carrying ANY of `tags` (the union). Pure local read — the
/// tag-navigation parallel of `list_cached_notes_in_folder`. The frontend
/// narrows the union to AND/OR per the active match mode, so loading the
/// union here serves either mode without a re-query on toggle.
#[tauri::command]
async fn list_cached_notes_with_tags(
    account_id: Option<String>,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let norm: Vec<String> = tags
        .iter()
        .filter_map(|t| normalize_tag(t))
        .collect();
    // account_id = None → search every account (cross-account tag filter).
    let cached = state.db
        .list_notes_with_tags(account_id.as_deref(), &norm, &hidden_account_ids(&state))
        .map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Rename a tag across every note in the account (global). Returns the
/// normalized new tag so the frontend can reconcile its optimistic value.
#[tauri::command]
async fn rename_tag(
    account_id: String,
    old_tag: String,
    new_tag: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    // Tagged Sidecars, not Notes, even though the mechanism below edits note
    // BODIES: tag identity is the sidecar concern being changed here (see
    // `add_tag`/`remove_tag`), so it must gate with the rest of the tag
    // commands. Do not "fix" this to Notes just because it touches content.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let from = normalize_tag(&old_tag).ok_or_else(|| format!("Invalid tag: {:?}", old_tag))?;
    let to = normalize_tag(&new_tag).ok_or_else(|| format!("Invalid tag: {:?}", new_tag))?;
    // Inline model: rewrites #from → #to in every carrying note's BODY, marks
    // them content-dirty (the normal content push round-trips the rename to
    // Apple), and re-derives note_tags. No tag sidecar involved.
    let n = state.db.rename_tag(&account_id, &from, &to).map_err(|e| e.to_string())?;
    log!("rename_tag: account={} '{}' -> '{}' ({} notes)", account_id, from, to, n);
    Ok(to)
}

/// Delete a tag from every note in the account (global).
#[tauri::command]
async fn delete_tag(
    account_id: String,
    tag: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    // Tagged Sidecars, not Notes, even though the mechanism below edits note
    // BODIES: tag identity is the sidecar concern being changed here (see
    // `add_tag`/`remove_tag`), so it must gate with the rest of the tag
    // commands. Do not "fix" this to Notes just because it touches content.
    refuse_write(&state, &account_id, backend::Write::Sidecars)?;
    let t = normalize_tag(&tag).unwrap_or_else(|| tag.clone());
    // Inline model: strips #tag from every carrying note's BODY + content-dirty
    // (round-trips the removal to Apple) + re-derives note_tags. No sidecar.
    let n = state.db.delete_tag(&account_id, &t).map_err(|e| e.to_string())?;
    log!("delete_tag: account={} tag={} ({} notes)", account_id, t, n);
    Ok(())
}

// ─── Folder management ──────────────────────────────────────────────────────
//
// Folders are Gmail labels under the "Notes/" hierarchy. We always prepend
// "Notes/" to user-supplied names at the command layer so callers don't have
// to think about it (and can't accidentally create a label outside Notes/).
// Cache is invalidated after every mutation so the next list_notes refetches.

fn invalidate_label_cache(state: &State<'_, AppState>, account_id: &str) {
    let mut states = state.account_states.lock().unwrap();
    if let Some(s) = states.get_mut(account_id) {
        s.label_map_cache = None;
    }
}

// Validate a single folder-name segment supplied by the user. Disallow "/"
// (would collide with hierarchy separator), empty/whitespace-only names, and
// excessively long names. Returned String is the trimmed name.
/// Storage name of the content-extraction workflow folder. Lives under Notes/
/// as `Notes/__Extracts__`. The `__name__` form is reserved syntax for
/// Jodd-managed workflow folders — see is_reserved_workflow_pattern. The
/// sidebar strips the underscore markers when displaying (so the user sees
/// "Extracts" with a 💡 icon, not `__Extracts__`).
/// No production code writes here since 2026-09-15 (extract filing); kept for
/// `ensure_workflow_folder`'s tests, including the Microsoft reconciliation
/// regression test.
pub const WORKFLOW_FOLDER_EXTRACTS: &str = "__Extracts__";

/// True for folder name segments matching the reserved `__name__` pattern —
/// Jodd's convention for system-managed workflow folders. The pattern requires
/// at least one character between the markers to avoid trivially rejecting
/// short user-typed sequences like `____` (which is unusual but legal).
fn is_reserved_workflow_pattern(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

/// Strip the `__` markers from a reserved-pattern folder name for display.
/// Returns the input unchanged if it doesn't match the pattern.
pub fn strip_workflow_markers(name: &str) -> &str {
    if is_reserved_workflow_pattern(name) {
        &name[2..name.len() - 2]
    } else {
        name
    }
}

/// App-side folder-name validation: the shared safety core in
/// [`crate::folder_label::validate_label_segment`] plus the one rule that is
/// app-only policy — the `__name__` reservation. The MCP write tools
/// deliberately do NOT inherit that rule (an agent writing into
/// `Notes/__Claude__` is the intended use), which is exactly why the two
/// halves live in different places rather than in one function with a flag.
fn validate_folder_segment(name: &str) -> Result<String, String> {
    let trimmed = crate::folder_label::validate_label_segment(name)?;
    // Reserve the __name__ pattern for Jodd-managed system workflow folders.
    // Anything matching `__*__` is off-limits to user creation — the sidebar
    // strips those markers when displaying so the user can still see a
    // "clean" name (e.g. __Extracts__ → Extracts) but the actual storage path
    // makes the system-managed status unambiguous. Users CAN still create a
    // folder named just "Extracts" (no markers) — only the underscored form
    // is reserved.
    if is_reserved_workflow_pattern(trimmed) {
        return Err(format!(
            "'{}' uses Jodd's reserved __name__ syntax for system folders. \
             Please pick a name without leading and trailing double-underscores.",
            trimmed
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod validate_folder_segment_tests {
    use super::*;

    #[test]
    fn reserved_pattern_matches() {
        assert!(is_reserved_workflow_pattern("__Extracts__"));
        assert!(is_reserved_workflow_pattern("__Foo__"));
        assert!(is_reserved_workflow_pattern("__a__"));
    }

    #[test]
    fn reserved_pattern_does_not_match_edge_cases() {
        assert!(!is_reserved_workflow_pattern("Extracts")); // no markers
        assert!(!is_reserved_workflow_pattern("__foo")); // suffix missing
        assert!(!is_reserved_workflow_pattern("foo__")); // prefix missing
        assert!(!is_reserved_workflow_pattern("____")); // empty middle
        assert!(!is_reserved_workflow_pattern("__")); // too short
        assert!(!is_reserved_workflow_pattern("")); // empty
    }

    #[test]
    fn strip_markers_when_pattern_matches() {
        assert_eq!(strip_workflow_markers("__Extracts__"), "Extracts");
        assert_eq!(strip_workflow_markers("__Foo__"), "Foo");
    }

    #[test]
    fn strip_markers_passes_through_when_not_pattern() {
        assert_eq!(strip_workflow_markers("Extracts"), "Extracts");
        assert_eq!(strip_workflow_markers("__foo"), "__foo");
        assert_eq!(strip_workflow_markers("normal-folder"), "normal-folder");
    }

    #[test]
    fn validate_rejects_reserved_pattern() {
        let err = validate_folder_segment("__Extracts__").unwrap_err();
        assert!(err.contains("__name__"), "actual: {err}");
    }

    #[test]
    fn validate_accepts_extracts_without_markers() {
        // Plain `Extracts` (no underscores) is fair game for users — only
        // the underscored form is reserved.
        assert_eq!(validate_folder_segment("Extracts").unwrap(), "Extracts");
    }

    #[test]
    fn validate_rejects_dot_and_dotdot() {
        // Prevent path-traversal: "." and ".." must be rejected so a
        // LocalFS folder path can never escape the notes root.
        let err_dot = validate_folder_segment(".").unwrap_err();
        assert!(err_dot.contains("'.'") || err_dot.contains("cannot be"), "actual: {err_dot}");
        let err_dotdot = validate_folder_segment("..").unwrap_err();
        assert!(
            err_dotdot.contains("'..'") || err_dotdot.contains("cannot be"),
            "actual: {err_dotdot}"
        );
    }
}

#[cfg(test)]
mod account_readiness_tests {
    use super::*;

    #[test]
    fn usable_with_cache_only() {
        assert!(account_is_usable(true, false));
    }

    #[test]
    fn usable_with_creds_only() {
        assert!(account_is_usable(false, true));
    }

    #[test]
    fn usable_with_both() {
        assert!(account_is_usable(true, true));
    }

    #[test]
    fn not_usable_with_neither() {
        assert!(!account_is_usable(false, false));
    }
}

// Scoped fetch: only the notes whose label is exactly `path`. Used by the
// frontend when the user has been focused on one folder long enough to
// warrant a refresh — far cheaper than fetching every Notes sub-label.
#[tauri::command]
async fn list_notes_in_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };

    let cache_map = cache_by_msg_id(&state, &account_id);
    let mut result = if backend_kind == accounts::BackendKind::Gmail {
        let token = ensure_token(&state, &account_id).await?;
        let label_map = cached_label_map(&state, &account_id, &token).await?;
        // Verify the folder exists (or is locally-pending). The vertical's
        // list_notes_in_folder returns an empty vec when the folder isn't in its
        // label_map; we want a real "Folder not found" error for folders that don't
        // exist at all, and an empty list only for locally-pending (not yet pushed).
        if !label_map.values().any(|n| n == &path) {
            if let Ok(Some(_)) = state.db.get_folder(&account_id, &path) {
                log!(
                    "list_notes_in_folder: '{}' exists locally but not on Gmail yet — returning empty",
                    path
                );
                return Ok(Vec::new());
            }
            return Err(format!("Folder not found: {}", path));
        }
        let v = vertical_from_parts(&state, &account_id, token, label_map)?;
        v.list_notes_in_folder(&path, &cache_map).await.map_err(|e| e.to_string())?
    } else {
        // LocalFs: no token/label_map — vertical handles a missing dir by returning empty.
        let v = vertical_for(&state, &account_id).await?;
        v.list_notes_in_folder(&path, &cache_map).await.map_err(|e| e.to_string())?
    };
    for n in &mut result {
        n.account_id = Some(account_id.clone());
    }
    // Same reconciliation as list_notes. The scoped prune drops clean
    // rows IN this label only (a per-folder fetch isn't authoritative
    // about other folders in the same account).
    {
        for n in &result {
            reconcile_one(&state, &account_id, n);
        }
        // Restamp local_version from the post-reconcile DB row — same
        // rationale as list_notes above: a wire-parsed Note always carries
        // local_version: 0, which is not the row's real value.
        for n in &mut result {
            if let Ok(Some(cached)) = state.db.get(&n.uuid, &account_id) {
                n.local_version = cached.local_version;
            }
        }
        let keep: Vec<String> = result.iter().map(|n| n.uuid.clone()).collect();
        match state.db.prune_clean_in_label(&account_id, &path, &keep) {
            Ok(n) if n > 0 => log!(
                "list_notes_in_folder: pruned {} clean row(s) no longer in folder '{}'",
                n, path
            ),
            Ok(_) => {}
            Err(e) => log!("list_notes_in_folder: prune failed: {}", e),
        }
    }
    // D8 fix: drop ghosts whose local cache row is deleted_pending. See
    // list_notes for the full rationale — same race, same fix shape.
    // This is the path the 10s folder settle uses, so it's the primary
    // exposure surface for the bug after a delete.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if !deleted.is_empty() {
            let drop: std::collections::HashSet<String> = deleted.into_iter().collect();
            let before = result.len();
            result.retain(|n| !drop.contains(&n.uuid));
            let dropped = before - result.len();
            if dropped > 0 {
                log!(
                    "list_notes_in_folder: filtered {} ghost(s) from Gmail fetch for '{}'",
                    dropped, path
                );
            }
        }
    }
    // Scoped to this folder — see `append_blocked_notes`. This is the path
    // the 2500 ms sweep uses, so it is the one that was dropping the note.
    append_blocked_notes(&state.db, &account_id, Some(&path), &mut result);
    Ok(result)
}

// Force-refetch a single note's body straight from Gmail, bypassing the
// cache-aware fan-out in list_notes / list_notes_in_label. Use case: the
// user suspects their local cache is stale or corrupted for a specific
// note (long notes edited from multiple places, recovery after a bug fix,
// etc.) and wants to pull the authoritative content without invalidating
// the whole folder. Cheap: one messages.get round-trip + one DB upsert.
//
// Returns the fresh Note (post strip_leading_title, label-mapped) so the
// frontend can replace its in-memory copy without an extra list cycle.
#[tauri::command]
async fn refetch_note(
    account_id: String,
    id: String,
    state: State<'_, AppState>,
) -> Result<gmail::Note, String> {
    if id.is_empty() {
        return Err("refetch_note: empty id (note has no remote version yet)".into());
    }
    let v = vertical_for(&state, &account_id).await?;
    let mut note = v.fetch_note(&id).await.map_err(|e| e.to_string())?;
    note.account_id = Some(account_id.clone());
    log!(
        "refetch_note: uuid={} id={} body_len={}",
        note.uuid, note.id, note.body_html.len()
    );
    // D8 fix: don't return ghosts. If the user marked this note for
    // deletion locally, fetching from Gmail would hand back a note that
    // we logically consider gone — same shape as the list_notes_in_folder
    // case, just for a single message. The reconcile_one call below
    // would correctly skip the upsert (DeletedPending branch), but the
    // frontend would still receive the note and show it. Refuse instead
    // so the caller surfaces a meaningful "already deleted" state.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if deleted.iter().any(|u| u == &note.uuid) {
            return Err(format!(
                "refetch_note: uuid={} is marked deleted locally — refusing to resurrect",
                note.uuid
            ));
        }
    }
    // Upsert through the same reconcile path list_notes uses, so dirty/
    // conflict states are honored consistently (we don't blindly stomp local
    // edits — see reconcile_one for the conflict-copy semantics).
    reconcile_one(&state, &account_id, &note);
    // `note` was parsed straight off the wire and carries local_version: 0
    // (backend/gmail/wire.rs, backend/localfs/decode.rs), which is NOT the
    // row's real local_version — upsert_from_remote's ON CONFLICT DO UPDATE
    // deliberately never touches local_version. If we returned `note` as-is,
    // the frontend would adopt 0 as its "last known" version and every
    // subsequent save would CAS against a stale expected value, producing a
    // permanent false conflict on a note nobody else touched. Restamp from
    // the post-reconcile DB row — the single source of truth for this field.
    if let Ok(Some(cached)) = state.db.get(&note.uuid, &account_id) {
        note.local_version = cached.local_version;
    }
    Ok(note)
}

// Read-only fetch of a trashed message's full content, for the "Recently
// Deleted" preview pane. Deliberately does NOT call reconcile_one or touch
// SQLite at all: the note is gone from the cache by design (deleted_pending
// was already pruned), and upserting it back in would resurrect a note the
// user just deleted. This is purely "show me what this used to say" —
// explicit user click, one messages.get round-trip, nothing persisted.
#[tauri::command]
async fn get_trashed_note_preview(
    account_id: String,
    id: String,
    state: State<'_, AppState>,
) -> Result<gmail::Note, String> {
    if id.is_empty() {
        return Err("get_trashed_note_preview: empty id".into());
    }
    let v = vertical_for(&state, &account_id).await?;
    let mut note = v.fetch_note(&id).await.map_err(|e| e.to_string())?;
    note.account_id = Some(account_id);
    Ok(note)
}

/// Pull pin state from this account's meta_label and apply to the local
/// cache. The frontend triggers this on cold start (after the index pass
/// completes) so a Jodd instance signed into a Gmail account that another
/// Jodd instance has been pinning notes on sees the pins as soon as
/// possible — without having to wait for the user to click "All" or
/// trigger a full list_notes.
///
/// Same shape as the inline reconciliation in list_notes: list the
/// sidecars, apply_remote_pin for each, then clear_pins_not_in for any
/// locally-pinned uuid the listing didn't return. Errors on the
/// meta_label path are surfaced (not silently swallowed like in list_notes
/// where they'd break the note list) — the frontend can log them.
///
/// Lightweight: meta_label is one Gmail label, sidecar count is bounded
/// by "notes the user has pinned" which is typically <100. Each sidecar
/// only needs a Subject-header fetch, no body. Fast even on a 6k mailbox.
#[tauri::command]
async fn sync_pin_state(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Account must exist — error if not found (unlike list_notes which silently skips).
    {
        let list = state.accounts.lock().unwrap();
        if !list.iter().any(|a| a.id == account_id) {
            return Err(format!("Account not found: {}", account_id));
        }
    }
    let v = vertical_for(&state, &account_id).await?;
    // list_sidecars returns Ok(None) if meta_label isn't on Gmail yet — not
    // an error; first pin push will ensure_label and the next sync_pin_state
    // call will find it. On None we skip pruning so we don't wipe locally-pinned
    // notes when the meta store hasn't been created yet (fresh install, sign-out/in).
    let sidecars_opt = v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string())?;
    let Some(sidecars) = sidecars_opt else {
        log!("sync_pin_state: meta store absent for {} — skipping (no prune)", account_id);
        return Ok(0);
    };
    let mut applied = 0usize;
    let mut orphans_removed = 0usize;
    let mut keep: Vec<String> = Vec::with_capacity(sidecars.len());
    for s in &sidecars {
        let n = state.db.apply_remote_pin(&s.note_uuid, &account_id, true, &s.id)
            .unwrap_or(0);
        if n == 0 {
            // Note no longer exists in the DB — orphan sidecar. Remove it so
            // stale .pin files (or Gmail meta messages) don't accumulate.
            // The returned RemoteNoteVersion (Microsoft only) is discarded —
            // there is no local note row to update it against, since this
            // branch only runs for a sidecar whose note no longer exists.
            match v.remove_sidecar(&s.id).await {
                Ok(_) => {
                    log!("sync_pin_state: removed orphan pin sidecar {} (uuid={})", s.id, s.note_uuid);
                    orphans_removed += 1;
                }
                Err(e) => log!("sync_pin_state: remove orphan sidecar {} failed: {}", s.id, e),
            }
        } else {
            applied += n;
            keep.push(s.note_uuid.clone());
        }
    }
    let cleared = state.db.clear_pins_not_in(&account_id, &keep).unwrap_or(0);
    log!(
        "sync_pin_state: account={} sidecars={} applied={} cleared={} orphans_removed={}",
        account_id, sidecars.len(), applied, cleared, orphans_removed
    );
    Ok(applied + cleared)
}

/// Disabled no-op, kept only because the frontend still calls it on cold
/// start and account reactivation (App.svelte, Sidebar.svelte) and removing
/// the command would turn that into a runtime "command not found" instead
/// of the harmless Ok(0) it returns today. Tags have no sidecar to pull —
/// see the comment inside.
#[tauri::command]
async fn sync_tag_state(
    account_id: String,
    _state: State<'_, AppState>,
) -> Result<usize, String> {
    // DISABLED. Tags are now inline #hashtags in the note body (they round-trip
    // with Apple Notes) and are derived into note_tags on every note write
    // (reconcile_tags_from_body_conn). Reading the legacy Notes-Meta tag sidecar
    // would fight that model — a tag the user removed from the body would
    // reappear from a stale sidecar — so this pull is a no-op now.
    log!("sync_tag_state: disabled — tags are inline in the body now ({})", account_id);
    Ok(0)
}

/// Cheap account-wide index — every Notes message's id + label, no body
/// fetch. Returns in seconds even for a 6k mailbox. The frontend uses this
/// to render folder counts and a "loaded X of Y" indicator before bodies
/// arrive. Bodies are hydrated on-demand by `list_notes_in_folder` / full
/// `list_notes` calls — both already cache-aware (Phase B), so this index
/// pass costs nothing the next time around.
///
/// Folder reconciliation runs first, upsert-only, through whichever helper
/// `folder_source_kind` names for this backend — the same mapping `list_notes`
/// uses, so the two passes cannot disagree.
#[tauri::command]
async fn index_account(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::MessageIndex>, String> {
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    // Same dispatch as `list_notes`, through the same one-place mapping, so the
    // cold-start pass and the authoritative pull can never disagree about which
    // source a backend's folders come from.
    //
    // prune=false throughout: cold start adds folders but defers removal to the
    // authoritative list_notes pull, which sees a complete view.
    match folder_source_kind(backend_kind) {
        FolderSourceKind::Labels => {
            let token = ensure_token(&state, &account_id).await?;
            let label_map = cached_label_map(&state, &account_id, &token).await?;
            // Populate the folders cache from the full remote label set so EMPTY
            // folders (no notes) show in the sidebar on cold start. list_notes — the
            // only other folder-sync path — is not called on cold start, so without
            // this an empty label like `Notes/play2` stayed invisible until the user
            // navigated.
            reconcile_folders_from_labels(&state.db, &account_id, &label_map, false);
            let v = vertical_from_parts(&state, &account_id, token, label_map)?;
            v.list_index().await.map_err(|e| e.to_string())
        }
        FolderSourceKind::Vertical => {
            // Exchange folder ids exist nowhere but this listing — see
            // `reconcile_folders_from_vertical`. Best-effort like the LocalFs
            // arm: a folder-scan failure must not sink the whole index pass.
            let v = vertical_for(&state, &account_id).await?;
            if let Err(e) = reconcile_folders_from_vertical(&state.db, &account_id, &*v, false).await {
                log!("index_account: folder reconcile failed for {}: {}", account_id, e);
            }
            let idx = v.list_index().await.map_err(|e| e.to_string())?;
            // AFTER the reads, never before: `blocked_reason` reports what a
            // completed read learned, and asking a fresh instance returns None
            // by construction (Component H2).
            record_backend_block(&state, &account_id, v.blocked_reason());
            Ok(idx)
        }
        FolderSourceKind::Paths => {
            // LocalFs: no token/label_map needed — `vertical_for` resolves
            // root_dir from the account config, and the filesystem is the
            // source of truth for folders.
            let v = vertical_for(&state, &account_id).await?;
            let fs_folders: Vec<String> = v.list_folders().await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path)
                .collect();
            reconcile_folders_from_paths(&state.db, &account_id, &fs_folders, false);
            v.list_index().await.map_err(|e| e.to_string())
        }
    }
}

/// Records a whole-account hard block a read just discovered — Component H2.
///
/// **The sign-in gate is not enough on its own.** Advanced Data Protection can
/// be switched on after the account exists, so an account that worked
/// yesterday can be unreadable today, and nothing about sign-in would ever run
/// again to notice. Every index pass re-stamps this, which also means the field
/// clears itself the moment the account becomes readable again — derived state,
/// re-derived on every pass, exactly as gotcha #4 requires.
///
/// **Writes only on a change.** `index_account` runs on cold start and on every
/// explicit re-index; rewriting `accounts.json` each time would be a file write
/// per refresh for a value that almost never moves.
///
/// The route back to the UI already exists and is deliberately reused rather
/// than invented: the frontend re-reads `list_accounts` on the poll the
/// Inactive group runs, and `Account` serializes whole — so the banner updates
/// with no new event channel. That is gotcha #6's lesson applied ahead of the
/// failure rather than after it.
fn record_backend_block(state: &State<'_, AppState>, account_id: &str, reason: Option<String>) {
    let _gate = state.ai_policy.gate.lock().unwrap();
    let mut list = state.accounts.lock().unwrap();
    let Some(acct) = list.iter_mut().find(|a| a.id == account_id) else { return };
    if acct.blocked_reason == reason {
        return;
    }
    match &reason {
        Some(r) => log!("account {account_id} is blocked: {r}"),
        None => log!("account {account_id} is no longer blocked"),
    }
    invalidate_ai(state, &state.app_handle);
    acct.blocked_reason = reason;
    if let Err(e) = accounts::save_accounts(&list) {
        log!("record_backend_block: save_accounts failed: {e}");
    }
}

/// Whether the local cache was rebuilt after a key-mismatch recovery and
/// the user should be prompted to re-index their Gmail accounts. Backed by
/// a marker file rather than in-memory state so it survives the recovery
/// happening inside `.setup()`, before any frontend command can run.
#[tauri::command]
fn needs_reindex_after_recovery() -> bool {
    paths::data_base()
        .map(|d| d.join("jodd").join("NEEDS_REINDEX").exists())
        .unwrap_or(false)
}

/// Clear the recovery marker once the user has re-indexed (or dismissed
/// the prompt).
#[tauri::command]
fn clear_reindex_marker() -> Result<(), String> {
    if let Some(dir) = paths::data_base() {
        let marker = dir.join("jodd").join("NEEDS_REINDEX");
        if marker.exists() {
            std::fs::remove_file(&marker).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Cache-first read scoped to one folder. Pure SQLite, no token refresh,
/// no label_map lookup, no Gmail round-trip — returns in sub-ms. This is
/// the doctrine-compliant navigation read: clicking a folder paints
/// immediately from the local replica. Reconciliation against Gmail is
/// the sweep tick's job (it calls `list_notes_in_folder` instead).
///
/// Returns notes whose label exactly equals `path`, excluding rows in
/// `deleted_pending`. A folder the user just created locally that has
/// no notes yet returns an empty vec — no "Folder not found" error,
/// even if the label hasn't been pushed to Gmail yet.
#[tauri::command]
async fn list_cached_notes_in_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let cached = state.db.list_notes_by_label(&account_id, &path).map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Read the local replica for one account. Used by the frontend on cold
/// start to paint the UI before the network fetch returns — this is the
/// "instant launch" path. Always succeeds (returns an empty vec if the
/// cache has never been populated). Pure local read, no network.
#[tauri::command]
async fn list_cached_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let db = state.db.clone();
    let cached = db.list_notes(&account_id).map_err(|e| e.to_string())?;
    // The pinned count rides along because this is the exact moment the list
    // paints: if the cache holds the pin here and the UI shows no PINNED
    // heading, the bug is in the paint; if it holds none, the pin never
    // reached SQLite and the walk finding it is beside the point.
    log!(
        "list_cached_notes: {} returned {} cached notes ({} pinned)",
        account_id,
        cached.len(),
        cached.iter().filter(|n| n.pinned).count()
    );
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Return folder paths from the LOCAL CACHE only. Sub-ms read. Includes
/// folders in any non-deleted state (clean / dirty_new / dirty_renamed)
/// so newly-created-but-not-yet-pushed folders are visible immediately.
///
/// Reconciliation with Gmail happens inside `list_notes` (which has the
/// authoritative label_map) — no network call needed here.
///
/// Returns the implicit "Notes" root if the cache doesn't have it yet
/// (first-run before any sync). This keeps the Sidebar from being
/// empty on the very first cold start.
#[tauri::command]
async fn list_folders(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let cached = state.db.list_folders(&account_id).map_err(|e| e.to_string())?;
    let mut paths: Vec<String> = cached.into_iter().map(|f| f.path).collect();
    // Ensure the "Notes" root is always present, even on a cold first run
    // before any sync has populated the folders table.
    if !paths.iter().any(|p| p == "Notes") {
        paths.push("Notes".to_string());
    }
    paths.sort();
    Ok(paths)
}

/// Folder kinds for one account — `(path, kind)` pairs, where kind is one of
/// `'user' | 'system_workflow' | 'smart_query'`. Companion to `list_folders`
/// for the Sidebar Folders/Workflows split (Task 16). Returning a separate
/// command keeps `list_folders` callers (NoteContextMenu, etc.) unchanged.
/// Paths not present in the cache (e.g. the implicit "Notes" root before any
/// sync) are absent here — the frontend treats absence as 'user'.
#[tauri::command]
async fn list_folder_kinds(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<(String, String)>, String> {
    let cached = state.db.list_folders(&account_id).map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|f| (f.path, f.kind)).collect())
}

/// Notes with zero incoming [[wikilink]] backlinks. Pure SQLite read.
/// Backs the fully-virtual "Orphaned" Smart Folder — no `folders` table
/// row (see design spec decision 4).
#[tauri::command]
fn list_orphaned_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = state.db.list_orphaned_notes(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

/// Notes untouched for 30+ days. Pure SQLite read. Backs the fully-virtual
/// "Stale" Smart Folder.
#[tauri::command]
fn list_stale_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = state.db.list_stale_notes(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

/// Notes carrying Extract's Source block, wherever they are filed. Pure
/// SQLite read. Backs the fully-virtual "Extracts" Smart Folder.
#[tauri::command]
fn list_extract_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = state.db.list_extract_notes(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

// ── Folder ops: local-first ─────────────────────────────────────────────
//
// All four mutate the SQLite `folders` table immediately and return. The
// background sync worker pushes the changes to Gmail on its next tick.
// Validation rules (name format, no-empty, no-root, etc.) run in the
// command path so the user gets an immediate error for invalid inputs.

#[tauri::command]
async fn create_folder(
    account_id: String,
    name: String,
    parent_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<gmail::FolderInfo, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!(
        "create_folder: account={} name={:?} parent={:?}",
        account_id, name, parent_path
    );
    let segment = validate_folder_segment(&name)?;
    let full = match parent_path.as_deref() {
        Some(p) if p == "Notes" || p.starts_with("Notes/") => format!("{}/{}", p, segment),
        None => format!("Notes/{}", segment),
        Some(other) => return Err(format!("Invalid parent path: {}", other)),
    };
    // Reject duplicates against what's in the cache (which mirrors Gmail
    // + any in-flight local creates). Worker re-checks against Gmail.
    if let Ok(Some(_)) = state.db.get_folder(&account_id, &full) {
        return Err(format!("Folder '{}' already exists", full));
    }
    let folder = db::CachedFolder {
        account_id: account_id.clone(),
        path: full.clone(),
        label_id: None,
        sync_state: db::FolderSyncState::DirtyNew,
        last_local_modified_at: db::now_ms(),
        last_synced_at: None,
        // User-created via the sidebar → kind='user'. Workflow folders
        // are minted by ensure_workflow_folder (Task 3) with kind=
        // 'system_workflow', not by this command.
        kind: "user".to_string(),
    };
    state.db.insert_folder_local_new(&folder).map_err(|e| e.to_string())?;
    log!("create_folder (local-first): path='{}'", full);
    // Return shape matches the old API so existing frontend works. id is
    // empty until the worker assigns one.
    Ok(gmail::FolderInfo { id: String::new(), name: full })
}

#[tauri::command]
async fn rename_folder(
    account_id: String,
    path: String,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<gmail::FolderInfo, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!(
        "rename_folder: account={} path={:?} new_name={:?}",
        account_id, path, new_name
    );
    let new_segment = validate_folder_segment(&new_name)?;
    if path == "Notes" {
        return Err("Cannot rename the root 'Notes' folder".to_string());
    }
    if !path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", path));
    }
    let parent_path: String = path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
    let new_path = if parent_path.is_empty() {
        new_segment.clone()
    } else {
        format!("{}/{}", parent_path, new_segment)
    };
    if new_path == path {
        return Ok(gmail::FolderInfo { id: String::new(), name: new_path });
    }
    // Reject if a sibling already has this name.
    if let Ok(Some(_)) = state.db.get_folder(&account_id, &new_path) {
        return Err(format!("'{}' already exists", new_path));
    }
    // Rename the folder AND cascade to descendants AND notes' label field
    // in one transaction. Each touched folder transitions to dirty_renamed
    // so the worker pushes each rename to Gmail individually.
    let touched = state.db.rename_subtree(&account_id, &path, &new_path)
        .map_err(|e| e.to_string())?;
    log!(
        "rename_folder (local-first): '{}' → '{}', {} folder row(s) cascaded",
        path, new_path, touched
    );
    Ok(gmail::FolderInfo { id: String::new(), name: new_path })
}

#[tauri::command]
async fn delete_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!("delete_folder: account={} path={:?}", account_id, path);
    if path == "Notes" {
        return Err("Cannot delete the root 'Notes' folder".to_string());
    }
    if !path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", path));
    }
    // Check non-empty from the cache (cheap, no network). The cache mirrors
    // notes from the most recent fetch; in-flight local-only edits are
    // counted too — both safer.
    let folders = state.db.list_folders(&account_id).map_err(|e| e.to_string())?;
    let folder_exists = folders.iter().any(|f| f.path == path);
    if !folder_exists {
        return Err(format!("Folder not found: {}", path));
    }
    let prefix = format!("{}/", path);
    let has_children = folders.iter().any(|f| f.path.starts_with(&prefix));
    if has_children {
        return Err(format!("Folder '{}' has sub-folders. Delete those first.", path));
    }
    // Count notes in this label (excluding deleted_pending). Local query, no network.
    let note_count = state.db.count_notes_in_label(&account_id, &path)
        .map_err(|e| e.to_string())?;
    if note_count > 0 {
        return Err(format!(
            "Folder '{}' is not empty ({} notes). Move or delete them first.",
            path, note_count
        ));
    }
    state.db.mark_folder_deleted(&account_id, &path).map_err(|e| e.to_string())?;
    log!("delete_folder (local-first): marked deleted_pending for '{}'", path);
    Ok(())
}

#[tauri::command]
async fn move_folder(
    account_id: String,
    from_path: String,
    to_parent_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    // **Reparenting is a different write than rename, and `Writes.folders`
    // does not cover it.** create_folder/rename_folder/delete_folder are all
    // measured live on iCloud (2026-08-24); a rename NEVER changes
    // ParentFolder (it always keeps the same parent segment — see this
    // command's own `new_path` construction below). This command is the one
    // path that changes a folder's PARENT, which would mean writing
    // `ParentFolder` on a record that already exists — nothing here has ever
    // sent that, live or in a unit test. Refuse it explicitly rather than let
    // `Writes.folders = true` imply a shape that was never checked, the same
    // discipline gotcha #12/#18 apply per-backend with no wildcard arm.
    if account_backend_kind(&state, &account_id) == Some(accounts::BackendKind::ICloud) {
        return Err(
            "Moving a folder to a different parent isn't available on iCloud yet — do it \
             in Apple Notes and it will sync back here."
                .to_string(),
        );
    }
    log!(
        "move_folder: account={} from={:?} to_parent={:?}",
        account_id, from_path, to_parent_path
    );
    if from_path == "Notes" {
        return Err("Cannot move the root 'Notes' folder".to_string());
    }
    if !from_path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", from_path));
    }
    if to_parent_path != "Notes" && !to_parent_path.starts_with("Notes/") {
        return Err(format!("Destination must be under Notes/: {}", to_parent_path));
    }
    let leaf = from_path.rsplit('/').next()
        .ok_or_else(|| "Invalid from_path".to_string())?;
    let new_path = format!("{}/{}", to_parent_path, leaf);
    if new_path == from_path {
        return Ok(new_path);
    }
    if to_parent_path == from_path || to_parent_path.starts_with(&format!("{}/", from_path)) {
        return Err("Cannot move a folder into itself or its sub-folder".to_string());
    }
    // Reject collision at destination.
    if let Ok(Some(_)) = state.db.get_folder(&account_id, &new_path) {
        return Err(format!("'{}' already exists at the destination", new_path));
    }
    let touched = state.db.rename_subtree(&account_id, &from_path, &new_path)
        .map_err(|e| e.to_string())?;
    log!(
        "move_folder (local-first): '{}' → '{}', {} folder row(s) cascaded",
        from_path, new_path, touched
    );
    Ok(new_path)
}

// move_note (label-modify based) was removed 2026-06-09 — dead code with no
// callers anywhere in the frontend. The actual move-folder flow is implemented
// in NoteContextMenu.svelte via save_note (insert + trash), which preserves
// the X-UUID and works cross-folder without separate move logic. If a future
// "fast move that skips body re-upload" is needed, re-add with: validate
// message_id belongs to account_id (cache lookup), check both labels exist,
// and update the local cache row's label inside the same critical section.

// ─── Orphan cleanup (safe replacement for cleanup_stale_uuid_duplicates) ─────
//
// Gmail can accumulate multiple messages with the same X-UUID when save's
// delete-old fails (network blip, race with Apple Notes' IMAP edits, etc.).
// The in-memory dedup in list_notes_in_label hides them from the UI, but
// they waste Gmail storage and slow down subsequent list operations.
//
// This is the SAFE cleanup path. Unlike the original fire-and-forget version
// (which captured keep_id at save time and raced with the next save), this:
//   1. Skips UUIDs whose push is currently in flight (state.pushing set)
//   2. Re-reads the canonical cache.id IMMEDIATELY before each trash call,
//      so a save that lands between scan and trash can't have its live
//      message destroyed
//   3. Bounds work to notes modified in the last 24 hours — older notes
//      rarely accumulate new orphans and the per-uuid header fetch cost
//      is O(messages_in_Notes_labels)
//
// Triggered manually via the cleanup_orphans command. Auto-trigger is held
// back until multi-device test coverage exists (specifically: ensuring a
// fresh Apple-Notes-side edit isn't trashed before it's been polled).

/// How long a tombstoned tag survives before we're confident the underlying
/// note really is gone (not just transiently missing from a Gmail listing)
/// and the tag can be permanently dropped. 7 days is generous relative to
/// any Gmail eventual-consistency hiccup or pagination glitch we've observed,
/// while still bounding how long deleted-account-style cruft lingers.
const TOMBSTONE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

async fn safe_cleanup_orphans_for_account(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Result<usize, String> {
    // Orphan cleanup only applies to Gmail — see `orphan_cleanup_supported`'s
    // doc comment.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if !orphan_cleanup_supported(a.backend_kind) {
                log!(
                    "safe_cleanup_orphans_for_account: skipping non-Gmail account {} ({:?})",
                    account_id, a.backend_kind
                );
                return Ok(0);
            }
        }
    }
    let token = ensure_token(state, account_id).await?;
    let label_map = cached_label_map(state, account_id, &token).await?;
    // One bulk uuid->ids scan instead of one full Notes/* re-scan per note
    // (see find_all_duplicate_ids doc comment — this used to be O(candidates
    // * mailbox size) and could hang for many minutes on a normal mailbox).
    let dup_map = gmail::find_all_duplicate_ids(&token, &label_map).await?;

    // The 24h "recent-edit" gate that used to live here was a
    // holdover from the auto-cleanup era (disabled 2026-06-09). Auto-trash
    // had to be cautious because a fresh Apple-Notes-side edit not yet
    // polled could look like an orphan; the gate kept it away from recent
    // notes. This is the user-triggered path now — the in-flight push
    // check + the live-cache-id refusal below are the actual safety net.
    // Keeping the 24h window made the sidebar "N dup" pill diverge from
    // what cleanup could actually fix: stale dups from >24h ago counted
    // toward the pill but were invisible to the modal and untouchable by
    // cleanup. Now any clean note with a non-empty cache id is in scope.
    let candidates: Vec<db::CachedNote> = state.db
        .list_notes(account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|n| {
            matches!(n.sync_state, db::SyncState::Clean) && !n.id.is_empty()
        })
        .collect();

    log!(
        "cleanup_orphans: scanning {} clean note(s) for account {}",
        candidates.len(), account_id
    );

    let mut trashed_total: usize = 0;
    for note in candidates {
        // Skip if any push is in flight for this uuid. Pushing entries are
        // added by sync_worker_tick lines 1161/1182 before gmail::save_note
        // and removed after — covers the only path that mutates Gmail for
        // this uuid (frontend save_note is local-first only).
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            p.contains(&(account_id.to_string(), note.uuid.clone()))
        };
        if in_flight {
            continue;
        }

        let found = dup_map.get(&note.uuid).cloned().unwrap_or_default();
        if found.len() <= 1 {
            continue; // no duplicates to clean
        }

        // For each candidate, re-verify safety RIGHT BEFORE trashing.
        // This closes the TOCTOU window: between scan completion and trash,
        // a new save could land. If it does, cache.id moves and we bail.
        for gmail_id in found {
            if gmail_id == note.id {
                continue; // this is our live one
            }
            let still_safe = {
                let p = state.pushing.lock().unwrap();
                if p.contains(&(account_id.to_string(), note.uuid.clone())) {
                    false
                } else {
                    match state.db.get(&note.uuid, account_id) {
                        Ok(Some(cur)) => cur.id == note.id,
                        _ => false,
                    }
                }
            };
            if !still_safe {
                log!(
                    "cleanup_orphans: bailing uuid={} — state moved during scan",
                    note.uuid
                );
                break;
            }
            let v_del = vertical_from_parts(state, account_id, token.clone(), label_map.clone())?;
            match v_del.delete(&gmail_id).await.map_err(|e| e.to_string()) {
                Ok(_) => {
                    trashed_total += 1;
                    log!(
                        "cleanup_orphans: trashed orphan id={} for uuid={}",
                        gmail_id, note.uuid
                    );
                }
                Err(e) => {
                    log!(
                        "cleanup_orphans: trash failed id={}: {}",
                        gmail_id, e
                    );
                }
            }
        }
    }
    log!(
        "cleanup_orphans: trashed {} total orphan(s) for {}",
        trashed_total, account_id
    );
    Ok(trashed_total)
}

#[tauri::command]
async fn cleanup_orphans(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let trashed = safe_cleanup_orphans_for_account(&state, &account_id).await?;
    // No optimistic decrement of dup_stats here. The previous "subtract
    // trashed from collapsed" looked responsive but caused the pill to
    // flicker: Gmail's index is eventually consistent, so the next
    // list_notes often still saw the just-trashed messages, the pill
    // jumped back up, then dropped again on the poll after. The next
    // list_notes is the single source of truth — frontend should trigger
    // a refresh after cleanup if it wants the pill to update sooner.
    Ok(trashed)
}

#[tauri::command]
fn get_dup_stats(
    account_id: String,
    state: State<'_, AppState>,
) -> gmail::DedupSummary {
    state
        .dup_stats
        .lock()
        .unwrap()
        .get(&account_id)
        .cloned()
        .unwrap_or_default()
}

// ─── Orphan review (Tier 2 — shows duplicates before trashing) ──────────────

#[derive(serde::Serialize, Clone, Debug)]
pub struct OrphanVersion {
    pub id: String,
    pub title: String,
    pub date: String,
    /// Plain-text preview of the body, first ~200 chars after stripping HTML.
    pub body_preview: String,
    pub label: String,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct OrphanGroup {
    pub uuid: String,
    pub keeper: OrphanVersion,
    /// The other Gmail messages with the same X-UUID. These would be trashed
    /// on user confirmation. Order: most recent first.
    pub orphans: Vec<OrphanVersion>,
}

/// Strip HTML tags and decode &nbsp; from body for a clean text preview.
fn body_to_preview(body_html: &str, max_chars: usize) -> String {
    // Crude but adequate: strip <tags>, collapse whitespace.
    let no_tags: String = {
        let mut out = String::with_capacity(body_html.len());
        let mut in_tag = false;
        for c in body_html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => out.push(c),
                _ => {}
            }
        }
        out
    };
    let collapsed: String = no_tags
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= max_chars {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(max_chars).collect();
        format!("{}…", truncated)
    }
}

#[tauri::command]
async fn preview_orphans(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<OrphanGroup>, String> {
    // Orphan preview only applies to Gmail — see `orphan_cleanup_supported`'s
    // doc comment.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if !orphan_cleanup_supported(a.backend_kind) {
                log!(
                    "preview_orphans: skipping non-Gmail account {} ({:?}) — no dups possible",
                    account_id, a.backend_kind
                );
                return Ok(Vec::new());
            }
        }
    }
    log!("preview_orphans: starting for account {}", account_id);
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    log!("preview_orphans: token + label_map ready ({} labels)", label_map.len());
    // One bulk uuid->ids scan instead of one full Notes/* re-scan per note
    // (see find_all_duplicate_ids doc comment — this used to be O(candidates
    // * mailbox size) and is what made this modal appear to hang forever).
    let dup_map = gmail::find_all_duplicate_ids(&token, &label_map).await?;
    log!("preview_orphans: dup scan found {} uuid(s) with >1 message", dup_map.iter().filter(|(_, v)| v.len() > 1).count());
    // No 24h recent-edit gate — see safe_cleanup_orphans_for_account for
    // the rationale. The modal must show every dup the sidebar's "N dup"
    // pill is counting; otherwise the user clicks cleanup and watches the
    // pill stay the same.
    let candidates: Vec<db::CachedNote> = state
        .db
        .list_notes(&account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|n| {
            matches!(n.sync_state, db::SyncState::Clean) && !n.id.is_empty()
        })
        .collect();

    let mut groups: Vec<OrphanGroup> = Vec::new();
    log!("preview_orphans: {} candidates to scan", candidates.len());
    for note in candidates {
        // Skip in-flight pushes — same safety rule as cleanup.
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            p.contains(&(account_id.to_string(), note.uuid.clone()))
        };
        if in_flight {
            continue;
        }
        let ids = dup_map.get(&note.uuid).cloned().unwrap_or_default();
        if ids.len() <= 1 {
            continue;
        }

        let keeper = OrphanVersion {
            id: note.id.clone(),
            title: note.title.clone(),
            date: note.date.clone(),
            body_preview: body_to_preview(&note.body_html, 200),
            label: note.label.clone(),
        };

        let mut orphan_versions: Vec<OrphanVersion> = Vec::new();
        for id in ids {
            if id == note.id {
                continue;
            }
            // Fetch each orphan so the user can see what's about to be trashed.
            // Slow on big mailboxes with many duplicates — this is explicit
            // user action, latency is acceptable.
            let v_fetch = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
            match v_fetch.fetch_note(&id).await.map_err(|e| e.to_string()) {
                Ok(n) => orphan_versions.push(OrphanVersion {
                    id: n.id,
                    title: n.title,
                    date: n.date,
                    body_preview: body_to_preview(&n.body_html, 200),
                    label: n.label,
                }),
                Err(e) => {
                    log!("preview_orphans: fetch failed id={}: {}", id, e);
                    continue;
                }
            }
        }
        if orphan_versions.is_empty() {
            continue;
        }
        // Sort most-recent first so the user sees the freshest "almost-keeper"
        // candidate at the top of each group.
        orphan_versions.sort_by(|a, b| {
            let parse = |s: &str| chrono::DateTime::parse_from_rfc2822(s).ok();
            parse(&b.date).cmp(&parse(&a.date))
        });

        groups.push(OrphanGroup {
            uuid: note.uuid.clone(),
            keeper,
            orphans: orphan_versions,
        });
    }
    log!("preview_orphans: returning {} group(s)", groups.len());
    Ok(groups)
}

/// Trash specific Gmail message ids. Used by the review modal after the
/// user confirms which orphans to clean up.
///
/// Safety re-checks every id immediately before the API call: it must not
/// be the current cache.id for ANY note (would be trashing the live one),
/// and the cache row whose uuid owns it must not have an in-flight push.
/// Either failure makes us skip that id.
#[tauri::command]
async fn trash_specific_messages(
    account_id: String,
    message_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    if message_ids.is_empty() {
        return Ok(0);
    }
    // Build a set of all current cache.ids for this account so we can refuse
    // to trash any of them. The cleanup_orphans path already filters by
    // re-reading per-uuid; for the explicit-id path we need a different
    // shape: which uuid owns each id, and is that uuid clean?
    let cached: HashMap<String, (String, db::SyncState)> = state
        .db
        .list_notes(&account_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|n| (n.id, (n.uuid, n.sync_state)))
        .collect();
    // Fetch the vertical once (token + label_map + meta_label) for all deletes.
    let v = vertical_for(&state, &account_id).await?;

    let mut trashed = 0;
    for id in message_ids {
        if id.is_empty() {
            continue;
        }
        if let Some((uuid, _state)) = cached.get(&id) {
            log!(
                "trash_specific_messages: refusing to trash id={} — it's the live cache id for uuid={}",
                id, uuid
            );
            continue;
        }
        let in_flight = {
            let p = state.pushing.lock().unwrap();
            // We don't know the uuid since this id isn't in the cache (it's
            // an orphan from Gmail). Conservative: if ANY push is in flight
            // for this account, skip. Keeps timing simple; orphans aren't
            // urgent and the next review will surface them again.
            p.iter().any(|(aid, _)| aid == &account_id)
        };
        if in_flight {
            log!(
                "trash_specific_messages: deferring id={} — pushes in flight",
                id
            );
            continue;
        }
        match v.delete(&id).await.map_err(|e| e.to_string()) {
            Ok(_) => {
                trashed += 1;
                log!("trash_specific_messages: trashed id={}", id);
            }
            Err(e) => log!("trash_specific_messages: trash failed id={}: {}", id, e),
        }
    }
    // No optimistic dup_stats decrement — same rationale as cleanup_orphans:
    // Gmail's index is eventually consistent so the next list_notes is the
    // single source of truth. Decrementing here caused the "N dup" pill to
    // flicker after cleanup, which read as a bug to users.
    Ok(trashed)
}

/// Run one sync tick immediately instead of waiting out the worker's sleep.
/// Called by the frontend on visibility transitions: on Android the worker
/// loop stops entirely once the OS freezes a backgrounded process, so without
/// this a note edited just before switching apps would sit dirty. See spec §5.
///
/// Shares per-account admission with periodic work and immediate LocalFS writes.
#[tauri::command]
async fn flush_sync(app: AppHandle) -> Result<(), String> {
    sync_worker_tick(&app).await;
    Ok(())
}

// ─── Legacy migration ────────────────────────────────────────────────────────

// On startup, if we find a legacy "jodd/refresh_token" keychain entry AND
// no accounts.json yet, migrate it to the multi-account model: refresh the
// token to learn the email, persist as the first account, delete the legacy
// keychain entry.
async fn migrate_legacy_keychain(state: &AppState) {
    if !state.accounts.lock().unwrap().is_empty() {
        return; // already migrated or new install
    }
    let Some(rt) = accounts::take_legacy_refresh_token() else {
        return; // no legacy entry
    };
    log!("migrate: found legacy refresh token, resolving email...");
    let token_data = match auth::refresh_access_token(&rt).await {
        Ok(t) => t,
        Err(e) => {
            log!("migrate: refresh failed: {} — discarding legacy token", e);
            return;
        }
    };
    let email = match gmail::get_user_email(&token_data.access_token).await {
        Ok(e) => e,
        Err(e) => {
            log!("migrate: getProfile failed: {} — discarding", e);
            return;
        }
    };
    log!("migrate: legacy account = {}", email);

    // Save refresh token under per-account key. Prefer Google's rotated rt if present.
    // Always Gmail — this migrates the pre-multi-account legacy keychain entry,
    // which predates any other backend existing.
    let rt_to_save = token_data.refresh_token.unwrap_or(rt);
    let account_id = accounts::account_id_for(accounts::BackendKind::Gmail, &email);
    let _ = accounts::save_refresh_token(&account_id, &rt_to_save);

    // Persist the account record.
    let mut list = state.accounts.lock().unwrap();
    list.push(Account {
        id: account_id.clone(),
        email: email.clone(),
        added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None,
        meta_label: None,
        llm: Default::default(),
        backend_kind: Default::default(), // Gmail
        root_dir: None,
        icloud_session_established: false,
                blocked_reason: None,
        sync_cursor: None,
        icloud_replica_id: None,
        status: accounts::AccountStatus::Active,
        pending_removal: false,
    });
    let _ = accounts::save_accounts(&list);

    // Cache the live access token so the user doesn't see a sign-in screen.
    // Keyed by account_id, matching every other reader of this map.
    let mut states = state.account_states.lock().unwrap();
    let entry = states.entry(account_id).or_default();
    entry.access_token = Some(token_data.access_token);
    entry.token_expires_at = Some(token_deadline_from_expires_in(token_data.expires_in));

    log!("migrate: legacy account migrated successfully");
}

// ─── Lesson extraction ───────────────────────────────────────────────────────

/// What an Extract created, and where — the frontend navigates to `label`
/// instead of assuming a folder.
#[derive(serde::Serialize)]
struct ExtractedNoteDto {
    uuid: String,
    label: String,
}

/// Where `extract_note_into` files the new note.
enum ExtractDestination {
    /// A fresh Extract: `filing::resolve_destination`.
    Resolve,
    /// A Re-extract: beside the source note, via `filing::destination_beside`.
    BesideSource(String),
}

/// Whether `account_id`'s backend can create folders. `false` for an
/// unknown account, which also routes its extract to the root.
fn account_can_create_folders(state: &AppState, account_id: &str) -> bool {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id)
        .map(|a| Capabilities::for_backend(a.backend_kind).writes.folders)
        .unwrap_or(false)
}

/// No credentials or note content in the eligibility snapshot. Changes to any
/// account's visibility/permission or provider invalidate retained AI history.
fn ai_stamp(state: &AppState) -> serde_json::Value {
    let accounts = state.accounts.lock().unwrap();
    let rows: Vec<_> = accounts.iter().map(|a| serde_json::json!([
        a.id, a.status, a.backend_kind, a.llm, a.blocked_reason, a.pending_removal
    ])).collect();
    serde_json::json!([state.ai_policy.revision.load(std::sync::atomic::Ordering::SeqCst), rows, app_llm_config::load()])
}

struct AiRequestCleanup<'a> {
    requests: &'a Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
    id: String,
}
impl Drop for AiRequestCleanup<'_> {
    fn drop(&mut self) { self.requests.lock().unwrap().remove(&self.id); }
}

fn checked_account_provider<'a>(state: &'a AppState, account: &accounts::Account)
    -> Result<Box<dyn llm::provider::LlmProvider + 'a>, llm::provider::ExtractError>
{
    let _gate = state.ai_policy.gate.lock().unwrap();
    let accounts = state.accounts.lock().unwrap().clone();
    let stamp = ai_stamp(state);
    let inner = llm::policy::build_account_provider(&accounts, &account.id, llm::resolve::resolve_provider_for_account)?;
    Ok(Box::new(llm::policy::CheckedProvider { cancel: Mutex::new(None), gate: &state.ai_policy.gate, inner, valid: Box::new(move || ai_stamp(state) == stamp) }))
}

fn issue_ai_result(state: &AppState, account_id: &str) -> String {
    state.ai_policy.issue_result(account_id, ai_stamp(state))
}
fn validate_ai_result(state: &AppState, account_id: &str, id: &str) -> Result<(), String> {
    llm::policy::require_account(&state.accounts.lock().unwrap(), account_id).map_err(|e| e.to_string())?;
    let stamp = ai_stamp(state);
    if state.ai_policy.result_valid(id, account_id, stamp) { Ok(()) }
    else { Err("AI result expired after account, permission or provider changes. Generate it again.".into()) }
}

fn invalidate_ai(state: &AppState, app: &tauri::AppHandle) {
    state.ai_policy.invalidate();
    for cancel in state.in_flight_extracts.lock().unwrap().values() { cancel.cancel(); }
    for cancel in state.in_flight_asks.lock().unwrap().values() { cancel.cancel(); }
    let _ = app.emit("ai-policy-changed", ());
}

#[derive(serde::Serialize)]
struct AskSessionInfo { session_id: String, destination: String, scope: String }

/// Read-only preflight: no provider construction, keychain access or AI call.
#[tauri::command]
fn begin_ask(scope: ask::AskScope, state: State<'_, AppState>) -> Result<AskSessionInfo, String> {
    let _gate = state.ai_policy.gate.lock().unwrap();
    let stamp = ai_stamp(&state);
    let accounts = state.accounts.lock().unwrap();
    if let Some(id) = scope.account_id() {
        llm::policy::require_account(&accounts, id).map_err(|e| e.to_string())?;
    }
    let count = accounts.iter().filter(|a| llm::policy::account_allowed(a)).count();
    if count == 0 { return Err("No accounts allow AI data access. Review Account Settings.".into()); }
    drop(accounts);
    let cfg = app_llm_config::load().ok_or("Ask Jodd needs an LLM provider. Open App Settings.")?;
    let destination = match cfg.llm.provider {
        accounts::LlmProviderKind::Http => {
            let base = cfg.llm.http_base_url.as_deref().ok_or("HTTP destination is missing. Open App Settings.")?;
            let mut url = reqwest::Url::parse(base).map_err(|_| "HTTP destination is invalid. Open App Settings.")?;
            let _ = url.set_username(""); let _ = url.set_password(None); url.set_query(None); url.set_fragment(None);
            format!("HTTP {} · model {}", url, cfg.llm.http_model.as_deref().ok_or("HTTP model is missing. Open App Settings.")?)
        },
        accounts::LlmProviderKind::AgentCli | accounts::LlmProviderKind::ClaudeCode => format!(
            "Agent CLI {} · destination/model follow the CLI configuration; may use cloud services",
            llm::resolve::agent_preset_id_of(&cfg.llm).ok_or("Agent CLI is missing. Open App Settings.")?),
        _ => return Err("Ask Jodd needs an LLM provider. Open App Settings.".into()),
    };
    let scope_description = match &scope {
        ask::AskScope::AllAccounts => format!("All AI-allowed active accounts ({count}); disabled accounts excluded"),
        ask::AskScope::Account { account_id } => format!("Account {account_id}"),
        ask::AskScope::Folder { account_id, label } => format!("{account_id} · {label} and subfolders"),
    };
    if ai_stamp(&state) != stamp { return Err("AI settings changed. Reopen Ask Jodd.".into()); }
    let id = uuid::Uuid::new_v4().to_string();
    let mut sessions = state.ai_policy.sessions.lock().unwrap();
    // Ephemeral conversations are bounded even if a webview disappears.
    if sessions.len() >= 16 {
        for (_, old) in sessions.drain() { old.cancel.cancel(); }
    }
    sessions.insert(id.clone(), llm::policy::Session {
        stamp, scope, turns: Vec::new(), cancel: tokio_util::sync::CancellationToken::new(), busy: false,
    });
    Ok(AskSessionInfo { session_id: id, destination, scope: scope_description })
}

#[tauri::command]
fn end_ask(session_id: String, state: State<'_, AppState>) {
    if let Some(session) = state.ai_policy.sessions.lock().unwrap().remove(&session_id) { session.cancel.cancel(); }
}

/// Workflow entry point: take a chunk of source text, ship it to the
/// configured LLM provider, render the structured response into a note, and
/// persist it locally in `filing::resolve_destination` (`Notes/Inbox`, or the
/// root where the backend cannot create folders). On LLM failure, creates a
/// fallback note containing only the verbatim source so the paste is never
/// lost.
///
/// Refuses `Write::Notes` only. It used to refuse `Write::Folders` as well,
/// which made Extract unavailable on Outlook, whose folder writes are
/// permanently false (gotcha #12) — while the root it now falls back to is
/// writable there by well-known name.
#[tauri::command]
async fn extract_note(
    account_id: String,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "extract_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        extract_note_into(account_id, source_text, title_override, request_id, ExtractDestination::Resolve, state).await
    }).await
}

async fn extract_note_into(
    account_id: String,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    destination: ExtractDestination,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
    log!(
        "extract_note: account={} source_len={} request_id={}",
        account_id,
        source_text.len(),
        request_id
    );

    // Register a CancellationToken under this request_id so the frontend can
    // abort via cancel_extraction(request_id). The token is removed on any
    // exit path — success, error, or cancel — by the guard below.
    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .in_flight_extracts
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    // Resolve provider from account config. Clone the Account out so we don't
    // hold the accounts Mutex across the LLM await.
    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .ok_or_else(|| {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                format!("account not found: {account_id}")
            })?
            .clone()
    };
    let provider = checked_account_provider(&state, &account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    // Roadmap #0: show the model this account's existing tag vocabulary so
    // it prefers reusing a tag over minting a near-duplicate. Best-effort —
    // a lookup failure here must not block extraction; an empty list just
    // falls back to the plain prompt (`extract_system_prompt`).
    let existing_tags: Vec<String> =
        state.db.list_all_tags(&account_id).map(|rows| rows.into_iter().map(|(tag, _)| tag).collect()).unwrap_or_default();

    // Call LLM. The provider races its I/O against `cancel` and returns
    // ExtractError::Cancelled if the token fires.
    llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
    llm::receipts::stage("extract");
    let envelope = match provider.extract(&source_text, &existing_tags, cancel).await {
        Ok(env) => env,
        Err(crate::llm::provider::ExtractError::Cancelled) => {
            // User-initiated abort: discard everything. Do NOT create a
            // fallback note — the user actively chose to cancel, not "lose"
            // their paste. The textarea still holds the source if they want
            // to retry.
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("extract_note: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("extract_note: provider error (details omitted) — creating fallback note");
            // Remove the cancel token BEFORE `?`, as append_extract_note
            // does: a failed fallback write must not leak it.
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = create_fallback_source_note(&state, &account_id, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let uuid = fallback?;
            return Err(format!(
                "LLM call failed; source preserved in note {uuid}. {e}"
            ));
        }
    };

    let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
    llm::receipts::stage("saving_locally");
    save_workflow_envelope(
        &state,
        &account,
        &account_id,
        &request_id,
        &source_text,
        envelope,
        title_override,
        "Extract",
        destination,
    )
}

/// Shared tail of every "LLM envelope -> saved local note" workflow: derive
/// the title, resolve/confirm the destination folder, mint a uuid (gotcha
/// #18 — per-backend, never assume Apple's wire format), and insert. Called
/// once the caller already has an `ExtractEnvelope`, however it got one --
/// covers `extract_note_into` (Extract) and `run_workflow_note_into` (the
/// three new workflows) alike; they diverge only in HOW they obtain the
/// envelope, not in what happens to it afterward.
fn save_workflow_envelope(
    state: &State<'_, AppState>,
    account: &accounts::Account,
    account_id: &str,
    request_id: &str,
    source_text: &str,
    envelope: crate::llm::provider::ExtractEnvelope,
    title_override: Option<String>,
    default_title_prefix: &str,
    destination: ExtractDestination,
) -> Result<ExtractedNoteDto, String> {
    // Assemble note body.
    let body_html = crate::llm::markdown::assemble_note_body(&envelope, source_text);

    // Derive title: override → envelope.title → first H2 from markdown → date.
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| envelope.title.clone())
        .or_else(|| crate::llm::markdown::derive_title_from_markdown(&envelope.lessons_markdown))
        .unwrap_or_else(|| format!("{default_title_prefix} — {}", chrono::Local::now().format("%Y-%m-%d")));

    // Decide where the note lives. May create `Notes/Inbox` (a dirty_new
    // folder row) — only after the LLM call succeeded, so a cancelled or
    // failed extract leaves no empty Inbox behind.
    let can_create = account_can_create_folders(state, account_id);
    let folder = match &destination {
        ExtractDestination::Resolve => {
            crate::llm::filing::resolve_destination(&state.db, account_id, can_create)
        }
        ExtractDestination::BesideSource(source_label) => {
            crate::llm::filing::destination_beside(&state.db, account_id, source_label, can_create)
        }
    }
    .map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(request_id);
        format!("resolve destination: {e}")
    })?;

    // Create a brand-new local note. apply_local_edit is UPDATE-only — we have
    // to insert_local_new for a freshly-generated UUID.
    // Per-backend shape (gotcha #18): a CloudKit recordName is lowercase.
    let uuid = crate::backend::mint_uuid_for(account.backend_kind);
    let now = db::now_ms();
    let new_note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: title.clone(),
        body_html,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder.clone(),
        local_version: 1,
        remote_version: None,
        sync_state: db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
    };
    state.db.insert_local_new(&new_note).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(request_id);
        format!("insert_local_new: {e}")
    })?;

    // Tag persistence: NO explicit add_tag calls here. v0.15.x made the body
    // the single source of truth — reconcile_tags_from_body_conn runs inside
    // insert_local_new (via save_note's path) and derives the tag set from
    // the inline <p>#tag</p> line we wrote into the body in assemble_note_body.
    // An earlier version of this code did call add_tag explicitly for each
    // envelope.tag, but the body-derived reconciliation overwrote those rows
    // a moment later — net effect was identical to letting the body parser
    // handle it, just with extra work and a brief race window. See db.rs's
    // tags_from_body + reconcile_tags_from_body_conn for the canonical path.

    // Success path: remove the in-flight cancel token. A late cancel call
    // arriving after this point becomes a no-op (the lookup misses), which
    // is the correct semantics — the work already completed.
    state.in_flight_extracts.lock().unwrap().remove(request_id);

    log!(
        "save_workflow_envelope: created note uuid={uuid} in {folder} with {} body-derived tag(s)",
        envelope.tags.len()
    );
    Ok(ExtractedNoteDto { uuid, label: folder })
}

#[derive(serde::Serialize)]
struct ActionItemsPreview {
    ai_result_id: String,
    title: String,
    body_html: String,
    before_html: Option<String>,
}

/// Read-only preview; B policy precedes source/target retrieval. D/E wrap the
/// existing ActionItems provider route. No receipt ID authorizes application.
#[tauri::command]
async fn preview_action_items(
    account_id: String,
    source_text: String,
    source_uuid: Option<String>,
    source_incomplete: Option<bool>,
    target_uuid: Option<String>,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ActionItemsPreview, String> {
    llm::receipts::run(&request_id, None, "preview_action_items", async {
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        let account = state
            .accounts
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == account_id)
            .cloned()
            .ok_or("Account unavailable")?;
        let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let _cleanup = AiRequestCleanup {
            requests: &state.in_flight_extracts,
            id: request_id.clone(),
        };
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());
        let read = |uuid: &str| -> Result<db::CachedNote, String> {
            let uuid = state
                .db
                .resolve_note_uuid(uuid, &account_id)
                .map_err(|e| e.to_string())?;
            state
                .db
                .get(&uuid, &account_id)
                .map_err(|e| e.to_string())?
                .filter(|n| n.sync_state != db::SyncState::DeletedPending)
                .ok_or("Note unavailable".into())
        };
        let source_is_html = source_uuid.is_some();
        let source = if let Some(uuid) = source_uuid {
            let note = read(&uuid)?;
            llm::receipts::source_version(
                &serde_json::to_vec(&(&account_id, &note.uuid, note.local_version))
                    .unwrap_or_default(),
            );
            // Preserve original HTML in the Source block, quote readable text.
            note.body_html
        } else {
            source_text
        };
        let target = target_uuid.as_deref().map(read).transpose()?;
        llm::meeting::passages(&source).map_err(|e| e.to_string())?;
        let text = if source_is_html {
            llm::meeting::text_from_html(&source)
        } else {
            source.clone()
        };
        let text = if source_incomplete.unwrap_or(false) {
            format!("[INCOMPLETE] Supplied source is incomplete.\n{text}")
        } else {
            text
        };
        if let Some(note) = &target {
            llm::receipts::source_version(
                &serde_json::to_vec(&(
                    &account_id,
                    &note.uuid,
                    note.local_version,
                    &note.body_html,
                ))
                .unwrap_or_default(),
            );
        }
        llm::meeting::request(&text).map_err(|e| e.to_string())?;
        llm::receipts::source_version(source.as_bytes());
        llm::receipts::metric("source_count", 1);
        llm::receipts::stage("action_items");
        let envelope = provider
            .run_workflow(llm::provider::WorkflowKind::ActionItems, &text, &[], cancel)
            .await
            .map_err(|e| e.to_string())?;
        let _gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let title = target
            .as_ref()
            .map(|n| n.title.clone())
            .or(title_override.filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "Meeting actions — draft".into());
        let body_html = llm::markdown::assemble_note_body(&envelope, &source);
        let before_html = target
            .as_ref()
            .map(|n| llm::markdown::sanitize_note_html(&n.body_html));
        let ai_result_id = issue_ai_result(&state, &account_id);
        let mut drafts = state.ai_policy.meeting_drafts.lock().unwrap();
        drafts.retain(|_, d| d.created.elapsed().as_secs() < 900);
        if drafts.len() >= 16 {
            drafts.clear();
        }
        drafts.insert(
            ai_result_id.clone(),
            llm::meeting::Draft {
                created: std::time::Instant::now(),
                account_id,
                source,
                envelope,
                title: title.clone(),
                target,
            },
        );
        llm::receipts::stage("awaiting_review");
        Ok(ActionItemsPreview {
            ai_result_id,
            title,
            body_html,
            before_html,
        })
    })
    .await
}

#[tauri::command]
fn discard_action_items(ai_result_id: String, state: State<'_, AppState>) {
    let _gate = state.ai_policy.gate.lock().unwrap();
    state
        .ai_policy
        .meeting_drafts
        .lock()
        .unwrap()
        .remove(&ai_result_id);
    state
        .ai_policy
        .results
        .lock()
        .unwrap()
        .remove(&ai_result_id);
}

#[tauri::command]
fn apply_action_items(
    account_id: String,
    ai_result_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let _gate = state.ai_policy.gate.lock().unwrap();
    validate_ai_result(&state, &account_id, &ai_result_id)?;
    let draft = state
        .ai_policy
        .meeting_drafts
        .lock()
        .unwrap()
        .remove(&ai_result_id)
        .ok_or("Draft expired; generate it again.")?;
    state
        .ai_policy
        .results
        .lock()
        .unwrap()
        .remove(&ai_result_id);
    if draft.account_id != account_id || draft.created.elapsed().as_secs() >= 900 {
        return Err("Draft expired; generate it again.".into());
    }
    if draft.target.is_some() {
        let (uuid, label) = llm::meeting::append_reviewed(&state.db, &draft)?;
        return Ok(ExtractedNoteDto { uuid, label });
    }
    let account = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .cloned()
        .ok_or("Account unavailable")?;
    save_workflow_envelope(
        &state,
        &account,
        &account_id,
        &ai_result_id,
        &draft.source,
        draft.envelope,
        Some(draft.title),
        "Meeting actions — draft",
        ExtractDestination::Resolve,
    )
}

/// Default title prefix for a fresh note created by `run_workflow_note_into`,
/// matching `extract_note_into`'s "Extract" default — see
/// `save_workflow_envelope`'s title derivation (override → envelope.title →
/// first H2 → `"{prefix} — {date}"`).
fn workflow_title_prefix(w: crate::llm::provider::WorkflowKind) -> &'static str {
    use crate::llm::provider::WorkflowKind;
    match w {
        WorkflowKind::Summarize => "Summary",
        WorkflowKind::ActionItems => "Action Items",
        WorkflowKind::ExpandBullets => "Expanded",
    }
}

/// Summarize / Action Items / Expand Bullets — the roadmap #2 sibling of
/// `extract_note`. Same refusal, same filing, same cancellation and
/// fallback-on-error behavior; differs only in which prompt the provider
/// runs (`run_workflow` instead of `extract`).
#[tauri::command]
async fn run_llm_workflow(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    if workflow == llm::provider::WorkflowKind::ActionItems {
        return Err("Action items require preview and explicit application.".into());
    }
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "run_llm_workflow", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        run_workflow_note_into(account_id, workflow, source_text, title_override, request_id, ExtractDestination::Resolve, state).await
    }).await
}

async fn run_workflow_note_into(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    destination: ExtractDestination,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
    log!(
        "run_llm_workflow: account={} workflow={:?} source_len={} request_id={}",
        account_id, workflow, source_text.len(), request_id
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());

    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).ok_or_else(|| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("account not found: {account_id}")
        })?.clone()
    };
    let provider = checked_account_provider(&state, &account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    let existing_tags: Vec<String> =
        state.db.list_all_tags(&account_id).map(|rows| rows.into_iter().map(|(tag, _)| tag).collect()).unwrap_or_default();

    llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
    llm::receipts::stage(match workflow {
        llm::provider::WorkflowKind::Summarize => "summarize",
        llm::provider::WorkflowKind::ActionItems => "action_items",
        llm::provider::WorkflowKind::ExpandBullets => "expand_bullets",
    });
    let envelope = match provider.run_workflow(workflow, &source_text, &existing_tags, cancel).await {
        Ok(env) => env,
        Err(crate::llm::provider::ExtractError::Cancelled) => {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("run_llm_workflow: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("run_llm_workflow: provider error (details omitted) — creating fallback note");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = create_fallback_source_note(&state, &account_id, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let uuid = fallback?;
            return Err(format!("LLM call failed; source preserved in note {uuid}. {e}"));
        }
    };

    let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
    llm::receipts::stage("saving_locally");
    save_workflow_envelope(
        &state, &account, &account_id, &request_id, &source_text, envelope,
        title_override, workflow_title_prefix(workflow), destination,
    )
}

/// Append variant, mirroring `append_extract_note` exactly — see its doc
/// comment for the full existence-check-before-and-after-the-LLM-call
/// reasoning, unchanged here. Only two things differ: the LLM call runs
/// `provider.run_workflow(workflow, ...)` instead of `provider.extract(...)`,
/// and the `request_id` bookkeeping happens under `run_llm_workflow`'s own
/// log prefix.
#[tauri::command]
async fn append_llm_workflow_note(
    account_id: String,
    workflow: crate::llm::provider::WorkflowKind,
    target_uuid: String,
    source_text: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if workflow == llm::provider::WorkflowKind::ActionItems {
        return Err("Action items require preview and explicit application.".into());
    }
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "append_llm_workflow_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        // Write::Notes only: appending to an existing note creates no folder.
        // (A fallback note, if the LLM fails, goes through resolve_destination,
        // which never creates a folder the backend cannot write.)
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        log!(
            "append_llm_workflow_note: account={} workflow={:?} target={} source_len={} request_id={}",
            account_id,
            workflow,
            target_uuid,
            source_text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        // Existence check ONLY — before spending an LLM call, confirm the target
        // note is real. Deliberately NOT reused for the write below: the LLM call
        // can run long enough for the note to change underneath us, so the actual
        // write re-fetches fresh (see step below).
        match state.db.get(&target_uuid, &account_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("target note not found: {target_uuid}"));
            }
            Err(e) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("get target note: {e}"));
            }
        }

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Roadmap #0: same tag-vocabulary nudge as `run_workflow_note_into`.
        let existing_tags: Vec<String> =
            state.db.list_all_tags(&account_id).map(|rows| rows.into_iter().map(|(tag, _)| tag).collect()).unwrap_or_default();

        llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
        llm::receipts::stage(match workflow {
            llm::provider::WorkflowKind::Summarize => "summarize",
            llm::provider::WorkflowKind::ActionItems => "action_items",
            llm::provider::WorkflowKind::ExpandBullets => "expand_bullets",
        });
        let envelope = match provider.run_workflow(workflow, &source_text, &existing_tags, cancel).await {
            Ok(env) => env,
            Err(crate::llm::provider::ExtractError::Cancelled) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                log!("append_llm_workflow_note: cancelled by user (request_id={request_id})");
                return Err("cancelled".to_string());
            }
            Err(e) => {
                log!("append_llm_workflow_note: provider error (details omitted) — creating fallback note");
                let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
                let fallback = create_fallback_source_note(&state, &account_id, &source_text);
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                let uuid = fallback?;
                return Err(format!(
                    "LLM call failed; source preserved in note {uuid}. {e}"
                ));
            }
        };

        // Existence pre-check — gives the fallback-note UX its "target
        // disappeared" message. The write itself goes through
        // apply_local_edit_with_retry below, which re-reads and recomputes the
        // append on every attempt rather than reusing this snapshot.
        if state.db.get(&target_uuid, &account_id).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("re-fetch target note: {e}")
        })?.is_none() {
            log!("append_llm_workflow_note: target note disappeared during LLM call — creating fallback note");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = create_fallback_source_note(&state, &account_id, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let uuid = fallback?;
            return Err(format!(
                "target note was deleted while extracting; source preserved in note {uuid}"
            ));
        }

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        apply_local_edit_with_retry(&state.db, &account_id, &target_uuid, 5, |existing| {
            let new_body = crate::llm::markdown::append_to_note_body(
                &existing.body_html,
                &envelope,
                &source_text,
            );
            Ok((existing.title.clone(), new_body))
        })
        .map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("apply_local_edit: {e}")
        })?;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        log!("append_llm_workflow_note: appended to note uuid={target_uuid}");
        Ok(target_uuid)
    }).await
}

/// Answer a question from the local cache. Read-only: touches no note,
/// folder, edge, or sidecar, and never calls Gmail.
#[tauri::command]
async fn ask_jodd(
    session_id: String,
    question: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ask::AskAnswer, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "ask_jodd", async {
        let stamp = ai_stamp(&state);
        let (session, turns) = {
            let mut sessions = state.ai_policy.sessions.lock().unwrap();
            let session = sessions.get_mut(&session_id).ok_or("Conversation expired. Reopen Ask Jodd.")?;
            let turns = session.begin_turn(&stamp, &question)?;
            (session.clone(), turns)
        };
        let cancel = session.cancel.child_token();
        state.in_flight_asks.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result: Result<ask::AskAnswer, String> = async {
            let inner = llm::resolve::resolve_app_provider().map_err(|e| e.to_string())?;
            let provider = llm::policy::CheckedProvider {
                cancel: Mutex::new(None),
                gate: &state.ai_policy.gate, inner, valid: Box::new(|| ai_stamp(&state) == stamp && !session.cancel.is_cancelled()),
            };
            // An allowlist from authoritative accounts, expanded to exclusions for
            // SQL. Unknown/orphaned database accounts fail closed too.
            let allowed: Vec<String> = state.accounts.lock().unwrap().iter()
                .filter(|a| llm::policy::account_allowed(a)).map(|a| a.id.clone()).collect();
            let excluded = state.db.ai_excluded_accounts(&allowed).map_err(|e| e.to_string())?;
            ask::run::run_ask(&state.db, &provider, &session.scope, &turns, cancel.clone(), &excluded)
                .await.map_err(|e| e.to_string())
        }.await;
        state.in_flight_asks.lock().unwrap().remove(&request_id);
        let current = ai_stamp(&state);
        let mut sessions = state.ai_policy.sessions.lock().unwrap();
        let live = sessions.get_mut(&session_id).ok_or("Conversation expired. Start again.")?;
        live.finish_turn(&current, question, result.as_ref().ok().map(|a| a.markdown.as_str()), cancel.is_cancelled())?;
        result
    }).await
}

#[tauri::command]
fn cancel_ask(request_id: String, state: State<'_, AppState>) {
    if let Some(tok) = state.in_flight_asks.lock().unwrap().remove(&request_id) { tok.cancel(); }
}

/// Mirrors `jodd-mcp`'s `write_with_retry` — same race, different crate and
/// lookup (`Db::get`, not `note_by_uuid`, matching what these two callers
/// already used). Re-reads the note fresh on every attempt so a retry
/// recomputes against the latest body instead of resubmitting a stale one.
/// See `Db::apply_local_edit_versioned` for the underlying guarantee.
///
/// `Db::get` here (not `note_by_uuid`) is the deliberate opposite choice
/// from `jodd-mcp`'s `write_with_retry`: `note_by_uuid` filters out
/// `deleted_pending` rows so an external MCP write can't resurrect a note
/// the user just deleted, but neither of this function's callers (Extract
/// re-ingest's `append_extract_note`, auto-link's `apply_wiki_link_appends`)
/// can target a `deleted_pending` uuid in the first place — both derive
/// their target uuid from state that already excludes deleted notes, so the
/// `note_by_uuid` filter would be dead weight here, not a needed guard.
/// That asymmetry is exactly why these two crates keep separate retry
/// helpers instead of one shared one (see the plan's Global Constraints,
/// docs/superpowers/plans/2026-08-12-concurrent-local-writer-race.md, and
/// `write_with_retry`'s doc comment in jodd-mcp/src/write.rs).
fn apply_local_edit_with_retry(
    db: &db::Db,
    account_id: &str,
    uuid: &str,
    max_attempts: u32,
    mut compute: impl FnMut(&db::CachedNote) -> Result<(String, String), String>,
) -> Result<(), String> {
    for _ in 0..max_attempts {
        let existing = db
            .get(uuid, account_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("note '{uuid}' not found"))?;
        let (new_title, new_body) = compute(&existing)?;
        let applied = db
            .apply_local_edit_versioned(uuid, account_id, &new_title, &new_body, &existing.label, existing.local_version)
            .map_err(|e| e.to_string())?;
        if applied {
            return Ok(());
        }
    }
    Err(format!(
        "note '{uuid}' is being edited concurrently; giving up after {max_attempts} attempts"
    ))
}

/// Ingest a new source by appending the LLM's extraction to an EXISTING
/// note's body, instead of creating a new note (contrast `extract_note`).
/// Never renames, relabels, or otherwise restructures the target note — only
/// appends. See docs/superpowers/specs/2026-07-10-extract-ingest-entrypoint-design.md.
#[tauri::command]
async fn append_extract_note(
    account_id: String,
    target_uuid: String,
    source_text: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "append_extract_note", async {
        llm::receipts::source_version(serde_json::to_vec(&(&account_id, &source_text)).unwrap_or_default().as_slice());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        // Refuse a write this area can't accept yet, BEFORE SQLite, so no
        // unpushable row is created. See `refuse_write`.
        // Write::Notes only: appending to an existing note creates no folder.
        // (A fallback note, if the LLM fails, goes through resolve_destination,
        // which never creates a folder the backend cannot write.)
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        log!(
            "append_extract_note: account={} target={} source_len={} request_id={}",
            account_id,
            target_uuid,
            source_text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        // Existence check ONLY — before spending an LLM call, confirm the target
        // note is real. Deliberately NOT reused for the write below: the LLM call
        // can run long enough for the note to change underneath us, so the actual
        // write re-fetches fresh (see step below).
        match state.db.get(&target_uuid, &account_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("target note not found: {target_uuid}"));
            }
            Err(e) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                return Err(format!("get target note: {e}"));
            }
        }

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Roadmap #0: same tag-vocabulary nudge as `extract_note_into`.
        let existing_tags: Vec<String> =
            state.db.list_all_tags(&account_id).map(|rows| rows.into_iter().map(|(tag, _)| tag).collect()).unwrap_or_default();

        llm::receipts::source_version(&serde_json::to_vec(&(&account_id, &source_text, &existing_tags)).unwrap_or_default());
        llm::receipts::stage("extract");
        let envelope = match provider.extract(&source_text, &existing_tags, cancel).await {
            Ok(env) => env,
            Err(crate::llm::provider::ExtractError::Cancelled) => {
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                log!("append_extract_note: cancelled by user (request_id={request_id})");
                return Err("cancelled".to_string());
            }
            Err(e) => {
                log!("append_extract_note: provider error (details omitted) — creating fallback note");
                let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
                let fallback = create_fallback_source_note(&state, &account_id, &source_text);
                state.in_flight_extracts.lock().unwrap().remove(&request_id);
                let uuid = fallback?;
                return Err(format!(
                    "LLM call failed; source preserved in note {uuid}. {e}"
                ));
            }
        };

        // Existence pre-check — gives the fallback-note UX its "target
        // disappeared" message. The write itself goes through
        // apply_local_edit_with_retry below, which re-reads and recomputes the
        // append on every attempt rather than reusing this snapshot.
        if state.db.get(&target_uuid, &account_id).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("re-fetch target note: {e}")
        })?.is_none() {
            log!("append_extract_note: target note disappeared during LLM call — creating fallback note");
            let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
            let fallback = create_fallback_source_note(&state, &account_id, &source_text);
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            let uuid = fallback?;
            return Err(format!(
                "target note was deleted while extracting; source preserved in note {uuid}"
            ));
        }

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        apply_local_edit_with_retry(&state.db, &account_id, &target_uuid, 5, |existing| {
            let new_body = crate::llm::markdown::append_to_note_body(
                &existing.body_html,
                &envelope,
                &source_text,
            );
            Ok((existing.title.clone(), new_body))
        })
        .map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            format!("apply_local_edit: {e}")
        })?;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        log!("append_extract_note: appended to note uuid={target_uuid}");
        Ok(target_uuid)
    }).await
}

#[derive(serde::Serialize)]
struct LinkTargetDto {
    uuid: String,
    title: String,
    /// Rename-safe slug (title-slug + uuid8), matching `db::note_slug` — the
    /// same format the `[[` autocomplete picker inserts. Callers should
    /// build inserted wikilinks as `[[{slug}]]`, never `[[{title}]]`, so
    /// they survive a later note rename.
    slug: String,
}

#[derive(serde::Serialize)]
struct ProposedAppendDto {
    ai_result_id: String,
    uuid: String,
    title: String,
    addition_text: String,
}

#[derive(serde::Serialize)]
struct LinkSuggestionsResponse {
    ai_result_id: String,
    auto_links: Vec<LinkTargetDto>,
    proposed_appends: Vec<ProposedAppendDto>,
}

/// Suggest wiki links for `text` — either the just-created/updated note's
/// own body (#2a, called after extract_note/append_extract_note
/// succeeds) or an existing note's current body as-is (#2b, "Link into
/// wiki"). See docs/superpowers/specs/2026-07-20-auto-link-ingest-design.md.
/// Caller is responsible for auto-inserting `auto_links` into the relevant
/// note's body (a separate apply_local_edit call) and for presenting
/// `proposed_appends` to the user for confirmation before calling
/// apply_wiki_link_appends.
#[tauri::command]
async fn suggest_wiki_links(
    account_id: String,
    text: String,
    exclude_uuid: Option<String>,
    new_note_title: String,
    new_note_uuid: String,
    request_id: String,
    parent_request_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<LinkSuggestionsResponse, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, parent_request_id.as_deref(), "suggest_wiki_links", async {
        llm::receipts::source_version(account_id.as_bytes());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        log!(
            "suggest_wiki_links: account={} text_len={} request_id={}",
            account_id,
            text.len(),
            request_id
        );

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        let account = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| {
                    state.in_flight_extracts.lock().unwrap().remove(&request_id);
                    format!("account not found: {account_id}")
                })?
                .clone()
        };
        let provider = checked_account_provider(&state, &account).map_err(|e| {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            e.to_string()
        })?;

        // Finding F1 (2026-09-15 whole-branch review): `text` is the just-written
        // note's own body — an ingest note's `## Sources` list (full URLs, query
        // strings and all) and its verbatim Source block (up to 400 000 stored
        // chars) must never reach a provider. Cut before both, then cap what's
        // left the same way every other LLM input in this codebase is capped.
        let suggestion_text: String = crate::llm::markdown::text_for_suggestions(&text)
            .chars()
            .take(crate::llm::markdown::SUGGESTION_TEXT_CHARS)
            .collect();

        let result = crate::llm::autolink::suggest_links(
            provider.as_ref(),
            &state.db,
            &account_id,
            exclude_uuid.as_deref(),
            &new_note_title,
            &new_note_uuid,
            &suggestion_text,
            cancel,
        )
        .await;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        let _policy_gate = provider.mutation_guard().map_err(|e| e.to_string())?;
        let ai_result_id = issue_ai_result(&state, &account_id);
        match result {
            Ok(suggestions) => Ok(LinkSuggestionsResponse {
                ai_result_id: ai_result_id.clone(),
                auto_links: suggestions
                    .auto_links
                    .into_iter()
                    .map(|t| {
                        let slug = crate::db::note_slug(&t.title, &t.uuid);
                        LinkTargetDto { uuid: t.uuid, title: t.title, slug }
                    })
                    .collect(),
                proposed_appends: suggestions
                    .proposed_appends
                    .into_iter()
                    .map(|a| ProposedAppendDto { ai_result_id: ai_result_id.clone(), uuid: a.uuid, title: a.title, addition_text: a.addition_text })
                    .collect(),
            }),
            Err(crate::llm::provider::ExtractError::Cancelled) => Err("cancelled".to_string()),
            Err(e) => Err(e.to_string()),
        }
    }).await
}

/// Propose an existing folder for note `uuid` — after an Extract, and from
/// the note context menu's "Suggest folder". Read-only (no `refuse_write`):
/// accepting the proposal is a separate `move_notes_batch`.
///
/// The caller decides how loud each outcome is (spec "Suggestion" table), so
/// this returns the outcome as-is. The cancel token is removed on every exit
/// path: everything fallible runs inside one block, then the removal.
#[tauri::command]
async fn suggest_note_folder(
    account_id: String,
    uuid: String,
    request_id: String,
    parent_request_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, parent_request_id.as_deref(), "suggest_note_folder", async {
        llm::receipts::source_version(account_id.as_bytes());
        let _request_cleanup = AiRequestCleanup { requests: &state.in_flight_extracts, id: request_id.clone() };
        let result_stamp = ai_stamp(&state);
        log!("suggest_note_folder: account={} uuid={} request_id={}", account_id, uuid, request_id);

        let cancel = tokio_util::sync::CancellationToken::new();
        state
            .in_flight_extracts
            .lock()
            .unwrap()
            .insert(request_id.clone(), cancel.clone());

        let result: Result<crate::llm::filing::FolderSuggestionOutcome, String> = async {
            let account = {
                let list = state.accounts.lock().unwrap();
                list.iter()
                    .find(|a| a.id == account_id)
                    .cloned()
                    .ok_or_else(|| format!("account not found: {account_id}"))?
            };
            let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
            crate::llm::filing::suggest_folder(provider.as_ref(), &state.db, &account_id, &uuid, cancel.clone())
                .await
                .map_err(|e| match e {
                    crate::llm::provider::ExtractError::Cancelled => "cancelled".to_string(),
                    other => other.to_string(),
                })
        }
        .await;

        state.in_flight_extracts.lock().unwrap().remove(&request_id);

        let mut value = serde_json::to_value(result?).map_err(|e| e.to_string())?;
        // The provider was checked above; validate the run's original policy once
        // more while issuing eligibility for a later user-confirmed move.
        let _policy_gate = state.ai_policy.gate.lock().unwrap();
        if cancel.is_cancelled() || ai_stamp(&state) != result_stamp { return Err("AI settings changed. Generate the suggestion again.".into()); }
        if value["kind"] == "suggested" { value["ai_result_id"] = issue_ai_result(&state, &account_id).into(); }
        Ok(value)
    }).await
}

#[derive(serde::Deserialize)]
struct ConfirmedAppend {
    ai_result_id: String,
    uuid: String,
    addition_text: String,
}

/// Apply confirmed append suggestions from suggest_wiki_links — one
/// apply_local_edit per note, pure concatenation onto the existing body
/// (never rewrites existing content, design spec decision 6). Skips (rather
/// than aborts on) any target that's disappeared since the suggestion was
/// made; returns the uuids that were successfully updated.
#[tauri::command]
fn apply_wiki_link_appends(
    account_id: String,
    appends: Vec<ConfirmedAppend>,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    for append in &appends { validate_ai_result(&state, &account_id, &append.ai_result_id)?; }
    let mut applied = Vec::new();
    for a in appends {
        let addition = a.addition_text.clone();
        match apply_local_edit_with_retry(&state.db, &account_id, &a.uuid, 5, |existing| {
            Ok((existing.title.clone(), format!("{}\n<p>{}</p>", existing.body_html, addition)))
        }) {
            Ok(()) => applied.push(a.uuid),
            Err(e) => log!("apply_wiki_link_appends: apply_local_edit {} failed: {e}", a.uuid),
        }
    }
    Ok(applied)
}

/// Cancel an in-flight extract_note call. The frontend passes the same
/// request_id it gave to extract_note; we look up the CancellationToken
/// and fire it. The extract_note command's tokio::select! sees the token
/// fire and unwinds (HTTP: drops the in-flight reqwest future; Claude CLI:
/// kills the child process). Returns Ok(true) if a token was found and
/// cancelled; Ok(false) if the request_id wasn't registered (likely the
/// extract already completed). Never returns Err — cancellation is
/// idempotent and best-effort.
#[tauri::command]
fn cancel_extraction(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let token = state.in_flight_extracts.lock().unwrap().remove(&request_id);
    match token {
        Some(t) => {
            t.cancel();
            log!("cancel_extraction: cancelled request_id={request_id}");
            Ok(true)
        }
        None => {
            log!("cancel_extraction: no in-flight request with id={request_id}");
            Ok(false)
        }
    }
}

/// Re-run lesson extraction on the preserved Source section of an existing
/// note. Creates a NEW note beside the source (does NOT overwrite it — spec
/// Decision 5) so the user can compare the two and delete either.
#[tauri::command]
async fn re_extract_note(
    account_id: String,
    uuid: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "re_extract_note", async {
        llm::receipts::source_version(account_id.as_bytes());
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        // The menu may hold a pre-rekey uuid (gotcha #16); follow its alias.
        let uuid = state
            .db
            .resolve_note_uuid(&uuid, &account_id)
            .map_err(|e| format!("resolve note uuid: {e}"))?;
        let note = state
            .db
            .get(&uuid, &account_id)
            .map_err(|e| format!("get note: {e}"))?
            .ok_or_else(|| format!("note not found: {uuid}"))?;

        let source = crate::llm::markdown::extract_source(&note.body_html)
            .ok_or_else(|| "note has no Source section to re-extract from".to_string())?;

        // A URL-ingest note holds one or more sources in one block (spec
        // Decision 8): re-run map-reduce over the stored text, never refetching.
        // Finding F2 (2026-09-15 whole-branch review): a `1 of 1` block can only
        // come from `ingest_to_note` itself (Decision 6: one map call, no reduce,
        // for a single source) — the ORIGINAL Extract path (`extract_note_into`)
        // never writes this header shape at all. Routing every recognised block
        // through here, not just `> 1`, is what gives a single-source ingest note
        // the same no-refetch, no-map-cap-bypass Re-extract as a multi-source one.
        if let Some(sources) = crate::ingest::stored::parse_sources(&source).filter(|s| !s.is_empty()) {
            let cancel = tokio_util::sync::CancellationToken::new();
            state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
            let result = run_ingest_command(
                &state,
                &account_id,
                crate::ingest::run::IngestInput::Stored(sources),
                String::new(),
                None,
                crate::ingest::run::IngestDestination::BesideSource(note.label),
                cancel,
                &|_| {},
            )
            .await;
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            return result;
        }

        extract_note_into(
            account_id,
            source,
            None,
            request_id,
            ExtractDestination::BesideSource(note.label),
            state,
        )
        .await
    }).await
}

// ─── URL ingest ──────────────────────────────────────────────────────────────
// docs/superpowers/specs/2026-09-15-url-ingest-design.md

#[derive(serde::Serialize)]
struct DuplicateOwnerDto {
    uuid: String,
    title: String,
}

#[derive(serde::Serialize)]
struct IngestSourceDto {
    url: String,
    kind: &'static str,
    supported: bool,
    reason: Option<String>,
    duplicate_owner: Option<DuplicateOwnerDto>,
}

#[derive(serde::Serialize)]
struct IngestAnalysisDto {
    sources: Vec<IngestSourceDto>,
    mostly_urls: bool,
    context_text: String,
    context_chars: usize,
}

/// What the Extract modal's "Sources from links" section shows. No network.
///
/// `is_html`: existing-note mode passes a `body_html`. URLs are detected in
/// the RAW text (an `href` is a link even when its text is a title); the
/// context and the Decision 9 count use the stripped text.
#[tauri::command]
fn analyze_ingest_sources(
    account_id: String,
    text: String,
    is_html: bool,
    exclude_uuids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<IngestAnalysisDto, String> {
    use crate::ingest::urls::{self, UrlKind};
    // Held uuids may predate a rekey (gotcha #16).
    let excluded: Vec<String> = exclude_uuids
        .iter()
        .map(|u| state.db.resolve_note_uuid(u, &account_id).unwrap_or_else(|_| u.clone()))
        .collect();
    let plain = if is_html { db::strip_html_to_text(&text) } else { text.clone() };
    let mut sources = Vec::new();
    for url in urls::detect(&text) {
        let (kind, supported, reason) = match urls::classify(&url) {
            UrlKind::Web => ("web", true, None),
            UrlKind::YouTube { .. } => ("youtube", true, None),
            UrlKind::Unsupported(r) => ("unsupported", false, Some(r)),
        };
        let duplicate_owner = state
            .db
            .find_citation_owner_excluding(&account_id, &url, &excluded)
            .map_err(|e| e.to_string())?
            .map(|n| DuplicateOwnerDto { uuid: n.uuid, title: n.title });
        sources.push(IngestSourceDto { url, kind, supported, reason, duplicate_owner });
    }
    let context_text = urls::context_text(&plain);
    Ok(IngestAnalysisDto {
        sources,
        mostly_urls: urls::is_mostly_urls(&plain),
        context_chars: context_text.chars().count(),
        context_text,
    })
}

/// Shared by `ingest_sources` and the multi-source `re_extract_note`: resolve
/// the provider, run `ingest::run::ingest_to_note`, map its errors to the
/// strings the modal already understands (`"cancelled"`).
#[allow(clippy::too_many_arguments)]
async fn run_ingest_command(
    state: &State<'_, AppState>,
    account_id: &str,
    input: crate::ingest::run::IngestInput,
    context: String,
    title_override: Option<String>,
    destination: crate::ingest::run::IngestDestination,
    cancel: tokio_util::sync::CancellationToken,
    progress: &(dyn Fn(crate::ingest::run::IngestProgress) + Send + Sync),
) -> Result<ExtractedNoteDto, String> {
    use crate::ingest::run::{ingest_to_note, map_input_cap, IngestError, IngestRequest};
    let account = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id).cloned().ok_or_else(|| format!("account not found: {account_id}"))?
    };
    let provider = checked_account_provider(&state, &account).map_err(|e| e.to_string())?;
    let req = IngestRequest {
        account_id: account_id.to_string(),
        backend_kind: account.backend_kind,
        can_create_folders: account_can_create_folders(state, account_id),
        context,
        title_override,
        destination,
        map_cap: map_input_cap(crate::llm::resolve::prompt_delivery_for_account(&account), std::env::consts::OS),
    };
    let fetcher = crate::ingest::HttpFetcher::default();
    match ingest_to_note(&state.db, provider.as_ref(), &fetcher, input, &req, cancel, progress).await {
        Ok(note) => {
            log!("ingest: created note uuid={} in {}", note.uuid, note.label);
            Ok(ExtractedNoteDto { uuid: note.uuid, label: note.label })
        }
        Err(IngestError::Cancelled) => {
            log!("ingest: cancelled by user");
            Err("cancelled".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Fetch the chosen links, condense each, combine them, write ONE note.
/// An explicit user-triggered remote operation, which the local-first
/// doctrine permits; the note itself is still one synchronous SQLite write.
/// Cancellable through the existing `cancel_extraction(request_id)`.
#[tauri::command]
async fn ingest_sources(
    account_id: String,
    urls: Vec<String>,
    context_text: String,
    title_override: Option<String>,
    request_id: String,
    on_progress: tauri::ipc::Channel<crate::ingest::run::IngestProgress>,
    state: State<'_, AppState>,
) -> Result<ExtractedNoteDto, String> {
    let receipt_request = request_id.clone();
    llm::receipts::run(&receipt_request, None, "ingest_sources", async {
        llm::receipts::source_version(account_id.as_bytes());
        refuse_write(&state, &account_id, backend::Write::Notes)?;
        if urls.is_empty() {
            return Err("Pick at least one link to ingest.".into());
        }
        if urls.len() > crate::ingest::MAX_URLS_PER_INGEST {
            return Err(format!("At most {} links can be ingested at once.", crate::ingest::MAX_URLS_PER_INGEST));
        }
        log!("ingest_sources: account={account_id} sources={} request_id={request_id}", urls.len());
        let cancel = tokio_util::sync::CancellationToken::new();
        state.in_flight_extracts.lock().unwrap().insert(request_id.clone(), cancel.clone());
        let result = run_ingest_command(
            &state,
            &account_id,
            crate::ingest::run::IngestInput::Urls(urls),
            context_text,
            title_override,
            crate::ingest::run::IngestDestination::Resolve,
            cancel,
            // A closed modal drops the channel; a failed send is not an ingest failure.
            &|p| {
                let _ = on_progress.send(p);
            },
        )
        .await;
        // One removal on every exit path — the orchestrator returns, never `?`s out of here.
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        result
    }).await
}

#[derive(serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    /// First ~500 chars of whatever came back, for diagnosing a Custom spec.
    pub raw_head: String,
    /// A named cause, when this CLI's measured failure modes recognise the
    /// error. `None` means show the raw text — never a guess.
    pub cause: Option<String>,
    /// What the user can do about `cause`. Always present when `cause` is.
    pub action: Option<String>,
}

/// Round-trip a configured provider with a fixed, representative workload
/// (`crate::llm::prompt::CONNECTION_TEST_SAMPLE`) — the same shape of input
/// Extract sees, not a greeting.
///
/// The dominant failure mode for a Custom agent-CLI spec is wrong flags: the
/// CLI waits for interactive input and the user sees "spun for two minutes,
/// then failed" *after* pasting real source text. This probe surfaces that in
/// one run of the real pipeline (structured output, retries, a multi-section
/// envelope) instead of the user's first real paste — at the real workload's
/// real cost, about 10-20 seconds, not a fixed-prompt shortcut. Works for any
/// provider kind, since it goes through the same resolvers the real
/// workflows use.
///
/// `account_id`:
///   - `Some(id)` → the §4.2 cascade for that account (Extract, auto-link)
///   - `None`     → the app-level provider (Ask Jodd), via resolve_app_provider
///
/// The `None` arm is what makes an app-level provider verifiable at all: Ask
/// Jodd is cross-account, so there is no account whose cascade would reach it.
#[tauri::command]
async fn test_llm_provider(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<LlmTestResult, String> {
    let receipt_request = uuid::Uuid::new_v4().to_string();
    llm::receipts::run(&receipt_request, None, "test_llm_provider", async {
        // `preset_id` is hoisted alongside `provider` (not computed separately
        // after the match) so both arms bind it from the exact config each one
        // resolved — an account's `preset_id` must come from its own cascade,
        // never from the app-level config the `None` arm reads.
        let (provider, preset_id) = match account_id {
            None => {
                let provider =
                    crate::llm::resolve::resolve_app_provider().map_err(|e| e.to_string())?;
                let preset_id = crate::app_llm_config::load()
                    .and_then(|c| crate::llm::resolve::agent_preset_id_of(&c.llm));
                (provider, preset_id)
            }
            Some(id) => {
                // Clone the account and drop the guard before any await:
                // `state.accounts` is a std Mutex and a guard held across an await
                // point is not Send.
                let account = {
                    let list = state.accounts.lock().unwrap();
                    list.iter()
                        .find(|a| a.id == id)
                        .cloned()
                        .ok_or_else(|| format!("account not found: {id}"))?
                };
                let provider = checked_account_provider(&state, &account)
                    .map_err(|e| e.to_string())?;
                // Goes through the same `effective_for_account` helper
                // `resolve_provider_for_account` uses internally, so the preset
                // id named here can never drift from the config the provider
                // above was actually built from.
                let preset_id = crate::llm::resolve::effective_for_account(&account)
                    .and_then(|e| crate::llm::resolve::agent_preset_id_of(&e.llm));
                (provider, preset_id)
            }
        };

        let started = std::time::Instant::now();
        let cancel = tokio_util::sync::CancellationToken::new();
        // No account tag vocabulary to show here — this probes connectivity
        // only, not a real extraction into a specific account.
        llm::receipts::stage("connection_test");
        let result = provider
            .extract(crate::llm::prompt::CONNECTION_TEST_SAMPLE, &[], cancel)
            .await;
        let elapsed_ms = started.elapsed().as_millis() as u64;

        let result = match result {
            Ok(env) => match env.usable() {
                Ok(()) => LlmTestResult {
                    ok: true,
                    elapsed_ms,
                    error: None,
                    raw_head: env.lessons_markdown.chars().take(500).collect(),
                    cause: None,
                    action: None,
                },
                Err(reason) => {
                    // `lessons_markdown` is empty (that's `reason`), but the
                    // envelope may still carry `title`/`tags`/`confidence` — the
                    // difference between "the model answered into the wrong
                    // field" and "nothing came back at all". Surface it instead
                    // of throwing it away, same ~500-char cap as the other sites.
                    let raw_head: String = format!(
                        "title={:?} tags={:?} confidence={:?}",
                        env.title, env.tags, env.confidence
                    )
                    .chars()
                    .take(500)
                    .collect();
                    LlmTestResult {
                        ok: false,
                        elapsed_ms,
                        error: Some(reason),
                        raw_head,
                        cause: None,
                        action: None,
                    }
                }
            },
            Err(e) => {
                let raw_head = match &e {
                    crate::llm::provider::ExtractError::MalformedEnvelope { raw, .. } => {
                        raw.chars().take(500).collect()
                    }
                    _ => String::new(),
                };
                let text = e.to_string();
                let diag = preset_id
                    .as_deref()
                    .and_then(|id| crate::llm::agent_cli::diagnose(id, &text));
                LlmTestResult {
                    ok: false,
                    elapsed_ms,
                    error: Some(text),
                    raw_head,
                    cause: diag.as_ref().map(|d| d.cause.clone()),
                    action: diag.as_ref().map(|d| d.action.clone()),
                }
            }
        };
        if !result.ok { llm::receipts::check("workflow_result_failed"); }
        Ok(result)
    }).await
}

/// Enumerate shipped agent-CLI presets with live PATH-detection status.
///
/// The preset list lives only in Rust; the settings UI is generated from this
/// response, so adding a CLI is a one-row edit in `llm/presets.rs`.
#[tauri::command]
fn list_agent_cli_presets() -> Vec<crate::llm::presets::AgentCliPresetInfo> {
    crate::llm::presets::preset_infos()
}

/// Read the per-account LlmConfig. Frontend uses this to populate the LLM
/// settings modal.
#[tauri::command]
fn get_llm_settings(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<crate::accounts::LlmConfig, String> {
    let list = state.accounts.lock().unwrap();
    list.iter()
        .find(|a| a.id == account_id)
        .map(|a| a.llm.clone())
        .ok_or_else(|| format!("account not found: {account_id}"))
}

/// Persist LlmConfig to accounts.json and (optionally) write or clear the
/// API key in the OS keychain. The API key never enters accounts.json.
///
/// `api_key` semantics:
///   - `None`             → leave keychain untouched
///   - `Some("")` (blank) → delete keychain entry
///   - `Some(key)`        → write key to keychain
#[tauri::command]
fn update_llm_settings(
    account_id: String,
    mut cfg: crate::accounts::LlmConfig,
    api_key: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    // Mutate in-memory under the lock, clone for I/O, release lock before
    // touching disk. Local-first doctrine: in-memory state is updated
    // synchronously; the disk write follows.
    let snapshot = {
        let mut list = state.accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("account not found: {account_id}"))?;
        llm::policy::preserve_permission(&acct.llm, &mut cfg);
        acct.llm = cfg;
        list.clone()
    };

    crate::accounts::save_accounts(&snapshot).map_err(|e| format!("save accounts: {e}"))?;

    if let Some(key) = api_key {
        if key.trim().is_empty() {
            crate::accounts::delete_llm_api_key(&account_id);
        } else {
            crate::accounts::write_llm_api_key(&account_id, &key)?;
        }
    }
    Ok(())
}

// ─── App-level OAuth credential config (BYO credentials) ─────────────────────

#[derive(serde::Serialize)]
struct OAuthConfigStatus {
    client_id: String,
    has_secret: bool,
    /// True when ANY credentials are available via the 3-tier resolution chain
    /// (user-configured OR compile-time embedded OR runtime env). AuthScreen
    /// uses this to decide whether to enable the Gmail sign-in button.
    credentials_available: bool,
}

#[tauri::command]
fn get_oauth_config() -> OAuthConfigStatus {
    // One read per source, two answers from each. The `*_from` variants take the
    // configured tier as an argument precisely so this command doesn't have to
    // load it twice: `client_id`/`has_secret` report what the user stored (for
    // the Settings fields), while `credentials_available` asks whether a
    // credential exists at ANY tier (stored, or embedded at compile time) to
    // decide whether sign-in is possible at all. Calling `auth::client_id()` and
    // `auth::client_secret()` for the second pair re-read `google_oauth.json`
    // and the `oauth_client_secret::google` keychain entry respectively — and
    // AuthScreen invokes this on mount.
    let configured_id = oauth_config::load().map(|c| c.client_id);
    let configured_secret = oauth_config::load_secret();
    let client_id = configured_id.clone().unwrap_or_default();
    let has_secret = configured_secret.is_some();
    let credentials_available = !auth::client_id_from(configured_id.as_deref()).is_empty()
        && !auth::client_secret_from(configured_secret.as_deref()).is_empty();
    OAuthConfigStatus { client_id, has_secret, credentials_available }
}

/// The four outcomes of submitting a (client_id, client_secret) pair.
///
/// A Google client secret belongs to exactly one client_id. Storing them in two
/// places (`google_oauth.json` + keychain) means nothing structurally forbids a
/// mismatched pair, and a mismatch fails auth in a way that reads as a Jodd bug,
/// not as a credential mistake — so the pair is validated before either half is
/// written.
#[derive(Debug, PartialEq, Eq)]
enum CredWrite {
    /// Empty client_id — the user is removing their credentials.
    ClearBoth,
    /// Same client_id as stored, blank secret field: the stored secret still
    /// belongs to this id, so keep it. This is what makes "leave blank to keep"
    /// safe.
    KeepSecret,
    Both,
    /// A new or changed client_id with no secret to go with it.
    RejectMissingSecret,
}

fn plan_cred_write(
    id: &str,
    secret: &str,
    stored_id: Option<&str>,
    has_stored_secret: bool,
) -> CredWrite {
    if id.is_empty() {
        return CredWrite::ClearBoth;
    }
    if !secret.is_empty() {
        return CredWrite::Both;
    }
    if stored_id == Some(id) && has_stored_secret {
        CredWrite::KeepSecret
    } else {
        CredWrite::RejectMissingSecret
    }
}

#[tauri::command]
fn save_oauth_config(client_id: String, client_secret: String) -> Result<(), String> {
    let id = client_id.trim();
    let secret = client_secret.trim();
    let stored = oauth_config::load().map(|c| c.client_id);

    match plan_cred_write(id, secret, stored.as_deref(), oauth_config::load_secret().is_some()) {
        CredWrite::ClearBoth => {
            oauth_config::clear()?;
            oauth_config::clear_secret()?;
        }
        CredWrite::KeepSecret => oauth_config::save(id)?,
        CredWrite::Both => {
            // Secret first: if the id write then fails, the leftover is a secret
            // with no id — which resolves to "not configured" and prompts a
            // re-entry. The reverse order would leave the mismatch this guards
            // against.
            oauth_config::save_secret(secret)?;
            oauth_config::save(id)?;
        }
        CredWrite::RejectMissingSecret => {
            return Err(
                "Client secret is required — it must be the one issued for this Client ID."
                    .to_string(),
            );
        }
    }
    Ok(())
}

#[tauri::command]
fn clear_oauth_config() -> Result<(), String> {
    oauth_config::clear()?;
    oauth_config::clear_secret()?;
    Ok(())
}

#[derive(serde::Serialize)]
struct MsOAuthConfigStatus {
    /// The user-configured id only — empty when the resolved id is coming from
    /// the env var or the embedded value instead. The Settings field must show
    /// what the user typed, not what Jodd resolved, or clearing it looks broken.
    client_id: String,
    /// True when *any* tier resolves to a non-empty id, so the sign-in button
    /// can be enabled on a build that embeds one. Mirrors
    /// `OAuthConfigStatus::credentials_available`, minus the secret half —
    /// Microsoft is a public client and has none.
    credentials_available: bool,
}

#[tauri::command]
fn get_ms_oauth_config() -> MsOAuthConfigStatus {
    MsOAuthConfigStatus {
        client_id: oauth_config::load_ms_client_id().unwrap_or_default(),
        credentials_available: !auth_ms::client_id().is_empty(),
    }
}

/// Save or clear the user-configured Microsoft client id.
///
/// No counterpart to `plan_cred_write` exists here on purpose: that machinery
/// guards a Google *secret* against being paired with the wrong id, and a public
/// client has no secret to mispair. An empty submission clears the file, which
/// drops resolution back to the env var or the embedded id rather than
/// disabling Microsoft sign-in.
///
/// Changing the id does **not** re-authenticate existing Microsoft accounts:
/// their refresh tokens were issued to the previous client and will be refused
/// on the next refresh, surfacing through the normal `is_unauthorized_error`
/// re-auth path. That is exactly what changing the Google client id already
/// does, and inventing a bespoke invalidation for one provider would be the
/// inconsistency, not the fix.
#[tauri::command]
fn save_ms_oauth_config(client_id: String) -> Result<(), String> {
    let id = client_id.trim();
    if id.is_empty() {
        oauth_config::clear_ms_client_id()
    } else {
        oauth_config::save_ms_client_id(id)
    }
}

#[derive(serde::Serialize)]
struct AppLlmConfigStatus {
    cfg: app_llm_config::AppLlmConfig,
    /// Never returns the key itself — only whether one is stored, matching
    /// get_oauth_config's has_secret contract.
    has_api_key: bool,
}

#[tauri::command]
fn get_app_llm_config() -> AppLlmConfigStatus {
    AppLlmConfigStatus {
        cfg: app_llm_config::load().unwrap_or_default(),
        has_api_key: app_llm_config::load_secret().is_some(),
    }
}

/// Provider-only probe: same non-secret half as `get_app_llm_config`, minus
/// the keychain read. Ask Jodd only needs `cfg.llm.provider` to decide
/// whether to show the "not configured" empty state, but it re-probes on
/// every modal open (the user may have just fixed it in App Settings) — so
/// routing that through `get_app_llm_config` hit `app_llm_config::load_secret()`
/// on every open and cost a Keychain prompt each time (gotcha #15's pattern:
/// a frequently-invoked read that looks pure but bottoms out in the OS
/// credential store). Settings' "is a key already stored" checkbox is the one
/// caller that actually needs `has_api_key`, so it keeps using
/// `get_app_llm_config`.
#[tauri::command]
fn get_app_llm_provider() -> accounts::LlmConfig {
    app_llm_config::load().unwrap_or_default().llm
}

/// `api_key`: Some("") clears the stored key, None leaves it untouched
/// (so the UI can save other fields without re-entering the secret).
#[tauri::command]
fn set_app_llm_config(
    cfg: app_llm_config::AppLlmConfig,
    api_key: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    app_llm_config::save(&cfg)?;
    match api_key.as_deref() {
        None => {}
        Some("") => app_llm_config::clear_secret()?,
        Some(k) => app_llm_config::save_secret(k.trim())?,
    }
    log!(
        "set_app_llm_config: provider={:?} apply_to_accounts={}",
        cfg.llm.provider,
        cfg.apply_to_accounts
    );
    Ok(())
}

// ─── Diagnostics: persistent file logging toggle ──────────────────────────

#[derive(serde::Serialize)]
struct LogSettingsStatus {
    file_logging_enabled: bool,
    /// Resolved path so the UI can show "logs saved to: X" and offer to
    /// reveal it, without duplicating the path-resolution logic client-side.
    log_file_path: String,
    /// Current size of the log file in bytes, so the UI can show growth
    /// ("N KB") and the user knows when "Clear log" is worth reaching for.
    log_file_size_bytes: u64,
}

#[tauri::command]
fn get_log_settings() -> LogSettingsStatus {
    LogSettingsStatus {
        file_logging_enabled: applog::is_enabled(),
        log_file_path: applog::log_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        log_file_size_bytes: applog::log_file_size(),
    }
}

#[tauri::command]
fn set_file_logging_enabled(enabled: bool) -> Result<(), String> {
    applog::set_enabled(enabled)
}

/// User-triggered reset from the Diagnostics UI — reclaims space immediately
/// instead of waiting for the automatic 20MB rotation.
#[tauri::command]
fn clear_log_file() -> Result<(), String> {
    applog::clear_log()
}

// `secrets_self_test` and `secrets_probe` lived here: hand-invoked commands
// that wrote, read and deleted a probe credential to prove the Android store
// survives process death (the keyring 2 in-memory `mock` failure). Their own
// doc comments said to remove them once the gate had passed on real hardware,
// and it has — write / force-stop / relaunch / read came back clean on an
// Infinix X6821 and a Galaxy S23 FE, and the read phase was checked against a
// wiped store first so a false pass was ruled out.
//
// Removed rather than kept behind `cfg(debug_assertions)`: they were reachable
// from the WebView in release builds, which is unearned write and delete access
// to the credential store for a gate that is finished. `secrets::self_test()`
// stays as a plain function — the unit tests use it, where its cost is nothing.

/// Doctrine compliance: if the LLM call fails, we still owe the user a note
/// preserving the verbatim source paste so they can retry or recover by hand.
fn create_fallback_source_note(
    state: &AppState,
    account_id: &str,
    source: &str,
) -> Result<String, String> {
    let folder = crate::llm::filing::resolve_destination(
        &state.db,
        account_id,
        account_can_create_folders(state, account_id),
    )
    .map_err(|e| format!("resolve destination: {e}"))?;
    let body = format!(
        "<p><em>Extraction failed. Source preserved below.</em></p>\n<hr>\n\
         <details open>\n<summary>Source (verbatim)</summary>\n<pre>{}</pre>\n</details>\n",
        crate::llm::markdown::escape_html(source)
    );
    // Per-backend shape (gotcha #18): a CloudKit recordName is lowercase.
    let backend_kind = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind)
        .ok_or_else(|| format!("account not found: {account_id}"))?;
    let uuid = crate::backend::mint_uuid_for(backend_kind);
    let title = format!(
        "Source (extraction failed) — {}",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let now = db::now_ms();
    let new_note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.to_string(),
        id: String::new(),
        title,
        body_html: body,
        date: chrono::Local::now().to_rfc2822(),
        x_mail_created_date: None,
        label: folder,
        local_version: 1,
        remote_version: None,
        sync_state: db::SyncState::Dirty,
        last_synced_at: None,
        last_local_modified_at: now,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        push_blocked_reason: None,
    };
    state
        .db
        .insert_local_new(&new_note)
        .map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(uuid)
}

/// Gotcha #18's mirror half — the uuid Jodd MINTS — for the two Extract
/// create sites. `format_apple_uuid` uppercases, which is right for Apple's
/// email wire format and wrong for a CloudKit `recordName`; every site that
/// produces a note the worker will CREATE must go through
/// `backend::mint_uuid_for(kind)`. These are Tauri-state functions with no
/// unit-test harness, so the invariant is pinned the way jodd-mcp pins
/// `do_create_note`'s write checks: by reading the function's own source.
#[cfg(test)]
mod extract_mint_tests {
    #[test]
    fn extract_create_sites_mint_through_the_backend_policy() {
        let src = include_str!("lib.rs");
        // `extract_note_into` used to mint inline; Task 4 (save_workflow_envelope)
        // moved that call into the shared tail it now delegates to — pin the
        // invariant at its new site, not the delegator. `save_workflow_envelope`
        // covers every "LLM envelope -> saved note" workflow, Extract included.
        for signature in ["fn save_workflow_envelope(", "fn create_fallback_source_note("] {
            let body = crate::test_support::extract_fn_body(src, signature);
            assert!(
                body.contains("mint_uuid_for("),
                "`{signature}` must mint its note uuid with backend::mint_uuid_for (gotcha #18)"
            );
            assert!(
                !body.contains("format_apple_uuid("),
                "`{signature}` must not mint with format_apple_uuid — it uppercases a CloudKit recordName (gotcha #18)"
            );
        }
    }
}

#[cfg(test)]
mod save_workflow_envelope_tests {
    #[test]
    fn extract_note_into_delegates_its_tail_to_save_workflow_envelope() {
        let src = include_str!("lib.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn extract_note_into(");
        assert!(
            body.contains("save_workflow_envelope("),
            "extract_note_into must delegate title/folder/uuid/insert to the \
             shared helper, not duplicate that logic inline"
        );
    }

    #[test]
    fn save_workflow_envelope_mints_a_uuid_and_never_formats_one() {
        let src = include_str!("lib.rs");
        let body = crate::test_support::extract_fn_body(src, "fn save_workflow_envelope(");
        assert!(body.contains("mint_uuid_for("), "must mint via the per-backend rule (gotcha #18)");
        assert!(!body.contains("format_apple_uuid("), "must not assume the Apple wire format");
    }
}

/// `run_llm_workflow` is new code (unlike `save_workflow_envelope`'s uuid
/// minting, which `extract_note_into` already exercised before Task 4 moved
/// it) — pin the refusal-before-write ordering explicitly rather than
/// inheriting it silently. Same rationale and technique as
/// `extract_mint_tests`: no unit-test harness for `State`-bound Tauri
/// commands, so the invariant is pinned by reading the function's own source.
#[cfg(test)]
mod run_llm_workflow_tests {
    #[test]
    fn run_llm_workflow_refuses_before_touching_sqlite() {
        let src = include_str!("lib.rs");
        let body = crate::test_support::extract_fn_body(src, "async fn run_llm_workflow(");
        let refuse_pos = body.find("refuse_write(").expect("must call refuse_write");
        let into_pos = body.find("run_workflow_note_into(").expect("must delegate to run_workflow_note_into");
        assert!(refuse_pos < into_pos, "refuse_write must run before the write path starts");
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Must run before the first `log!` call so the persisted file-logging
    // choice (default on) is honored from the very first line.
    applog::init();

    // Before anything is spawned. keyring-core will not create an Entry until
    // a store is registered, and a failure here means every credential read
    // and write in this process would fail — say so loudly rather than
    // discovering it later as "the user has to sign in again".
    // Desktop only. On Android the credential store cannot be registered this
    // early: it needs the JavaVM and Activity from tao, and tao has not filed
    // them yet when `run()` begins — verified on a device, which logged
    // "tao has no Android context yet". The crash backtrace showing
    // `ndk_glue::create -> _start_app` says only who called us, not that the
    // context was already recorded. The Android call moved into `.setup()`,
    // which runs after the Activity exists.
    #[cfg(not(target_os = "android"))]
    if let Err(e) = secrets::init() {
        log!("FATAL: credential store init failed: {}", e);
    }

    // Before anything resolves a binary. A Finder-launched app inherits
    // launchd's PATH, which contains none of the places agent CLIs install to.
    shell_path::adopt_login_shell_path();

    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".env");
    dotenv::from_path(&env_path).ok();
    dotenv::dotenv().ok();

    // Report on what will actually be used, not on one of the inputs.
    //
    // This used to read the `GOOGLE_CLIENT_ID` env var directly. On Android
    // that is always absent — the credential is baked in at compile time, the
    // `.env` path above is the BUILD machine's, and Android reads the
    // `*_ANDROID` pair anyway — so every launch logged "GOOGLE_CLIENT_ID not
    // set" while the binary held a perfectly good client id. During the App
    // Links debugging that false alarm cost real time, because it asserted the
    // opposite of the truth at exactly the moment the credential was suspect.
    //
    // `auth::client_id()` is the resolver every call site uses: stored
    // override → compile-time embedded → runtime env, with the platform's own
    // key names. An empty result here is the only condition that actually
    // breaks sign-in.
    if auth::client_id().is_empty() {
        log!(
            "WARNING: no OAuth client id resolved — sign-in will fail. \
             Set one in Settings, or build with credentials (dev .env path: {})",
            env_path.display()
        );
    } else {
        log!("OAuth client id resolved");
    }

    tauri::Builder::default()
        // **The hidden iCloud webview must not keep the app alive.** Tauri
        // quits when the window count reaches zero, and Component B4's session
        // webview is a window — an invisible one. Closing Jodd's last visible
        // window therefore left `jodd.exe` running with no UI and no way to
        // notice, until Task Manager. Measured on Windows 2026-09-09, and it
        // only became possible when iCloud started running here: macOS apps
        // outliving their windows is normal, so nobody would have seen it.
        //
        // Keyed on "no VISIBLE window remains" rather than on the main
        // window's label, so a future second visible window does not
        // reintroduce it. Closing the iCloud webviews then drops the count to
        // zero and Tauri's own exit path takes over — no `app.exit()` here,
        // which would bypass whatever else is listening for the real one.
        .on_window_event(|window, event| {
            use tauri::Manager;
            // Android has no window to outlive: `is_visible()` is unsupported
            // there, so this guard would act on an answer that means nothing —
            // and closing the sign-in webview mid-flow is the way it would show.
            if cfg!(target_os = "android") {
                return;
            }
            if !matches!(event, tauri::WindowEvent::Destroyed) {
                return;
            }
            let app = window.app_handle();
            let any_visible = app
                .webview_windows()
                .values()
                .any(|w| w.is_visible().unwrap_or(false));
            if any_visible {
                return;
            }
            for label in [icloud_auth::SIGNIN_WINDOW, icloud_auth::SESSION_WINDOW] {
                if let Some(w) = app.get_webview_window(label) {
                    let _ = w.close();
                    log!("icloud: closed '{label}' so the app can quit");
                }
            }
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .setup(|app| {
            // paths:: must be initialized before ANYTHING reads a file — on Android
            // there is no `dirs::` fallback, so accounts.json, google_oauth.json,
            // the SQLite cache and the log file are all unreachable until this runs.
            //
            // Deliberately `config_dir()`/`data_dir()`, NOT `app_config_dir()`/
            // `app_data_dir()`. The `app_*` variants append the bundle identifier
            // (`.../co.bbmedia.jodd`), one path segment below the HISTORICAL
            // `.../jodd` this app has always used — feeding paths::init() that
            // value orphans every existing install's accounts.json, because
            // paths::resolve() has no platform-aware handling of the explicit
            // value it's given (confirmed empirically: a fresh `co.bbmedia.jodd`
            // directory was created and 0 accounts loaded). The unsuffixed
            // `config_dir()`/`data_dir()` are exactly `dirs::config_dir()`/
            // `dirs::data_dir()` on desktop — byte-identical to paths.rs's own
            // fallback — and on Android resolve to the SAME native call as
            // `app_config_dir()`/`app_data_dir()` (Android's dir is already
            // app-private, so there is no separate "base" to append an
            // identifier to). One call site is correct on both platforms.
            let config_base = app
                .path()
                .config_dir()
                .map_err(|e| format!("config_dir: {e}"))?;
            let data_base = app
                .path()
                .data_dir()
                .map_err(|e| format!("data_dir: {e}"))?;
            paths::init(config_base, data_base);

            // Android's turn at the credential store. It cannot happen at the
            // top of `run()` like the desktop one does — registering the store
            // needs tao's JavaVM and Activity, and tao has not recorded them
            // that early. Here the Activity exists, and this still precedes
            // every credential reader: `migrate_legacy_keychain` and the sync
            // worker are both spawned below, and no command can run until
            // `.setup()` returns.
            #[cfg(target_os = "android")]
            if let Err(e) = secrets::init() {
                log!("FATAL: credential store init failed: {}", e);
            }

            // Re-run applog init: the call at the top of run() happens before the
            // Tauri app exists, so on Android it could not resolve a log path and
            // silently skipped opening the file (open_file() early-returns on a path
            // error). Calling init() again is safe — FILE_HANDLE is a OnceLock, so on
            // desktop, where the first call already succeeded, this is a no-op.
            applog::init();

            // NOTE: `accounts::load_accounts()` is deliberately NOT called here.
            // Opening the database below runs migration #19, which rewrites
            // `accounts.json` itself to `{backend}:{email}` ids — a list read
            // before that point is stale for the whole session, and every
            // `save_accounts` call site would write it back over the qualified
            // file. See `setup_loads_accounts_after_the_database_is_opened`.

            // Local SQLite replica. Lives in the platform's per-user app data dir —
            // never the working dir, so reinstalls don't wipe the cache. Falls back
            // to temp as a last resort: a volatile cache beats refusing to launch.
            // The temp-dir fallback is DESKTOP-ONLY. std::env::temp_dir() resolves to
            // $TMPDIR or /tmp, and on Android /tmp does not exist inside the app
            // sandbox and is not writable — so the fallback would fail to open, and
            // the .expect() below would panic the app at startup. On Android,
            // paths::data_base() is the only valid answer, and if it is unavailable
            // there is nothing to fall back TO: fail loudly rather than panic
            // obscurely three lines later.
            #[cfg(target_os = "android")]
            let data_dir = paths::data_base()
                .map(|d| d.join("jodd"))
                .ok_or("no app data dir on Android — paths::init did not run")?;
            #[cfg(not(target_os = "android"))]
            let data_dir = paths::data_base()
                .map(|d| d.join("jodd"))
                .unwrap_or_else(|| std::env::temp_dir().join("jodd"));
            let db = match db::Db::open(&data_dir) {
                Ok(d) => {
                    log!("local cache opened at {}", data_dir.display());
                    Arc::new(d)
                }
                Err(db_crypto::DbOpenError::KeyMismatchOrCorrupt) => {
                    log!(
                        "WARNING: local cache at {} could not be decrypted with the stored \
                         key — quarantining and starting a fresh encrypted cache",
                        data_dir.display()
                    );
                    match db_crypto::recover_from_key_mismatch(&data_dir) {
                        Ok(d) => Arc::new(d),
                        Err(e) => return Err(format!("recovery failed: {e}").into()),
                    }
                }
                Err(db_crypto::DbOpenError::AccountIdMigration(msg)) => {
                    // NOT the temp-dir fallback below. That fallback is right for
                    // "the cache is unusable" — it trades a volatile cache for a
                    // launch. This error means the opposite: the cache is intact
                    // and readable, and migration #19 refused to rewrite ids it
                    // could not resolve. Substituting an empty database would show
                    // the user zero notes and zero folders on every launch with
                    // one line in the log to explain it (gotcha #14's "do not
                    // lie", one layer down). Fail with the reason instead.
                    log!("FATAL: {}", msg);
                    return Err(format!(
                        "the local cache could not be migrated to backend-qualified \
                         account ids: {msg}. The cache itself is intact — nothing has \
                         been deleted. Restore accounts.json for the account named \
                         above, or move {} aside to start over with an empty cache.",
                        data_dir.join("jodd.sqlite3").display()
                    )
                    .into());
                }
                Err(e) => {
                    // Same reasoning as above: on Android there is no writable temp
                    // dir to retreat to, so `expect` here would be a startup panic
                    // with a misleading message. Return the real error instead.
                    #[cfg(target_os = "android")]
                    return Err(format!("failed to open local cache at {}: {e}", data_dir.display()).into());
                    #[cfg(not(target_os = "android"))]
                    {
                        log!("FATAL: failed to open local cache: {} — using temp dir", e);
                        let tmp = std::env::temp_dir().join("jodd");
                        Arc::new(db::Db::open_unencrypted(&tmp).expect("temp-dir DB open"))
                    }
                }
            };

            // Field list re-verified 2026-07-30. AppState gained `in_flight_asks`
            // with Ask Jodd, after this plan was first written. Copy the CURRENT
            // literal from run() rather than trusting this block — if the struct has
            // gained another field since, the compiler will say so.
            //
            // AppState is now managed here, inside `.setup()` (moved from the
            // builder's `.manage()` call for the Android path work), so "state
            // exists before any command runs" is a timing property of setup
            // ordering, not a structural guarantee the type system enforces.
            // `flush_sync` -> `sync_worker_tick` calls `app.state::<AppState>()`,
            // which panics rather than returning an error if state isn't managed
            // yet — worth knowing if `.setup()` is ever reordered.
            // Read the account list only NOW — after `Db::open` has run
            // migration #19. That migration rewrites `accounts.json` in place
            // (bare email -> `{backend}:{email}`), so a list loaded before it
            // holds ids that name nothing in the freshly-qualified cache: every
            // command would read zero rows, `index_account` would re-insert the
            // whole cache under the bare id, the worker would skip every
            // pre-existing dirty note, and the first `save_accounts` would put
            // the bare list back over the qualified file. Pinned by
            // `setup_loads_accounts_after_the_database_is_opened`.
            let accounts_list = accounts::load_accounts();
            log!(
                "loaded {} from persistence",
                accounts::account_census(&accounts_list)
            );

            app.manage(AppState {
                icloud_scans: Mutex::new(HashMap::new()),
                app_handle: app.handle().clone(),
                accounts: Mutex::new(accounts_list),
                account_states: Mutex::new(HashMap::new()),
                pending_pkce: Mutex::new(None),
                pending_backend: Mutex::new(accounts::BackendKind::default()),
                db,
                pushing: Mutex::new(std::collections::HashSet::new()),
                dup_stats: Mutex::new(HashMap::new()),
                in_flight_extracts: Mutex::new(HashMap::new()),
                in_flight_asks: Mutex::new(HashMap::new()),
                ai_policy: Default::default(),
                oauth_cancel: Mutex::new(None),
                sync_schedule: sync_schedule::Scheduler::default(),
                last_pull: Mutex::new(HashMap::new()),
            });

            // Android's OAuth redirect arrives as an Intent, not an HTTP request.
            // Two integration points exist for it: `get_current()` (reads the
            // launch URL directly — relevant on a cold launch, where the redirect
            // itself starts the process) and `on_open_url` (an event channel —
            // relevant on a warm re-entry, where the process is already running
            // and Android delivers a fresh Intent via `onNewIntent`).
            //
            // What follows is NOT verified against a running device — this
            // environment has no Android toolchain (see
            // .superpowers/sdd/2026-07-27-android-bringup/progress.md, Task 7's
            // open question). Based on reading `tauri-plugin-deep-link`'s Kotlin
            // side (`DeepLinkPlugin.kt`), the two paths look mutually exclusive
            // rather than double-firing on the same URL: cold launch calls
            // `load()`, which populates `currentUrl` for `get_current()` to read,
            // but the event channel isn't wired up yet at that point, so
            // `on_open_url` does not also fire for that URL; warm re-entry calls
            // `onNewIntent`, which emits through the now-wired channel to
            // `on_open_url`, but `get_current()` is never re-consulted once
            // `.setup()` has already run. If that reading is right, the dedup
            // below never actually trips in production — it guards a race this
            // plugin version doesn't have.
            //
            // It stays anyway: it's cheap, plugin behavior is version-dependent,
            // and the reading above is inference from source, not an on-device
            // observation. Dedup keys on the authorization `code`, not the URL
            // string or a boolean flag — a code is single-use by construction, so
            // two deliveries of one flow always carry the same code, while two
            // genuine sign-in attempts never do. `HashSet::insert` returns `false`
            // for a value already present, so `.insert(code.clone())` is an atomic
            // check-and-mark under the one lock: a genuine duplicate delivery
            // never reaches `complete_oauth` a second time (which would otherwise
            // surface "PKCE verifier missing" as an `oauth-error` right after a
            // successful sign-in — worse than a silent no-op, a visible error with
            // no real cause), while a real second flow (a different code) still
            // runs normally and a real PKCE-missing case (no flow in progress at
            // all) still reports the same error as before.
            #[cfg(target_os = "android")]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let seen_codes: Arc<Mutex<std::collections::HashSet<String>>> =
                    Arc::new(Mutex::new(std::collections::HashSet::new()));

                let handle = app.handle().clone();
                let seen = seen_codes.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        if let Some((code, state)) = parse_oauth_callback(url.as_str()) {
                            if seen.lock().unwrap().insert(code.clone()) {
                                let h = handle.clone();
                                tauri::async_runtime::spawn(complete_oauth(h, code, state));
                            }
                        }
                    }
                });
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    for url in urls {
                        if let Some((code, state)) = parse_oauth_callback(url.as_str()) {
                            if seen_codes.lock().unwrap().insert(code.clone()) {
                                let h = app.handle().clone();
                                tauri::async_runtime::spawn(complete_oauth(h, code, state));
                            }
                        }
                    }
                }
            }

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                migrate_legacy_keychain(&state).await;
            });
            spawn_sync_worker(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            llm::budget::get_ai_limits,
            llm::budget::set_ai_limits,
            llm::receipts::list_ai_receipts,
            llm::receipts::delete_ai_receipts,
            llm::receipts::get_ai_receipt_retention,
            llm::receipts::set_ai_receipt_retention,
            llm::receipts::export_ai_receipts,

            platform_name,
            get_auth_url,
            open_auth_url,
            is_authenticated,
            list_accounts,
            remove_account,
            get_account_settings,
            backend_capabilities,
            update_account_settings,
            add_local_account,
            icloud_sign_in,
            icloud_reauthenticate,
            icloud_census,
            icloud_write_selftest,
            icloud_relocation_selftest,
            icloud_content_write_selftest,
            icloud_debug_note_history,
            icloud_debug_record_lookup,
            rename_local_account,
            count_pending_pushes,
            retry_blocked_pushes,
            set_account_status,
            list_notes,
            list_notes_in_folder,
            list_cached_notes_in_folder,
            refetch_note,
            get_trashed_note_preview,
            list_cached_notes,
            note_persistence,
            index_account,
            needs_reindex_after_recovery,
            clear_reindex_marker,
            sync_pin_state,
            sync_tag_state,
            save_note,
            delete_note,
            move_notes_batch,
            delete_notes_batch,
            set_pin,
            set_pin_batch,
            get_note_attachments,
            list_trashed_notes,
            restore_note,
            search_notes,
            note_connections,
            note_citations,
            check_duplicate_citations,
            search_note_links,
            add_tag,
            remove_tag,
            list_tags,
            list_note_tags,
            list_cached_notes_with_tags,
            rename_tag,
            delete_tag,
            list_folders,
            list_folder_kinds,
            list_orphaned_notes,
            list_stale_notes,
            list_extract_notes,
            create_folder,
            rename_folder,
            delete_folder,
            move_folder,
            cleanup_orphans,
            get_dup_stats,
            preview_orphans,
            trash_specific_messages,
            extract_note,
            re_extract_note,
            analyze_ingest_sources,
            ingest_sources,
            append_extract_note,
            run_llm_workflow,
            append_llm_workflow_note,
            preview_action_items,
            apply_action_items,
            discard_action_items,
            ask_jodd,
            begin_ask,
            end_ask,
            cancel_ask,
            suggest_wiki_links,
            suggest_note_folder,
            apply_wiki_link_appends,
            cancel_extraction,
            get_llm_settings,
            list_agent_cli_presets,
            test_llm_provider,
            update_llm_settings,
            get_oauth_config,
            save_oauth_config,
            clear_oauth_config,
            get_ms_oauth_config,
            save_ms_oauth_config,
            get_app_llm_config,
            get_app_llm_provider,
            set_app_llm_config,
            get_log_settings,
            set_file_logging_enabled,
            clear_log_file,
            flush_sync,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod note_push_due_tests {
    use super::*;

    const S: i64 = 5_000;   // settle
    const D: i64 = 60_000;  // max defer
    const NOW: i64 = 1_000_000;

    #[test]
    fn quiet_recently_synced_pushes() {
        // edited 6s ago, synced 6s ago → settled
        assert!(note_push_due(NOW, NOW - 6_000, Some(NOW - 6_000), S, D));
    }

    #[test]
    fn actively_editing_recently_synced_skips() {
        // edited 1s ago, synced 2s ago → not settled, not overdue
        assert!(!note_push_due(NOW, NOW - 1_000, Some(NOW - 2_000), S, D));
    }

    #[test]
    fn editing_nonstop_but_overdue_pushes() {
        // edited 1s ago (not settled) but synced 70s ago → overdue cap fires
        assert!(note_push_due(NOW, NOW - 1_000, Some(NOW - 70_000), S, D));
    }

    #[test]
    fn never_synced_actively_editing_skips() {
        // edited 1s ago, never synced → not settled; overdue cannot fire
        assert!(!note_push_due(NOW, NOW - 1_000, None, S, D));
    }

    #[test]
    fn never_synced_quiet_pushes() {
        // edited 6s ago, never synced → settled
        assert!(note_push_due(NOW, NOW - 6_000, None, S, D));
    }

    #[test]
    fn settle_boundary_is_inclusive() {
        // exactly settle_ms elapsed → push (>=)
        assert!(note_push_due(NOW, NOW - 5_000, Some(NOW - 5_000), S, D));
    }

    #[test]
    fn overdue_boundary_is_inclusive() {
        // exactly max_defer_ms since last sync, still actively editing → push (>=)
        assert!(note_push_due(NOW, NOW - 1_000, Some(NOW - 60_000), S, D));
    }

    #[test]
    fn negative_delta_skips() {
        // clock skew: timestamps in the "future" → neither branch fires → skip
        assert!(!note_push_due(NOW, NOW + 100, Some(NOW + 100), S, D));
    }
}

/// Guards the Rust↔Svelte IPC boundary, which nothing else checks.
///
/// A Tauri command is bound to the frontend by a bare string: Svelte writes
/// `invoke('extract_note')` and Rust registers `extract_note` in
/// `generate_handler!`. Nothing connects the two — a mismatch compiles
/// cleanly on both sides and passes every other test, then fails at runtime
/// the moment a user clicks the button. This module reads both sides from
/// source at test time and compares them.
#[cfg(test)]
mod ipc_contract {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has a parent")
            .to_path_buf()
    }

    /// Command names registered in `generate_handler![...]`, parsed from this
    /// very file. Parsing the source (rather than hardcoding a list) is what
    /// makes the test notice when someone adds or renames a command.
    fn registered_commands() -> BTreeSet<String> {
        let src = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("read lib.rs");
        let start = src
            .find("generate_handler![")
            .expect("generate_handler! block present");
        let end = start
            + src[start..]
                .find("])")
                .expect("generate_handler! block is closed");
        src[start..end]
            .lines()
            .skip(1)
            .map(|l| l.trim().trim_end_matches(',').trim())
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            // Tauri exposes the function name, including when registered with
            // a module-qualified Rust path.
            .map(|path| path.rsplit("::").next().unwrap().to_string())
            .collect()
    }

    fn frontend_files() -> Vec<PathBuf> {
        walkdir::WalkDir::new(repo_root().join("src"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .filter(|p| {
                matches!(
                    p.extension().and_then(|x| x.to_str()),
                    Some("svelte") | Some("ts") | Some("js")
                )
            })
            // Frontend tests are not IPC call sites: a vitest `invoke` mock or
            // an assertion naming a command would otherwise register as a
            // dynamic invocation and trip the tripwire below. Only production
            // source binds commands for real, so only production source is
            // policed here.
            .filter(|p| {
                let name = p.file_name().and_then(|x| x.to_str()).unwrap_or_default();
                !name.contains(".test.")
                    && !p.components().any(|c| c.as_os_str() == "__fixtures__")
            })
            .collect()
    }

    /// `(file, line_number, command_name)` for every `invoke('name')` whose
    /// first argument is a string literal. Comment lines are skipped so that
    /// prose mentioning a command name cannot fail the test.
    fn literal_invocations() -> Vec<(PathBuf, usize, String)> {
        let mut out = Vec::new();
        for path in frontend_files() {
            let text = std::fs::read_to_string(&path).expect("read frontend file");
            for (lineno, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                let mut rest = line;
                while let Some(i) = rest.find("invoke") {
                    let after = &rest[i + "invoke".len()..];
                    rest = after;
                    // Skip an optional generic argument, then require '('.
                    let open = match after.find('(') {
                        Some(o) => o,
                        None => break,
                    };
                    let between = &after[..open];
                    if !between
                        .chars()
                        .all(|c| c.is_whitespace() || "<>[],_".contains(c) || c.is_alphanumeric())
                    {
                        continue;
                    }
                    let args = after[open + 1..].trim_start();
                    let quote = match args.chars().next() {
                        Some(q @ ('\'' | '"')) => q,
                        _ => continue, // dynamic first argument — counted separately
                    };
                    if let Some(close) = args[1..].find(quote) {
                        out.push((path.clone(), lineno + 1, args[1..1 + close].to_string()));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn every_literal_invoke_name_is_a_registered_command() {
        let registered = registered_commands();
        assert!(
            registered.len() > 20,
            "parsed only {} commands from generate_handler! — the parser is broken, \
             not the code under test",
            registered.len()
        );

        let calls = literal_invocations();
        assert!(
            calls.len() > 20,
            "found only {} invoke() call sites — the scanner is broken",
            calls.len()
        );

        let missing: Vec<String> = calls
            .iter()
            .filter(|(_, _, name)| {
                // Plugin commands (`plugin:dialog|open`) are not ours to register.
                !name.contains('|') && !name.contains(':') && !registered.contains(name)
            })
            .map(|(p, line, name)| {
                format!(
                    "{}:{} invokes '{}' which is not in generate_handler!",
                    p.strip_prefix(repo_root()).unwrap_or(p).display(),
                    line,
                    name
                )
            })
            .collect();

        assert!(
            missing.is_empty(),
            "frontend invokes commands the backend does not register:\n  {}",
            missing.join("\n  ")
        );
    }

    /// `invoke(someVariable)` cannot be checked statically, so each one is
    /// pinned here deliberately: adding another forces a human to decide how
    /// it will be covered instead of letting it slip past this test silently.
    #[test]
    fn dynamic_invocations_are_the_known_ones_only() {
        let known_dynamic_commands = ["list_orphaned_notes", "list_stale_notes", "list_extract_notes"];
        let registered = registered_commands();
        for name in known_dynamic_commands {
            assert!(
                registered.contains(name),
                "'{name}' is reached through a dynamic invoke() and is not registered"
            );
        }

        let mut dynamic_sites = Vec::new();
        for path in frontend_files() {
            let text = std::fs::read_to_string(&path).expect("read frontend file");
            for (lineno, line) in text.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }
                if let Some(i) = line.find("invoke") {
                    let after = &line[i + "invoke".len()..];
                    if let Some(open) = after.find('(') {
                        let args = after[open + 1..].trim_start();
                        let first = args.chars().next();
                        if first.is_some()
                            && first != Some('\'')
                            && first != Some('"')
                            && first != Some(')')
                        {
                            dynamic_sites.push(format!(
                                "{}:{}",
                                path.strip_prefix(repo_root()).unwrap_or(&path).display(),
                                lineno + 1
                            ));
                        }
                    }
                }
            }
        }

        assert_eq!(
            dynamic_sites.len(),
            1,
            "expected exactly one dynamic invoke() (App.svelte's smart-folder loader); \
             found {}: {:?}. Add coverage for the new one, then update this test.",
            dynamic_sites.len(),
            dynamic_sites
        );
    }
}

#[cfg(test)]
mod remote_changed_tests {
    use super::*;

    #[test]
    fn a_remote_edit_that_keeps_its_id_is_still_a_remote_change() {
        // Exchange PATCHes in place, so an Apple-side edit leaves the id alone
        // and only lastModifiedDateTime moves. Keying on the id makes the
        // detector blind and the worker overwrites Apple's edit silently.
        let cached_version = "2026-08-14T10:00:00Z";
        let fetched_version = "2026-08-14T11:00:00Z";
        assert!(
            remote_changed(Some(cached_version), fetched_version),
            "same id, newer lastModifiedDateTime must read as a remote change"
        );
        assert!(
            !remote_changed(Some(fetched_version), fetched_version),
            "an unchanged version must not manufacture a conflict"
        );
        assert!(
            remote_changed(None, fetched_version),
            "a row that has never been synced must count as changed"
        );
    }

    #[test]
    fn an_empty_fetched_version_always_reads_as_changed() {
        // Microsoft's `lastModifiedDateTime` is `#[serde(default)]`, so a
        // message missing that field produces `Note::version == ""`. A cache
        // that has never disagreed with an empty fetched version (including a
        // cache that is ALSO "", e.g. this note's very first sync) must still
        // report changed — the alternative is a version token that reports
        // "unchanged" forever and an Apple-side edit disappears with no
        // conflict copy, silently. See `remote_changed`'s doc comment.
        assert!(
            remote_changed(Some("2026-08-14T10:00:00Z"), ""),
            "empty fetched_version against a real cached version must read as changed"
        );
        assert!(
            remote_changed(Some(""), ""),
            "empty fetched_version against an equally-empty cached version must STILL read as changed"
        );
        assert!(
            remote_changed(None, ""),
            "empty fetched_version on a never-synced row must read as changed"
        );
    }
}

#[cfg(test)]
mod localfs_push_reconcile_tests {
    use super::*;
    use crate::backend::localfs::LocalFsVertical;
    use crate::backend::{NoteStore, SaveOp};

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, body_html: &str) -> db::CachedNote {
        db::CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: "T".to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        }
    }

    /// Regression test for the `mark_pushed`/`SavedNote` version-mismatch bug
    /// (fix round 1 on Task 1's coordinator review).
    ///
    /// `CachedNote::from_remote` stamps `remote_version` from the vertical's
    /// `Note::version`, but `mark_pushed` used to stamp it from the post-push
    /// `id` instead. On Gmail those are the same value, so nothing broke
    /// there. On LocalFs they are NOT — `id` is the file path, `version` is
    /// the Date header — so after every LocalFs push `remote_version` held a
    /// file path while the very next disk read reports a Date header:
    /// permanently mismatched, not transiently. A user who edits again
    /// before the next poll lands (the ordinary "save, then keep typing"
    /// flow) has a Dirty row when that mismatch gets evaluated, and
    /// Dirty + remote_changed is exactly the arm that fabricates a conflict
    /// copy — on totally normal editing, with nothing external having
    /// changed at all.
    #[tokio::test]
    async fn a_localfs_push_does_not_manufacture_a_conflict_on_the_next_reconcile() {
        let db = temp_db();
        let acct = "a@example.com";
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        let vault = tempfile::tempdir().unwrap();
        let v = LocalFsVertical::new(vault.path().to_path_buf(), acct.to_string());

        db.insert_local_new(&mk_note(acct, uuid, "<p>hello</p>")).unwrap();

        // Worker pushes it — the same calls push_one_dirty makes.
        let op = SaveOp {
            title: "T",
            body_html: "<p>hello</p>",
            existing_remote_id: None,
            existing_uuid: Some(uuid),
            existing_created_date: None,
            label: "Notes",
        };
        let saved = v.save_note_full(&op, &[]).await.unwrap();
        db.mark_pushed(uuid, acct, &saved.id, &saved.version, &saved.date, &saved.body_html, 1)
            .unwrap();

        // "Keep typing": a second local edit lands before the next poll — the
        // ordinary flow, and the one that puts the row back in Dirty while
        // the stale-vs-correct remote_version question is what matters.
        db.apply_local_edit(uuid, acct, "T", "<p>hello more</p>", "Notes").unwrap();

        // The next poll fetches exactly what's on disk right now — i.e.
        // exactly what was just pushed. Nothing external has changed.
        let fetched = v.fetch_note(&saved.id).await.unwrap();
        reconcile_one_db(&db, acct, &fetched, db::RemotePin::LocalWins, "CONFLICT-COPY-UUID");

        // The bug's user-visible symptom: a spurious "(conflict from ...)" row.
        let rows = db.list_notes(acct).unwrap();
        assert_eq!(
            rows.len(),
            1,
            "a no-op push must not manufacture a conflict copy: {:?}",
            rows.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        let row = db.get(uuid, acct).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty, "the second local edit is still unpushed");
        assert!(row.body_html.contains("hello more"), "the second edit must survive: {}", row.body_html);
    }
}

#[cfg(test)]
mod vertical_gate_tests {
    use super::*;
    use crate::accounts::AccountStatus;

    /// Draining MUST pass. It is the state in which the worker is still
    /// pushing, so refusing it here would mean nothing ever drains and every
    /// deactivated account stayed Draining forever.
    #[test]
    fn draining_is_admitted_and_inactive_is_refused() {
        assert!(refuse_if_inactive("a@x", AccountStatus::Active).is_ok());
        assert!(refuse_if_inactive("a@x", AccountStatus::Draining).is_ok());
        let err = refuse_if_inactive("a@x", AccountStatus::Inactive)
            .expect_err("inactive must be refused");
        assert!(err.contains("a@x"), "message should name the account: {err}");
        assert!(err.contains("inactive"), "message should say why: {err}");
    }
}

#[cfg(test)]
mod capabilities_tests {
    use crate::accounts;
    use crate::accounts::BackendKind;
    use crate::backend;
    use crate::backend::{
        gmail::GmailVertical, localfs::LocalFsVertical, microsoft::MicrosoftVertical, Capabilities,
        Vertical,
    };
    use crate::write_refusal_for;
    use std::collections::HashMap;

    /// The one place all three verticals are asserted side by side: whether
    /// deleted notes land somewhere the user can restore from.
    #[test]
    fn every_vertical_reports_whether_it_has_a_trash() {
        let g = GmailVertical::new("t".into(), HashMap::new(), "a@b.com".into(), "meta".into());
        assert!(g.capabilities().has_trash, "Gmail Trash is Apple's Recently Deleted here");

        let l = LocalFsVertical::new(std::path::PathBuf::from("/tmp/x"), "acct".into());
        assert!(l.capabilities().has_trash, "LocalFs keeps a real trash_dir()");

        let m = MicrosoftVertical::new("t".into(), "acct".into(), Default::default());
        assert!(!m.capabilities().has_trash);
    }

    /// Same shape for write permissions: the three verticals side by side.
    /// Gmail and LocalFs get every area; Microsoft has notes on (measured
    /// live 2026-08-15, `ms_write_probe`), folders permanently refused —
    /// live testing the same day found Graph-created folders never reach
    /// Apple Notes (`ErrorObjectTypeChanged` on the one avenue that could
    /// have fixed it after creation) — and sidecars on as of M4 (2026-08-16):
    /// pin state lives in a named MAPI property on the note itself, measured
    /// live to round-trip, survive a content PATCH, and render normally in
    /// Notes.app — see the mapping test below.
    #[test]
    fn every_vertical_reports_what_it_can_be_written_to() {
        let g = GmailVertical::new("t".into(), HashMap::new(), "a@b.com".into(), "meta".into());
        assert_eq!(g.capabilities().writes, backend::Writes::ALL);

        let l = LocalFsVertical::new(std::path::PathBuf::from("/tmp/x"), "acct".into());
        assert_eq!(l.capabilities().writes, backend::Writes::ALL);

        let m = MicrosoftVertical::new("t".into(), "acct".into(), Default::default());
        assert_eq!(
            m.capabilities().writes,
            backend::Writes { notes: true, relocate: true, folders: false, sidecars: true },
            "notes writable (M2), folders permanently refused — never reach Apple (M3), \
             sidecars writable via a named property on the note itself (M4)"
        );
    }

    /// Content editing came on, went off, and came back on across
    /// 2026-08-26 — the final state is ON, with the duplicated-note merge
    /// root-caused to the replica table's serialization order and fixed in
    /// `crdt::ensure_replica` (editor first, everything renumbered — the
    /// shape a live icloud.com capture pinned). Sidecars stay refused for
    /// their own reason: the pin is Apple's own on a per-user record.
    #[test]
    fn icloud_writes_everything_but_sidecars() {
        let caps = backend::Capabilities::for_backend(accounts::BackendKind::ICloud);
        assert_eq!(
            caps.writes,
            backend::Writes { notes: true, relocate: true, folders: true, sidecars: false },
            "content editing, relocation, and folder create/rename/delete are all measured \
             safe live; sidecars stay refused independently of any of them"
        );
    }

    /// Guards against the one real hazard in having two readers of the same
    /// table: `Capabilities::for_backend` (what `backend_capabilities` reads,
    /// with no vertical and no token) must never drift from what each
    /// vertical's own `capabilities()` reports (what the sync worker / save
    /// path reads).
    #[test]
    fn for_backend_agrees_with_every_vertical() {
        let g = GmailVertical::new("t".into(), HashMap::new(), "a@b.com".into(), "meta".into());
        assert_eq!(
            Capabilities::for_backend(BackendKind::Gmail).has_trash,
            g.capabilities().has_trash
        );

        let l = LocalFsVertical::new(std::path::PathBuf::from("/tmp/x"), "acct".into());
        assert_eq!(
            Capabilities::for_backend(BackendKind::LocalFs).has_trash,
            l.capabilities().has_trash
        );

        let m = MicrosoftVertical::new("t".into(), "acct".into(), Default::default());
        assert_eq!(
            Capabilities::for_backend(BackendKind::Microsoft).has_trash,
            m.capabilities().has_trash
        );
        assert_eq!(
            Capabilities::for_backend(BackendKind::Microsoft).writes,
            m.capabilities().writes
        );
    }

    /// An unknown account defers to its own "account not found" path rather
    /// than being refused here, for every write kind.
    #[test]
    fn unknown_account_write_refusal_defers_to_account_not_found() {
        use crate::backend::Write::*;

        assert_eq!(write_refusal_for(None, Notes), None);
        assert_eq!(write_refusal_for(None, Folders), None);
        assert_eq!(write_refusal_for(None, Sidecars), None);
    }

    /// Scans this file's OWN source for `refuse_write(&state, &account_id,
    /// backend::Write::X)` calls and walks each one back to its nearest
    /// enclosing `fn`, returning `(command, kind)` pairs sorted so that
    /// reordering or reformatting the source cannot change the result.
    ///
    /// This exists because a hardcoded `mapping` literal in a test body only
    /// proves the table agrees with itself: wire `set_pin` to the wrong
    /// `Write` variant and a literal-only test still passes, because the
    /// literal still says what it always said. Reading the actual call
    /// sites is the only way this test can fail when a call site is wrong.
    fn write_kinds_declared_in_source() -> Vec<(String, String)> {
        let src = concat!(include_str!("lib.rs"), "\n", include_str!("note_commands.rs"));
        let lines: Vec<&str> = src.lines().collect();
        let marker = "refuse_write(&state, &account_id, backend::Write::";
        let mut found = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let Some(after_marker) = line.find(marker).map(|idx| &line[idx + marker.len()..])
            else {
                continue;
            };
            let Some(end) = after_marker.find(')') else { continue };
            let kind = after_marker[..end].to_string();

            // Walk back to the nearest `fn` declaration above this call —
            // the same thing a human reviewer does by eye to find which
            // command a guard belongs to.
            let fn_name = lines[..i].iter().rev().find_map(|prior| {
                let trimmed = prior.trim_start();
                ["pub(super) async fn ", "pub(super) fn ", "pub async fn ", "async fn ", "pub fn ", "fn "]
                    .iter()
                    .find_map(|prefix| trimmed.strip_prefix(prefix))
                    .and_then(|rest| rest.split(['(', ' ']).next())
                    .map(|name| name.to_string())
            });
            if let Some(name) = fn_name {
                found.push((name, kind));
            }
        }
        found.sort();
        found
    }

    /// The call-site mapping table: every command that guards a write, with
    /// the kind it must pass. Nothing in the type system catches a
    /// miscategorised call site, and a miscategorised one reopens the wedge
    /// (see `backend::Writes`'s doc comment) — so this table IS the spec,
    /// executable, and checked against the actual `refuse_write` call sites
    /// in this file rather than against itself.
    #[test]
    fn every_guarded_command_declares_the_right_write_kind() {
        use backend::Write::*;
        // Every command that calls refuse_write, with the kind it must pass.
        // If you add a guarded command, add it here — this list is the contract.
        let mapping: &[(&str, backend::Write)] = &[
            ("save_note", Notes),
            // `delete_note`/`move_notes_batch`/`delete_notes_batch`/
            // `restore_note` move or trash/restore a note without touching
            // its content — `Write::Relocate`, not `Write::Notes`, since
            // iCloud can do these and cannot do content edits (M2 live pass,
            // 2026-08-24).
            ("delete_note", Relocate),
            ("move_notes_batch", Relocate), ("delete_notes_batch", Relocate),
            ("restore_note", Relocate), ("apply_wiki_link_appends", Notes),
            ("create_folder", Folders), ("rename_folder", Folders),
            ("delete_folder", Folders), ("move_folder", Folders),
            ("set_pin", Sidecars), ("set_pin_batch", Sidecars),
            ("add_tag", Sidecars), ("remove_tag", Sidecars),
            ("rename_tag", Sidecars), ("delete_tag", Sidecars),
            // Extract/append/re-extract file through `filing::resolve_destination` /
            // `filing::destination_beside`, which fall back to the root instead of
            // creating a folder when the backend can't — so these refuse
            // `Write::Notes` only, not `Write::Folders` (that refusal used to make
            // Extract unusable on Outlook).
            ("extract_note", Notes),
            ("append_extract_note", Notes),
            ("re_extract_note", Notes),
            // URL ingest writes exactly one note, same as Extract — see
            // docs/superpowers/specs/2026-09-15-url-ingest-design.md.
            ("ingest_sources", Notes),
            // Roadmap #2's three new workflows (Summarize/Action Items/Expand
            // Bullets) share Extract's write shape exactly — same refusal,
            // same filing fallback to the root — so they refuse `Write::Notes`
            // only, for the same reason as the Extract trio above.
            ("run_llm_workflow", Notes),
            ("append_llm_workflow_note", Notes),
            ("preview_action_items", Notes), ("apply_action_items", Notes),
        ];
        assert_eq!(mapping.len(), 24, "24 commands, each single-kind");

        // The load-bearing check: the table must equal what the source
        // actually does, not what it says it does. A command miswired to the
        // wrong `Write` variant, or one that stopped calling `refuse_write`
        // entirely, changes this multiset and fails here — the thing the
        // literal-only version of this test could never catch.
        let mut expected: Vec<(String, String)> =
            mapping.iter().map(|(cmd, kind)| (cmd.to_string(), format!("{kind:?}"))).collect();
        expected.sort();
        let actual = write_kinds_declared_in_source();
        assert_eq!(
            actual, expected,
            "the (command, kind) pairs `refuse_write` is actually called with, scanned from \
             source, must equal the mapping table above"
        );

        // Live-verified 2026-08-15: notes are real code, confirmed against
        // kaiwan.h@live.com, and must NOT be refused. Folders are refused —
        // not caution, a measured negative: Graph-created folders never
        // reached Apple Notes (see `Capabilities::for_backend`'s Microsoft
        // arm for the full mechanism, `ErrorObjectTypeChanged` included).
        // Sidecars (pin) are live as of M4 (2026-08-16) — a named MAPI
        // property on the note itself, measured to round-trip and render
        // normally in Notes.app — and must NOT be refused either.
        let ms = Some(accounts::BackendKind::Microsoft);
        for (cmd, kind) in mapping {
            let refused = write_refusal_for(ms, *kind).is_some();
            let expect_refused = matches!(kind, Folders);
            assert_eq!(
                refused, expect_refused,
                "{cmd} with {kind:?}: refused={refused}, expected {expect_refused}"
            );
        }
    }

    /// M4 (2026-08-16) turned sidecars on for Microsoft — the refusal that
    /// used to name "Milestone 4" no longer fires at all. Folders keep their
    /// own, unrelated refusal.
    #[test]
    fn sidecars_are_no_longer_refused_on_microsoft_but_folders_still_are() {
        use backend::Write::*;
        let ms = Some(accounts::BackendKind::Microsoft);
        assert!(
            write_refusal_for(ms, Sidecars).is_none(),
            "M4 shipped — pin sidecars must not be refused on Microsoft"
        );
        assert!(
            write_refusal_for(ms, Notes).is_none(),
            "notes have been writable since M2"
        );
        // Folders are refused (measured 2026-08-15: Graph-created folders
        // never reach Apple Notes), for a reason unrelated to sidecars — it
        // isn't gated behind any milestone, so it must not claim to be.
        let folder_msg = write_refusal_for(ms, Folders).expect("folders are refused");
        assert!(
            !folder_msg.contains("Milestone 4"),
            "folder refusal is a measured Graph limitation, not an M4 gate: {folder_msg}"
        );
        // Gmail and LocalFs refuse nothing.
        for k in [accounts::BackendKind::Gmail, accounts::BackendKind::LocalFs] {
            for w in [Notes, Folders, Sidecars] {
                assert!(write_refusal_for(Some(k), w).is_none(), "{k:?}/{w:?}");
            }
        }
    }

    /// iCloud's write surface is split, and it is `sidecars` alone now —
    /// content editing turned on 2026-08-26 (M2.5) joined `relocate`/
    /// `folders` in the not-refused group; `sidecars` is refused for an
    /// independent reason that content editing turning on does not touch.
    #[test]
    fn icloud_refuses_sidecars_but_not_content_relocation_or_folders() {
        use backend::Write::*;
        let ic = Some(accounts::BackendKind::ICloud);
        // Sidecars: a design answer, not a gap — the pin is Apple's own on a
        // different record type, and RemoteWins would overwrite anything a
        // Jodd sidecar wrote.
        let msg = write_refusal_for(ic, Sidecars)
            .unwrap_or_else(|| panic!("iCloud cannot write sidecars — it must be refused"));
        // The sidecar message named Microsoft and Milestone 4 while it was
        // unreachable. iCloud made it reachable again; a refusal that
        // names the wrong backend is worse than a generic one.
        assert!(
            !msg.contains("Microsoft") && !msg.contains("Milestone"),
            "Sidecars refusal must not name another backend or a milestone: {msg}"
        );
        // Content editing (root-caused and fixed 2026-08-26 — the replica
        // table's serialization order, `crdt::ensure_replica`; two
        // fresh-note live passes survived the delayed-merge window),
        // relocation, and folder create/rename/delete: none refused.
        for w in [Notes, Relocate, Folders] {
            assert!(
                write_refusal_for(ic, w).is_none(),
                "{w:?} must not be refused — measured safe on a live account"
            );
        }
    }
}

#[cfg(test)]
mod drain_flip_tests {
    use super::*;
    use crate::accounts::AccountStatus;

    /// Pins the decision, not the plumbing: only a Draining account with an
    /// empty queue may flip to Inactive. Everything else must refuse,
    /// regardless of queue state — in particular `Active` must refuse even
    /// when `queue_empty` is true, because that is exactly the shape of the
    /// race the end-of-tick flip re-checks against (a reactivate landing
    /// between the tick's snapshot of Draining ids and the write-back lock).
    #[test]
    fn should_flip_to_inactive_only_admits_draining_with_empty_queue() {
        assert!(should_flip_to_inactive(AccountStatus::Draining, true));

        assert!(!should_flip_to_inactive(AccountStatus::Draining, false));
        assert!(!should_flip_to_inactive(AccountStatus::Active, true));
        assert!(!should_flip_to_inactive(AccountStatus::Active, false));
        assert!(!should_flip_to_inactive(AccountStatus::Inactive, true));
        assert!(!should_flip_to_inactive(AccountStatus::Inactive, false));
    }
}

/// Roadmap #0d: the gentler removal path for a `Draining` account.
///
/// `remove_account` used to refuse outright — `removal_allowed(Draining) ==
/// false`, unchanged by this feature — leaving "wait" or the "Stop waiting"
/// force-strand as the only ways forward. `pending_removal` is the third
/// option: queue the request on the row instead of refusing it, and let
/// `sync_worker_tick` finish it once `has_pending_pushes` genuinely clears.
#[cfg(test)]
mod pending_removal_tests {
    use super::*;
    use crate::accounts::AccountStatus::{Active, Draining, Inactive};
    use crate::accounts::{Account, AccountStatus};

    fn draining_account(id: &str) -> Account {
        Account {
            id: id.to_string(),
            email: id.to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: accounts::BackendKind::Gmail,
            root_dir: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: Draining,
            pending_removal: false,
        }
    }

    /// The exact shape `remove_account`'s Draining branch now performs: no
    /// refusal, no credential touched (nothing here can reach
    /// `perform_account_removal` — the branch returns before ever calling
    /// it), just the flag recorded on the row so the worker can finish the
    /// job later.
    #[test]
    fn requesting_removal_while_draining_queues_it_without_deleting_anything() {
        let mut accounts = vec![draining_account("a@x.com")];
        assert!(!accounts[0].pending_removal);

        let found = queue_removal(&mut accounts, "a@x.com");

        assert!(found, "the account must be found");
        assert!(accounts[0].pending_removal, "the request must be recorded");
        // Still here, still Draining — queueing changes nothing else about
        // the account's identity or lifecycle state.
        assert_eq!(accounts.len(), 1, "queueing must not remove the row");
        assert_eq!(accounts[0].status, Draining);
    }

    #[test]
    fn queueing_an_unknown_account_id_is_a_no_op() {
        let mut accounts = vec![draining_account("a@x.com")];
        assert!(!queue_removal(&mut accounts, "ghost@x.com"));
        assert!(!accounts[0].pending_removal, "the real account must be untouched");
    }

    /// Mirrors `should_flip_to_inactive_only_admits_draining_with_empty_
    /// queue` for the next stage. In particular `Draining` must refuse even
    /// with the flag set: a Draining account's queue is not yet confirmed
    /// empty, and deleting its credential out from under a live drain is
    /// exactly the bug this whole feature exists to avoid — `pending_removal`
    /// must never become a second way to skip that guarantee.
    #[test]
    fn should_complete_pending_removal_only_admits_inactive_with_the_flag_set() {
        assert!(should_complete_pending_removal(Inactive, true));

        assert!(!should_complete_pending_removal(Inactive, false), "no request was ever made");
        assert!(!should_complete_pending_removal(Draining, true), "queue not yet confirmed empty");
        assert!(!should_complete_pending_removal(Draining, false));
        assert!(!should_complete_pending_removal(Active, true));
        assert!(!should_complete_pending_removal(Active, false));
    }

    /// "Stop waiting" is `transition_allowed(Draining, Inactive)` — the exact
    /// same edge as before this feature, untouched. This pins that it stays
    /// reachable, and separately that a queued removal composes with it
    /// rather than needing a special case: once Stop waiting's forced flip
    /// lands the account on `Inactive`, `removal_allowed` already treats
    /// `Inactive` as unconditionally safe to remove, so
    /// `should_complete_pending_removal` agreeing is the SAME gate, not a
    /// looser one invented for this path.
    #[test]
    fn stop_waiting_is_unchanged_and_composes_with_a_queued_removal() {
        assert!(transition_allowed(Draining, Inactive), "stop waiting must still work");

        // No removal was ever queued: Stop waiting alone must not trigger one.
        assert!(!should_complete_pending_removal(Inactive, false));

        // A removal WAS queued before Stop waiting was pressed: once the
        // forced flip lands on Inactive, the queued request may complete —
        // consistent with removal_allowed(Inactive) already being true.
        assert!(removal_allowed(Inactive));
        assert!(should_complete_pending_removal(Inactive, true));
    }

    /// End-to-end of the decision logic (minus the network): a Draining
    /// account queued for removal, then a real Db proving the queue
    /// genuinely empties, must not jump straight to "remove it" — it goes
    /// through the SAME Inactive flip `sync_worker_tick` already performs for
    /// every Draining account, and only a tick where the account was already
    /// found Inactive may complete the removal. Pins that ordering with a
    /// real `has_pending_pushes`/`mark_pushed` round trip rather than an
    /// assumption about what "empties" means.
    #[test]
    fn a_queued_removal_completes_only_after_the_queue_genuinely_empties() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let acct_id = "a@x.com";

        let note = db::CachedNote {
            uuid: "u1".to_string(),
            account_id: acct_id.to_string(),
            id: "remote-1".to_string(),
            title: "T".to_string(),
            body_html: "<p>x</p>".to_string(),
            date: "Mon, 14 Aug 2026 09:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: db::now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
        };
        db.insert_local_new(&note).unwrap();

        let mut accounts = vec![draining_account(acct_id)];
        assert!(queue_removal(&mut accounts, acct_id));

        // Tick 1: an unsent edit is still queued. Neither the Inactive flip
        // nor the removal completion may fire yet.
        assert!(db.has_pending_pushes(acct_id).unwrap());
        assert!(!should_flip_to_inactive(accounts[0].status, false));
        assert!(!should_complete_pending_removal(accounts[0].status, accounts[0].pending_removal));

        // The push actually lands — what push_one_dirty + mark_pushed do on
        // a real tick.
        db.mark_pushed(
            "u1", acct_id, "remote-1", "v1",
            "Mon, 14 Aug 2026 09:00:00 +0700", "<p>x</p>", 1,
        ).unwrap();
        assert!(!db.has_pending_pushes(acct_id).unwrap(), "the queue must be genuinely empty now");

        // Tick 2: the flip loop runs first and is the one and only proof the
        // queue is empty — this is should_flip_to_inactive admitting it.
        assert!(should_flip_to_inactive(accounts[0].status, true));
        accounts[0].status = AccountStatus::Inactive;
        // The SAME tick's removal-completion snapshot was taken before this
        // write (see sync_worker_tick), so this account is NOT yet a
        // candidate on tick 2 — only from tick 3 onward. Assert that
        // directly rather than assuming it from the production code's
        // ordering.
        let snapshot_before_the_flip_this_tick = Draining;
        assert!(!should_complete_pending_removal(
            snapshot_before_the_flip_this_tick,
            accounts[0].pending_removal
        ));

        // Tick 3: the account was already Inactive at the top of this tick —
        // now the removal actually completes.
        assert!(should_complete_pending_removal(accounts[0].status, accounts[0].pending_removal));
    }
}

#[cfg(test)]
mod status_transition_tests {
    use super::*;
    use crate::accounts::AccountStatus::{Active, Draining, Inactive};

    /// Only three transitions are reachable from the UI. Anything else would
    /// put an account somewhere the worker will not move it out of — an
    /// Active -> Inactive jump, for instance, skips the drain and strands
    /// every queued edit silently, which is the failure this design exists to
    /// prevent.
    #[test]
    fn only_the_offered_transitions_are_allowed() {
        assert!(transition_allowed(Active, Draining), "deactivate");
        assert!(transition_allowed(Draining, Inactive), "give up waiting");
        assert!(transition_allowed(Inactive, Active), "reactivate");

        assert!(!transition_allowed(Active, Inactive), "must drain first");
        assert!(!transition_allowed(Draining, Active), "reactivate from Inactive only");
        assert!(!transition_allowed(Inactive, Draining));
    }

    #[test]
    fn a_no_op_transition_is_allowed() {
        for s in [Active, Draining, Inactive] {
            assert!(transition_allowed(s, s), "setting {s:?} to itself must not error");
        }
    }

    /// remove_account deletes the refresh token FIRST (lib.rs:507), before the
    /// account leaves state.accounts and before the cache is wiped. Removing a
    /// draining account would therefore pull the credential out from under a
    /// drain that is still pushing: the in-flight request fails on auth and
    /// every queued push after it has no token. Removal is restricted to a
    /// state whose queue is empty by construction.
    #[test]
    fn removal_is_refused_while_draining() {
        assert!(removal_allowed(Active), "removing a live account is unchanged");
        assert!(removal_allowed(Inactive), "queue is empty by construction");
        assert!(!removal_allowed(Draining), "would delete the key mid-drain");
    }
}

#[cfg(test)]
mod cred_write_tests {
    use super::*;

    const OLD: &str = "old-123.apps.googleusercontent.com";
    const NEW: &str = "new-456.apps.googleusercontent.com";

    /// The bug this guards: the UI clears the secret field after every save, so
    /// reopening Settings to change ONLY the Client ID submits a blank secret.
    /// The old code took that as "keep the stored secret" and wrote the new id
    /// beside a secret issued for the old one — a pair that cannot authenticate.
    #[test]
    fn a_changed_client_id_may_not_inherit_the_old_secret() {
        assert_eq!(
            plan_cred_write(NEW, "", Some(OLD), true),
            CredWrite::RejectMissingSecret
        );
    }

    /// Same trap on a fresh install: nothing stored, blank secret, non-empty id.
    #[test]
    fn a_first_client_id_may_not_be_saved_without_a_secret() {
        assert_eq!(plan_cred_write(NEW, "", None, false), CredWrite::RejectMissingSecret);
        assert_eq!(
            plan_cred_write(NEW, "", Some(NEW), false),
            CredWrite::RejectMissingSecret,
            "id matches but no secret was ever stored"
        );
    }

    /// "(already saved — leave blank to keep)" still has to work — that is the
    /// whole point of not simply making the field mandatory.
    #[test]
    fn an_unchanged_client_id_keeps_its_stored_secret() {
        assert_eq!(plan_cred_write(OLD, "", Some(OLD), true), CredWrite::KeepSecret);
    }

    #[test]
    fn supplying_both_always_writes_both() {
        assert_eq!(plan_cred_write(NEW, "sec", Some(OLD), true), CredWrite::Both);
        assert_eq!(plan_cred_write(NEW, "sec", None, false), CredWrite::Both);
        assert_eq!(plan_cred_write(OLD, "sec", Some(OLD), true), CredWrite::Both);
    }

    /// An empty id is the "remove my credentials" gesture, and it must take the
    /// secret with it — a stranded keychain entry would silently re-pair with
    /// whatever id is entered next.
    #[test]
    fn an_empty_client_id_clears_both_halves() {
        assert_eq!(plan_cred_write("", "", Some(OLD), true), CredWrite::ClearBoth);
        assert_eq!(plan_cred_write("", "sec", Some(OLD), true), CredWrite::ClearBoth);
    }
}

#[cfg(test)]
mod oauth_callback_parse_tests {
    use super::parse_oauth_callback;

    /// What Android actually receives now: the App Links https redirect.
    const APP_LINK: &str = "https://jodd.bbmedia.co.th/oauth2redirect";

    #[test]
    fn parses_code_and_state_from_the_app_links_redirect() {
        let url = format!("{APP_LINK}?code=ABC123&state=XYZ789");
        assert_eq!(
            parse_oauth_callback(&url),
            Some(("ABC123".to_string(), "XYZ789".to_string()))
        );
    }

    #[test]
    fn parses_code_and_state_from_a_loopback_redirect() {
        let url = "http://localhost:8080/callback?state=XYZ789&code=ABC123";
        assert_eq!(
            parse_oauth_callback(url),
            Some(("ABC123".to_string(), "XYZ789".to_string()))
        );
    }

    // The redirect shape has changed twice already (custom scheme → loopback →
    // App Links). This parser survived both because it reads only the query
    // string; the retired custom scheme is kept here as the cheapest possible
    // proof that the next change will not need to touch it either.
    #[test]
    fn ignores_the_scheme_entirely() {
        let url = "co.bbmedia.jodd:/oauth2redirect?code=ABC123&state=XYZ789";
        assert_eq!(
            parse_oauth_callback(url),
            Some(("ABC123".to_string(), "XYZ789".to_string()))
        );
    }

    #[test]
    fn percent_decodes_the_values() {
        let url = format!("{APP_LINK}?code=A%2FB%2BC&state=S%3DT");
        assert_eq!(
            parse_oauth_callback(&url),
            Some(("A/B+C".to_string(), "S=T".to_string()))
        );
    }

    #[test]
    fn returns_none_when_the_user_denied_consent() {
        // Google sends ?error=access_denied with no code.
        let url = format!("{APP_LINK}?error=access_denied&state=XYZ");
        assert_eq!(parse_oauth_callback(&url), None);
    }

    #[test]
    fn returns_none_when_state_is_missing() {
        // Without state we cannot do the CSRF check, so this must not proceed.
        let url = format!("{APP_LINK}?code=ABC123");
        assert_eq!(parse_oauth_callback(&url), None);
    }

    // The plain landing page, which a user reaches whenever verification is not
    // working and the browser renders the redirect instead of Android
    // intercepting it. Must not be mistaken for a callback.
    #[test]
    fn returns_none_for_a_url_with_no_query_string() {
        assert_eq!(parse_oauth_callback(APP_LINK), None);
    }
}

/// What the user is told when the authorization server refuses.
#[cfg(test)]
#[cfg(not(target_os = "android"))]
mod signin_denial_tests {
    use super::*;

    /// The refusal exactly as measured on 2026-08-17 from a non-admin user in
    /// an outside Microsoft 365 tenant, after Microsoft showed "Need admin
    /// approval".
    fn measured_microsoft_refusal() -> auth::CallbackDenial {
        auth::CallbackDenial {
            error: "access_denied".to_string(),
            subcode: Some("cancel".to_string()),
            description: None,
            state: Some("QljKk8On4aTpcb0O".to_string()),
        }
    }

    #[test]
    fn a_microsoft_refusal_always_carries_the_admin_consent_link() {
        let out = signin_denial(accounts::BackendKind::Microsoft, &measured_microsoft_refusal());
        let url = out
            .admin_consent_url
            .expect("a Microsoft refusal must offer the link — it is the whole point");
        assert!(url.contains("/organizations/adminconsent"), "wrong endpoint: {url}");
    }

    /// The link is offered on EVERY Microsoft refusal, including one that is
    /// really just a cancel, because the two are byte-identical (see
    /// `signin_denial`'s doc comment). This test exists to stop a future
    /// change from "optimising" the cancel case away — doing so would silently
    /// drop the link in the exact case it was built for.
    #[test]
    fn the_link_is_offered_even_though_this_may_only_be_a_cancel() {
        let plain_cancel = auth::CallbackDenial {
            error: "access_denied".to_string(),
            subcode: Some("cancel".to_string()),
            description: None,
            state: Some("S1".to_string()),
        };
        assert_eq!(
            signin_denial(accounts::BackendKind::Microsoft, &plain_cancel),
            signin_denial(accounts::BackendKind::Microsoft, &measured_microsoft_refusal()),
        );
    }

    #[test]
    fn a_gmail_refusal_has_no_link_to_offer() {
        // Google's equivalent is Workspace app allowlisting in the Admin
        // console — no per-app URL exists to hand to an administrator, so
        // inventing one would be worse than saying nothing.
        let out = signin_denial(
            accounts::BackendKind::Gmail,
            &auth::CallbackDenial {
                error: "access_denied".to_string(),
                subcode: None,
                description: None,
                state: Some("S1".to_string()),
            },
        );
        assert_eq!(out.admin_consent_url, None);
        assert!(!out.message.is_empty(), "a message is the minimum every failure owes the user");
    }

    #[test]
    fn a_provider_description_reaches_the_user_when_one_exists() {
        let out = signin_denial(
            accounts::BackendKind::Gmail,
            &auth::CallbackDenial {
                error: "access_denied".to_string(),
                subcode: None,
                description: Some("AADSTS65004: User declined to consent.".to_string()),
                state: None,
            },
        );
        assert!(
            out.message.contains("AADSTS65004: User declined to consent."),
            "the only provider-side detail available must not be swallowed: {}",
            out.message
        );
    }

    /// The wire contract the frontend reads. `adminConsentUrl` is camelCase
    /// there; a rename on either side breaks the Copy button silently, since
    /// a missing field just deserialises to `undefined`.
    #[test]
    fn the_payload_serialises_with_the_field_names_the_frontend_expects() {
        let json = serde_json::to_value(OauthError::plain("nope")).unwrap();
        assert_eq!(json["message"], "nope");
        assert!(json.get("adminConsentUrl").is_some(), "expected camelCase key in {json}");
    }
}

#[cfg(test)]
mod concurrent_write_tests {
    use super::*;

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> db::CachedNote {
        db::CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: String::new(),
            title: title.to_string(),
            body_html: body_html.to_string(),
            date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: 0,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        push_blocked_reason: None,
        }
    }

    #[test]
    fn apply_local_edit_with_retry_recomputes_against_fresh_state_after_a_lost_race() {
        let db = temp_db();
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note("a@x.com", uuid, "T", "<p>base</p>")).unwrap();

        let mut first_attempt = true;
        apply_local_edit_with_retry(&db, "a@x.com", uuid, 5, |existing| {
            if first_attempt {
                first_attempt = false;
                db.apply_local_edit(uuid, "a@x.com", &existing.title, "<p>base</p><p>racer</p>", &existing.label)
                    .unwrap();
            }
            Ok((existing.title.clone(), format!("{}<p>mine</p>", existing.body_html)))
        })
        .unwrap();

        let row = db.get(uuid, "a@x.com").unwrap().unwrap();
        assert!(row.body_html.contains("racer"), "the racer's edit must survive: {}", row.body_html);
        assert!(row.body_html.contains("mine"), "our edit must also land: {}", row.body_html);
    }
}

#[cfg(test)]
mod vertical_folder_reconcile_tests {
    use super::*;
    use crate::backend::{
        ChangeSet, RemoteFolder, SaveOp, SaveOutcome, SyncCursor, Transport, TransportError,
    };
    use async_trait::async_trait;

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    /// A bare `Transport` that only answers `list_folders`. Deliberately not a
    /// full `Vertical`: `reconcile_folders_from_vertical` needs exactly one
    /// method, and a stub carrying twenty `unimplemented!()`s would obscure
    /// which of them the code under test actually depends on.
    struct StubTransport {
        folders: Vec<RemoteFolder>,
    }

    #[async_trait]
    impl Transport for StubTransport {
        async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
            Ok(self.folders.clone())
        }
        async fn changes_since(&self, _c: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn save(&self, _op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn delete(&self, _id: &str) -> Result<(), TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn ensure_folder(&self, _p: &str) -> Result<RemoteFolder, TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn create_folder(&self, _n: &str) -> Result<RemoteFolder, TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn rename_folder(&self, _id: &str, _n: &str) -> Result<(), TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn delete_folder(&self, _id: &str) -> Result<(), TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
        async fn move_note(&self, _id: &str, _a: &[String], _r: &[String]) -> Result<Option<backend::RemoteNoteVersion>, TransportError> {
            unreachable!("reconcile only calls list_folders")
        }
    }

    fn folder(id: &str, path: &str) -> RemoteFolder {
        RemoteFolder { id: id.into(), path: path.into() }
    }

    #[tokio::test]
    async fn folders_from_a_vertical_land_in_the_cache_with_their_ids() {
        let db = temp_db();
        // Exchange's shape: a real folder literally named `Notes`, siblings
        // beside it rather than under it (the tree is flat — gotcha #12), and
        // ids that exist nowhere but this listing.
        let v = StubTransport {
            folders: vec![folder("id-a", "Notes"), folder("id-b", "L1")],
        };
        reconcile_folders_from_vertical(&db, "acct", &v, false).await.unwrap();

        let rows = db.list_folders("acct").unwrap();
        assert_eq!(rows.len(), 2, "got {:?}", rows.iter().map(|f| &f.path).collect::<Vec<_>>());
        assert!(
            rows.iter().any(|f| f.path == "Notes" && f.label_id.as_deref() == Some("id-a")),
            "the Exchange folder id must be stored so per-folder reads can address it: {rows:?}"
        );
        assert!(
            rows.iter().any(|f| f.path == "L1" && f.label_id.as_deref() == Some("id-b")),
            "a flat sibling of Notes must keep its own id too: {rows:?}"
        );
    }

    /// The failure this helper exists to prevent: `reconcile_folders_from_paths`
    /// stores the path as the id, which is correct for LocalFs and destroys the
    /// only copy of an Exchange folder id.
    #[tokio::test]
    async fn the_path_helper_would_have_lost_the_exchange_id() {
        let db = temp_db();
        reconcile_folders_from_paths(&db, "acct", &["L1".to_string()], false);
        let rows = db.list_folders("acct").unwrap();
        assert_eq!(rows[0].label_id.as_deref(), Some("L1"), "documents the wrong-helper behaviour");
    }

    #[tokio::test]
    async fn prune_drops_folders_the_backend_no_longer_reports() {
        let db = temp_db();
        let before = StubTransport { folders: vec![folder("id-a", "Notes"), folder("id-b", "L1")] };
        reconcile_folders_from_vertical(&db, "acct", &before, false).await.unwrap();

        let after = StubTransport { folders: vec![folder("id-a", "Notes")] };
        reconcile_folders_from_vertical(&db, "acct", &after, true).await.unwrap();

        let rows = db.list_folders("acct").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path, "Notes");
    }

    /// Gotcha #4: `kind` is derived from the path leaf on every reconciliation,
    /// so the `__*__` convention holds on this backend exactly as on Gmail.
    #[tokio::test]
    async fn a_workflow_named_folder_is_classified_by_its_leaf_here_too() {
        let db = temp_db();
        let v = StubTransport { folders: vec![folder("id-x", "__Extracts__")] };
        reconcile_folders_from_vertical(&db, "acct", &v, false).await.unwrap();

        let rows = db.list_folders("acct").unwrap();
        assert_eq!(rows[0].kind, "system_workflow");
    }

    /// Critical 3 from the whole-branch review (2026-08-15). Before
    /// `wire::folder_paths` rooted every real Exchange folder under
    /// `"Notes/"`, the sequence was: `ensure_workflow_folder` inserts
    /// `"Notes/__Extracts__"`; the worker pushes it and `mark_folder_created`
    /// marks the row `clean`; the next pull's scan reports the SAME Exchange
    /// folder back under its bare leaf `"__Extracts__"` (Exchange has no
    /// concept of `"Notes/"`); `prune_clean_folders` sees a `clean` row whose
    /// path isn't in the scan's keep-list and drops it; the next Extract
    /// re-inserts `"Notes/__Extracts__"` and the worker creates a SECOND
    /// Exchange folder. This drives that exact local sequence — create, push,
    /// reconcile against what the (post-fix) vertical now reports — and
    /// proves the row survives unchanged rather than being pruned.
    #[tokio::test]
    async fn a_pushed_workflow_folder_survives_reconciliation_against_the_same_scan() {
        let db = temp_db();
        let path = db.ensure_workflow_folder("acct", "__Extracts__").unwrap();
        assert_eq!(path, "Notes/__Extracts__");
        // Simulate a successful push (push_one_folder's DirtyNew arm): the
        // worker stamps the Exchange id and marks the row clean.
        db.mark_folder_created("acct", &path, "EXCHANGE-ID").unwrap();

        // The next pull's scan. Post-fix, MicrosoftVertical::list_folders
        // (via wire::to_folders / present_under_notes_root) reports this
        // folder back ALREADY rooted under Notes/ — matching the row's own
        // path instead of diverging to a bare "__Extracts__".
        let v = StubTransport { folders: vec![folder("EXCHANGE-ID", "Notes/__Extracts__")] };
        reconcile_folders_from_vertical(&db, "acct", &v, true).await.unwrap();

        let rows = db.list_folders("acct").unwrap();
        let extracts: Vec<_> = rows.iter().filter(|f| f.path.contains("Extracts")).collect();
        assert_eq!(
            extracts.len(), 1,
            "must not duplicate — got {:?}",
            rows.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
        assert_eq!(extracts[0].path, "Notes/__Extracts__");
        assert_eq!(extracts[0].label_id.as_deref(), Some("EXCHANGE-ID"), "must keep the pushed id, not a fresh one");
        assert_eq!(extracts[0].sync_state, db::FolderSyncState::Clean, "must survive prune, not be dropped");
    }

    /// The defect this whole helper exists to prevent, guarded at the point of
    /// *choice* rather than at the helpers themselves.
    ///
    /// `the_path_helper_would_have_lost_the_exchange_id` below pins what
    /// `reconcile_folders_from_paths` does, which stays true whoever calls it —
    /// so on its own it would still pass if someone re-pointed Microsoft back at
    /// that helper. This is the test that fails in that case, and both
    /// `list_notes` and `index_account` dispatch through the function it covers.
    #[test]
    fn each_backend_reconciles_from_the_only_source_that_carries_its_ids() {
        assert_eq!(
            folder_source_kind(accounts::BackendKind::Microsoft),
            FolderSourceKind::Vertical,
            "Exchange folder ids come only from the vertical's listing; the path \
             helper would store the path as the id and destroy them (gotcha #12)"
        );
        assert_eq!(
            folder_source_kind(accounts::BackendKind::LocalFs),
            FolderSourceKind::Paths,
            "on LocalFs the path IS the folder id — push_one_folder renames by it"
        );
        assert_eq!(
            folder_source_kind(accounts::BackendKind::Gmail),
            FolderSourceKind::Labels
        );
    }

    /// `list_notes` asserts this pairing at runtime via `debug_assert_eq!`; this
    /// pins the mapping itself, so a payload variant renamed or reordered
    /// without updating `kind()` is caught here rather than in a debug build.
    #[test]
    fn a_folder_source_payload_reports_the_helper_it_was_built_for() {
        assert_eq!(FolderSource::Labels(HashMap::new()).kind(), FolderSourceKind::Labels);
        assert_eq!(FolderSource::Paths(vec![]).kind(), FolderSourceKind::Paths);
        assert_eq!(FolderSource::Vertical.kind(), FolderSourceKind::Vertical);
    }

    #[test]
    fn sign_in_defaults_to_gmail_and_refuses_an_unknown_backend() {
        // Every pre-existing frontend call site sends no argument at all.
        assert_eq!(backend_kind_for_signin(None).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(backend_kind_for_signin(Some("gmail")).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(
            backend_kind_for_signin(Some("microsoft")).unwrap(),
            accounts::BackendKind::Microsoft
        );
        // Never silently fall back — that would sign the user into the wrong
        // provider and persist the account under it.
        assert!(backend_kind_for_signin(Some("local_fs")).is_err());
        assert!(backend_kind_for_signin(Some("Microsoft")).is_err());
    }

    /// Microsoft used to be refused on Android because its only redirect was
    /// the loopback listener Android cannot run. It now redirects through the
    /// same App Links URL as Gmail, so the platform no longer decides.
    #[test]
    fn android_accepts_microsoft_now_that_it_redirects_through_app_links() {
        assert_eq!(
            backend_kind_for_signin(Some("microsoft")).unwrap(),
            accounts::BackendKind::Microsoft
        );
        assert_eq!(backend_kind_for_signin(None).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(backend_kind_for_signin(Some("gmail")).unwrap(), accounts::BackendKind::Gmail);
        assert!(backend_kind_for_signin(Some("local_fs")).is_err());
    }

    /// `pending_backend` lives only in memory, so a process cold-started by
    /// the redirect Intent has a DEFAULT there — Gmail — whatever flow was
    /// actually started. The persisted entry carries the truth for that case.
    #[test]
    fn a_cold_started_callback_takes_its_backend_from_the_persisted_entry() {
        let persisted = secrets::PendingSignIn {
            pkce: auth::PkcePair::generate(),
            backend: accounts::BackendKind::Microsoft,
        };
        let expected = persisted.pkce.verifier.clone();
        let (pkce, backend) =
            resolve_pending_signin(None, accounts::BackendKind::Gmail, Some(persisted))
                .expect("the persisted entry completes the flow");
        assert_eq!(backend, accounts::BackendKind::Microsoft);
        assert_eq!(pkce.verifier, expected);
    }

    /// The in-memory pair is set together with `pending_backend` by
    /// `get_auth_url`, so when it is present the two agree and win over
    /// whatever the keychain holds.
    #[test]
    fn a_warm_callback_keeps_the_in_memory_pair_and_backend() {
        let in_mem = auth::PkcePair::generate();
        let expected = in_mem.verifier.clone();
        let stale = secrets::PendingSignIn {
            pkce: auth::PkcePair::generate(),
            backend: accounts::BackendKind::Gmail,
        };
        let (pkce, backend) =
            resolve_pending_signin(Some(in_mem), accounts::BackendKind::Microsoft, Some(stale))
                .expect("the in-memory pair completes the flow");
        assert_eq!(backend, accounts::BackendKind::Microsoft);
        assert_eq!(pkce.verifier, expected);
    }

    #[test]
    fn no_pending_sign_in_anywhere_is_none() {
        assert!(resolve_pending_signin(None, accounts::BackendKind::Gmail, None).is_none());
    }

    #[test]
    fn a_missing_client_id_is_refused_by_name_before_any_browser_opens() {
        // The fresh-checkout / missing-CI-secret case: without this check the
        // auth URL ships `client_id=`, Microsoft answers AADSTS900144 in the
        // browser, and the loopback listener hangs for its full timeout with
        // nothing emitted.
        //
        // Asserted against `refuse_empty_client_id` rather than the real
        // resolved id **on purpose** — see that function's doc comment. Reading
        // the live id here made this test silently vacuous on any machine whose
        // build embedded a client id.
        for empty in ["", "   ", "\t\n"] {
            let err = refuse_empty_client_id(empty, "MS_CLIENT_ID").unwrap_err();
            assert!(err.contains("MS_CLIENT_ID"), "must name the variable to set: {err}");
            assert!(err.contains(".env"), "must say where to set it: {err}");
        }
        assert!(refuse_empty_client_id("f95a0627-not-a-real-id", "MS_CLIENT_ID").is_ok());
        // Both providers route through the same check, so both name their own var.
        let err = refuse_empty_client_id("", "GOOGLE_CLIENT_ID").unwrap_err();
        assert!(err.contains("GOOGLE_CLIENT_ID"), "must name the variable to set: {err}");
        // LocalFs has no OAuth flow and must never be gated on a client id.
        assert!(refuse_missing_client_id(accounts::BackendKind::LocalFs).is_ok());
    }
}

#[cfg(test)]
mod icloud_replica_id_persistence_tests {
    use crate::accounts::{Account, BackendKind, AccountStatus, LlmConfig};

    /// Exercises `Account::ensure_icloud_replica_id()` directly, NOT the
    /// stateful wrapper `ensure_icloud_replica_id(state, account_id)` above —
    /// this codebase has no `AppState`/`tauri::AppHandle` test harness, so the
    /// wrapper's own lock/find/save mechanics (see its doc comment) have no
    /// direct test. What this test does cover, and the reason it exists: the
    /// comparison logic the wrapper relies on to decide whether to persist
    /// (before-vs-after the mint) behaves correctly on a corrupted stored
    /// value, which is the case the wrapper's fix was about.
    #[test]
    fn a_corrupted_replica_id_is_replaced_and_the_change_is_detectable() {
        // This test verifies the fix to `ensure_icloud_replica_id` wrapper:
        // it now compares the replica id before and after calling the Account method,
        // so both "never had one" and "had a corrupted one that got replaced" cases
        // persist correctly.
        //
        // The Account::ensure_icloud_replica_id() method:
        // - Returns existing valid UUID if present
        // - Mints a fresh UUID if field is None OR if stored value doesn't parse as UUID
        //
        // The wrapper must persist in both cases (None → Some and corrupted-Some → fresh-Some).
        // Prior to the fix, it only persisted when field was None before the call,
        // so corrupted values would be replaced in-memory but not persisted to disk,
        // causing infinite re-minting on each startup.

        let mut a = Account {
            id: "icloud:test@me.com".into(),
            email: "test@me.com".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: BackendKind::ICloud,
            root_dir: None,
            icloud_session_established: true,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: Some("not-a-uuid".into()),
            status: AccountStatus::Active,
            pending_removal: false,
        };

        // Before calling ensure_icloud_replica_id, capture the corrupted value
        let before = a.icloud_replica_id.clone();
        assert_eq!(before, Some("not-a-uuid".into()));

        // Call the method - it should mint a fresh UUID because the stored value
        // doesn't parse as a UUID
        let _id = a.ensure_icloud_replica_id();

        // After the call, verify the ID changed
        let after = a.icloud_replica_id.clone();
        assert_ne!(before, after, "ensure_icloud_replica_id should mint a fresh UUID when stored value is corrupted");
        assert_ne!(after, Some("not-a-uuid".into()), "corrupted value should be replaced");

        // Verify the new value is a valid UUID
        let stored_id = after.as_ref().expect("should have a new id");
        assert!(uuid::Uuid::parse_str(stored_id).is_ok(), "minted id should parse as UUID: {stored_id}");

        // The wrapper's logic should detect that `before != after` and persist.
        // We verify this by checking: if the wrapper used the old `had_one` logic,
        // it would not persist (because before was Some), causing the fresh UUID
        // to be lost on next startup. With the fixed before/after comparison,
        // the persistence happens regardless.
    }
}

/// The app icon must be FULL-BLEED — opaque all the way to the canvas edge.
///
/// macOS 26 (Tahoe) no longer draws an app icon the way the bundle supplies
/// it. Every icon is masked into the system's Liquid Glass squircle, and the
/// artwork's own alpha decides which of two very different treatments it gets:
///
///   * artwork that reaches the canvas edge  -> masked to the squircle and
///     drawn edge to edge, which is what every well-behaved app looks like;
///   * artwork with a transparent margin     -> the system draws its OWN light
///     grey plate behind it and insets the artwork inside that plate.
///
/// Jodd shipped the second shape: `source.svg` drew the background as
/// `<circle r="472">` on a 1024 canvas, so the corners AND the edge midpoints
/// were fully transparent. Tahoe therefore put the mark on a grey plate and
/// shrank it — and because the mark's own field was a near-white `#f3f1ec`,
/// the result at App Switcher size read as an empty grey tile. The icon was
/// present and valid the whole time (`CFBundleIconFile`, the `.icns` and every
/// slice inside it were correct); only the alpha at the edge was wrong.
///
/// This was isolated by a controlled experiment, not inferred: two bundles
/// built through the same pipeline with the same colour at the same size, one
/// full-bleed and one a circle-with-margin, resolved to the two treatments
/// above. Canvas-edge alpha was the single variable.
///
/// So the rule this pins is the one the experiment established, and it is
/// checked on the sources a build actually consumes rather than on the `.icns`
/// — `tauri.conf.json`'s `bundle.icon` list is where a regenerated icon set
/// enters the build, and a margin reintroduced there is invisible until
/// somebody looks at Cmd+Tab on a Mac.
#[cfg(test)]
mod app_icon {
    use std::path::{Path, PathBuf};

    fn icons_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("icons")
    }

    /// Alpha of one pixel, from a PNG decoded to 8-bit RGBA.
    struct Rgba {
        width: usize,
        height: usize,
        pixels: Vec<u8>,
    }

    impl Rgba {
        fn alpha_at(&self, x: usize, y: usize) -> u8 {
            self.pixels[(y * self.width + x) * 4 + 3]
        }
    }

    fn decode(path: &Path) -> Rgba {
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
        let mut reader = png::Decoder::new(file)
            .read_info()
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()));
        buf.truncate(info.buffer_size());

        // Normalise whatever colour type the file uses into RGBA8. An icon
        // encoded without an alpha channel is opaque everywhere, which passes
        // this rule on its own terms.
        let pixels = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            png::ColorType::Grayscale => {
                buf.iter().flat_map(|&v| [v, v, v, 255]).collect()
            }
            png::ColorType::GrayscaleAlpha => buf
                .chunks_exact(2)
                .flat_map(|p| [p[0], p[0], p[0], p[1]])
                .collect(),
            other => panic!("{}: unexpected colour type {other:?}", path.display()),
        };
        assert_eq!(
            info.bit_depth,
            png::BitDepth::Eight,
            "{}: expected 8-bit samples",
            path.display()
        );
        Rgba {
            width: info.width as usize,
            height: info.height as usize,
            pixels,
        }
    }

    /// The PNGs `tauri.conf.json` lists in `bundle.icon`, plus the 1024 master
    /// they are all derived from. `icon.icns` and `icon.ico` are containers
    /// built from these, so fixing the sources fixes them too.
    const MACOS_RELEVANT: &[&str] = &[
        "source-1024.png",
        "icon.png",
        "128x128@2x.png",
        "128x128.png",
        "32x32.png",
    ];

    #[test]
    fn app_icon_is_full_bleed() {
        let dir = icons_dir();
        let mut transparent = Vec::new();

        for name in MACOS_RELEVANT {
            let path = dir.join(name);
            let img = decode(&path);
            let (w, h) = (img.width, img.height);

            // The four corners tell a circle from a square; the four edge
            // midpoints tell an inset square from a full-bleed one. Jodd's old
            // icon failed both, which is why both are checked.
            let probes = [
                ("top-left", 0, 0),
                ("top-right", w - 1, 0),
                ("bottom-left", 0, h - 1),
                ("bottom-right", w - 1, h - 1),
                ("top-middle", w / 2, 0),
                ("bottom-middle", w / 2, h - 1),
                ("left-middle", 0, h / 2),
                ("right-middle", w - 1, h / 2),
            ];

            for (where_, x, y) in probes {
                let a = img.alpha_at(x, y);
                if a != 255 {
                    transparent.push(format!("  {name} {where_} ({x},{y}): alpha={a}"));
                }
            }
        }

        assert!(
            transparent.is_empty(),
            "app icon artwork does not reach the canvas edge, so macOS 26 will \
             draw it on its own grey plate instead of masking it to the \
             squircle — in the App Switcher that reads as a blank tile. Redraw \
             icons/source.svg so the background fills the full 1024x1024 canvas \
             (no circle, no margin) and regenerate. Transparent probes:\n{}",
            transparent.join("\n")
        );
    }
}

#[cfg(test)]
mod package_c_persistence_tests {
    use super::*;
    #[test]
    fn sqlite_status_survives_reopen_rekey_and_an_edit_during_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let a = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some("same"), "A", "body", "Notes", None, None).unwrap();
        let b = save_note_db(&db, "b", accounts::BackendKind::Microsoft, Some(&a.uuid), "B", "other", "Notes", None, None).unwrap();
        // Offline/transient transport failure never calls mark_pushed.
        assert_eq!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().sync_state, db::SyncState::Dirty);
        let newer = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some(&a.uuid), "A", "new edit", "Notes", None, Some(a.local_version)).unwrap();
        let saved = backend::SavedNote { id: "remote".into(), uuid: "assigned".into(), version: "v1".into(), date: "today".into(), body_html: a.body_html.clone(), local_version: 0 };
        push_one_dirty_db(&db, &a, &saved, "assigned", true, true).unwrap();
        let status = note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap();
        assert_eq!(status.uuid, "assigned");
        assert_eq!(status.local_version, newer.local_version);
        assert_eq!(status.sync_state, db::SyncState::Dirty);
        assert_eq!(note_persistence_db(&db, "b", &b.uuid).unwrap().unwrap().uuid, b.uuid);
        db.mark_push_blocked("assigned", "a", "permanent refusal").unwrap();
        let blocked = note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap();
        assert_eq!(blocked.push_blocked_reason.as_deref(), Some("permanent refusal"));
        assert_eq!(blocked.sync_state, db::SyncState::Dirty);
        drop(db);
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        assert!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().push_blocked_reason.is_some());
        let retry = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some(&a.uuid), "A", "final edit", "Notes", None, Some(newer.local_version)).unwrap();
        push_one_dirty_db(&db, &retry, &saved, "assigned", false, false).unwrap();
        assert_eq!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().sync_state, db::SyncState::Clean);
        assert_eq!(note_persistence_db(&db, "b", &b.uuid).unwrap().unwrap().sync_state, db::SyncState::Dirty);
        assert!(note_persistence_db(&db, "missing", &a.uuid).unwrap().is_none());
    }
    #[test]
    fn mutations_follow_rekeys_only_inside_the_requested_account() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        for u in ["single", "batch"] {
            crate::test_support::note("a", u).insert(&db);
            crate::test_support::note("b", u).insert(&db);
            db.rekey_note_uuid(u, &format!("new-{u}"), "a").unwrap();
        }
        db.set_pin("single", "a", true).unwrap();
        assert!(db.get("new-single", "a").unwrap().unwrap().pinned);
        assert_eq!(note_persistence_db(&db, "a", "single").unwrap().unwrap().sync_state, db::SyncState::Dirty);
        db.set_pin_batch("a", &["batch".into()], true).unwrap();
        assert!(db.get("new-batch", "a").unwrap().unwrap().pinned);
        assert!(!db.get("single", "b").unwrap().unwrap().pinned);
        db.mark_deleted("single", "a").unwrap();
        db.delete_notes_batch("a", &["batch".into()]).unwrap();
        assert_eq!(db.get("new-single", "a").unwrap().unwrap().sync_state, db::SyncState::DeletedPending);
        assert_eq!(db.get("new-batch", "a").unwrap().unwrap().sync_state, db::SyncState::DeletedPending);
        assert_eq!(db.get("batch", "b").unwrap().unwrap().sync_state, db::SyncState::Clean);
    }
    #[test]
    fn worker_receipt_matches_the_typescript_contract() {
        let value = serde_json::to_value(PushConfirmation { account_id: "a".into(), uuid: "u".into(), body_html: "body".into() }).unwrap();
        assert_eq!(value["accountId"], "a");
        assert_eq!(value["bodyHtml"], "body");
    }
}
