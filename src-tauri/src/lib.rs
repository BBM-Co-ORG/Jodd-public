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

// Timestamped log: prints `[jodd HH:MM:SS.mmm] ...` to stderr, and — when
// file logging is enabled (default on, see applog.rs) — appends the same
// line to the persistent log file so it survives past the current process.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        let line = format!(
            "[jodd {}] {}",
            chrono::Local::now().format("%H:%M:%S%.3f"),
            format_args!($($arg)*)
        );
        eprintln!("{}", line);
        $crate::applog::write_line(&line);
    }};
}

pub struct AppState {
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
    /// Cancels the loopback listener of an in-flight sign-in. A single slot,
    /// because only one OAuth flow can be in progress — and because the second
    /// attempt has to be able to reclaim port 8080 from an abandoned first one.
    pub oauth_cancel: Mutex<Option<tokio_util::sync::CancellationToken>>,
    /// Serializes `sync_worker_tick` so the periodic 5s loop and an
    /// on-demand `flush_sync` call (Task 9, Android visibility flush) can
    /// never execute the tick body concurrently. Before `flush_sync`
    /// existed, exactly one call site ever ran a tick, so the read-then-
    /// push-then-write sequence inside it (`list_dirty` → network save →
    /// `mark_pushed`) was implicitly single-threaded. `flush_sync` can now
    /// fire at any moment, including mid-tick: two overlapping ticks could
    /// both `list_dirty()` the same still-not-yet-`mark_pushed` row and
    /// both push it, producing two Gmail messages for one uuid (an orphan
    /// once `mark_pushed` from the second call overwrites the first's id).
    /// `AppState.pushing` does not prevent this — it only stops a
    /// concurrent *poll* from misreading an in-flight push as a remote
    /// conflict, not from two *pushes* racing each other. A `tokio::sync`
    /// mutex (not `std::sync`, since the guard is held across `.await`)
    /// held for the whole tick body makes overlap impossible: a tick that
    /// starts while another is running waits for it to finish first, so
    /// `list_dirty()` always sees the up-to-date `sync_state`.
    pub sync_tick_lock: tokio::sync::Mutex<()>,
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

/// May this account be removed right now?
///
/// `remove_account` deletes the refresh token before anything else, so a
/// draining account would lose its credential while still pushing. The UI
/// offers Stop waiting instead — instant, and it names the cost — after which
/// removal is safe because the queue is empty.
fn removal_allowed(status: accounts::AccountStatus) -> bool {
    status != accounts::AccountStatus::Draining
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
    }
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
/// `android` is a parameter rather than a `cfg!` so the refusal is testable on
/// a desktop host. **Microsoft sign-in cannot work on Android at all today:**
/// the flow is loopback, and `auth::wait_for_callback_blocking` is `cfg`-ed out
/// there (it binds `0.0.0.0:8080`, which on a phone is an unauthenticated
/// server every other app can reach). Without this refusal the browser would
/// open, the user would consent, and nothing would ever come back — no
/// callback, no error, no timeout. Gmail's Android path is an entirely separate
/// four-constraint App Links chain (gotcha #8) which Microsoft has no
/// counterpart to yet.
fn backend_kind_for_signin(backend: Option<&str>, android: bool) -> Result<accounts::BackendKind, String> {
    match backend {
        None | Some("") | Some("gmail") => Ok(accounts::BackendKind::Gmail),
        Some("microsoft") if android => Err(
            "Microsoft sign-in is not supported on Android yet — it needs a loopback \
             redirect, which Android builds do not run. Add the account on desktop."
                .to_string(),
        ),
        Some("microsoft") => Ok(accounts::BackendKind::Microsoft),
        // LocalFs is deliberately absent: it has no OAuth flow at all
        // (`add_local_account` is its entry point).
        Some(other) => Err(format!("unknown sign-in backend '{other}'")),
    }
}

/// Refuse a sign-in whose OAuth client id is unset, **before the browser opens**.
///
/// `auth_ms::client_id()` is `env::var(..).unwrap_or_default()`, so a missing
/// `MS_CLIENT_ID` puts a bare `client_id=` into the auth URL. Microsoft then
/// renders AADSTS900144 in the browser and never redirects — so the loopback
/// listener waits out its full five-minute timeout while the frontend shows
/// nothing at all. Naming the variable here turns a silent hang into a fixable
/// error. Same check for Google, whose `client_id()` has config-file and
/// compile-time-embedded tiers above the env var and so is rarely empty in a
/// release build, but is trivially empty in a fresh checkout with no `.env`.
fn refuse_missing_client_id(kind: accounts::BackendKind) -> Result<(), String> {
    let (id, var) = match kind {
        accounts::BackendKind::Microsoft => (auth_ms::client_id(), "MS_CLIENT_ID"),
        accounts::BackendKind::Gmail => (auth::client_id(), "GOOGLE_CLIENT_ID"),
        // No OAuth flow — never reached via `backend_kind_for_signin`.
        accounts::BackendKind::LocalFs => return Ok(()),
    };
    if id.trim().is_empty() {
        return Err(format!(
            "{var} is not set, so this build cannot start a sign-in. Add it to .env \
             (see .env.example) and restart Jodd."
        ));
    }
    Ok(())
}

#[tauri::command]
async fn get_auth_url(
    backend: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let kind = backend_kind_for_signin(backend.as_deref(), cfg!(target_os = "android"))?;
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
    // `secrets::save_pending_pkce` for the full reasoning. Best-effort: log
    // and continue rather than fail the whole flow on a write error — the
    // in-memory slot alone is already sufficient on desktop and on any
    // Android launch that isn't evicted mid-flow.
    if let Err(e) = secrets::save_pending_pkce(&pair) {
        log!("get_auth_url: failed to persist PKCE pair: {}", e);
    }
    *state.pending_pkce.lock().unwrap() = Some(pair);
    Ok(url)
}

/// Finish an OAuth flow given the `code` and `state` returned by the
/// authorization server. Shared by the desktop loopback listener and the
/// Android deep-link handler — the trigger differs, the security checks and
/// account persistence must not.
async fn complete_oauth(app: AppHandle, code: String, cb_state: String) {
    log!("complete_oauth: received auth code (len={})", code.len());
    let state = app.state::<AppState>();
    let pkce = state.pending_pkce.lock().unwrap().take();
    // Fall back to the persisted copy when the in-memory slot is empty —
    // this may be a fresh process that cold-launched from the redirect
    // Intent and never ran `get_auth_url` itself (see that function's
    // comment). Whichever source it came from, clear the persisted copy
    // unconditionally right here, before any check below can short-circuit
    // out: every subsequent return path (missing entirely, state mismatch,
    // exchange failure, success) must leave nothing behind for a stray retry
    // to replay. A leftover pair is worse than none.
    let pkce = pkce.or_else(secrets::load_pending_pkce);
    secrets::clear_pending_pkce();
    let Some(pkce) = pkce else {
        log!("complete_oauth: PKCE verifier MISSING");
        let _ = app.emit("oauth-error", "PKCE verifier missing");
        return;
    };
    // OAuth `state` CSRF check (RFC 6749 §10.12). Constant-time compare so a
    // timing oracle can't be used to fish out the expected value byte-by-byte.
    if !constant_time_eq(cb_state.as_bytes(), pkce.state.as_bytes()) {
        log!("complete_oauth: state mismatch — possible CSRF, aborting");
        let _ = app.emit("oauth-error", "OAuth state mismatch — request rejected");
        return;
    }
    // Which provider this flow was started against. Taken (not peeked) so an
    // abandoned Microsoft attempt cannot leave the slot set for a later Gmail
    // sign-in — same one-shot discipline as the PKCE pair above.
    let backend_kind = std::mem::take(&mut *state.pending_backend.lock().unwrap());
    let token_data = match backend_kind {
        accounts::BackendKind::Microsoft => auth_ms::exchange_code(&code, &pkce.verifier).await,
        _ => auth::exchange_code(&code, &pkce.verifier).await,
    };
    let token_data = match token_data {
        Ok(td) => td,
        Err(e) => {
            log!("complete_oauth: token exchange FAILED: {}", e);
            let _ = app.emit("oauth-error", e);
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
            let _ = app.emit("oauth-error", format!("get user profile: {}", e));
            return;
        }
    };
    log!("complete_oauth: resolved account email = {}", email);

    // Persist refresh token to keychain under per-account key.
    if let Some(rt) = token_data.refresh_token.as_ref() {
        if let Err(e) = accounts::save_refresh_token(&email, rt) {
            log!("complete_oauth: keychain write failed: {}", e);
        } else {
            log!("complete_oauth: refresh token saved for {}", email);
        }
    }

    // Add or update the account in the persisted list.
    {
        let mut list = state.accounts.lock().unwrap();
        if !list.iter().any(|a| a.id == email) {
            list.push(Account {
                id: email.clone(),
                email: email.clone(),
                added_at: chrono::Utc::now().to_rfc3339(),
                // Leave label config unset — effective_*_label
                // resolves to DEFAULT_* until the user customizes.
                notes_label: None,
                meta_label: None,
                llm: Default::default(),
                backend_kind,
                root_dir: None,
                status: accounts::AccountStatus::Active,
            });
            if let Err(e) = accounts::save_accounts(&list) {
                log!("complete_oauth: save_accounts failed: {}", e);
            }
        }
    }

    // Cache the access token in this account's state.
    {
        let mut states = state.account_states.lock().unwrap();
        let entry = states.entry(email.clone()).or_default();
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
    // (`secrets::save_pending_pkce`), which is what makes a cold-start callback
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
                Ok(Ok(cb)) => complete_oauth(app_clone, cb.code, cb.state).await,
                Ok(Err(e)) => {
                    log!("open_auth_url: wait_for_callback FAILED: {}", e);
                    let _ = app_clone.emit("oauth-error", e.to_string());
                }
                Err(e) => {
                    log!("open_auth_url: listener task panicked: {}", e);
                    let _ = app_clone.emit("oauth-error", "sign-in listener crashed");
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

#[tauri::command]
async fn remove_account(account_id: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(status) = account_status_of(&state, &account_id) {
        if !removal_allowed(status) {
            return Err(format!(
                "{} is still finishing sync. Use \"Stop waiting\" first, then remove it.",
                account_id
            ));
        }
    }
    accounts::delete_refresh_token(&account_id);
    {
        let mut list = state.accounts.lock().unwrap();
        list.retain(|a| a.id != account_id);
        accounts::save_accounts(&list)?;
    }
    state
        .account_states
        .lock()
        .unwrap()
        .remove(&account_id);
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
        .retain(|(aid, _)| aid != &account_id);
    // Drop any stale dup_stats so the sidebar pill doesn't linger after sign-out.
    state.dup_stats.lock().unwrap().remove(&account_id);
    // Wipe the local replica for this account. Keeping rows around after
    // remove would (a) leak note bodies on disk for an account the user
    // thinks they signed out of, and (b) confuse any sync worker that
    // wakes up while the keychain entry is gone.
    match state.db.delete_account(&account_id) {
        Ok((n, f)) => log!(
            "remove_account: wiped {} note row(s) and {} folder row(s) for {}",
            n, f, account_id
        ),
        Err(e) => log!("remove_account: cache wipe failed for {}: {}", account_id, e),
    }
    Ok(())
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

const SIDECARS_UNAVAILABLE_MSG: &str =
    "Pinning and tags aren't available on Microsoft accounts yet — they land in \
     Milestone 4. Editing notes and folders works normally.";

const NOTES_UNAVAILABLE_MSG: &str =
    "This account can't accept note edits in Jodd yet. Edit the note in Apple \
     Notes and it will sync back here.";

const FOLDERS_UNAVAILABLE_MSG: &str =
    "This account can't accept folder changes in Jodd yet. Create or rename the \
     folder in Apple Notes and it will sync back here.";

/// The decision `refuse_write` makes, without the `State` around it.
/// `None` for an unknown account — every command has its own not-found path.
fn write_refusal_for(kind: Option<accounts::BackendKind>, w: backend::Write)
    -> Option<&'static str>
{
    let k = kind?;
    if Capabilities::for_backend(k).writes.allows(w) { return None; }
    Some(match w {
        backend::Write::Notes => NOTES_UNAVAILABLE_MSG,
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
/// whose `put_sidecar` is still `Err(milestone_2())` (Microsoft, pre-M4)
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
        accounts::BackendKind::LocalFs | accounts::BackendKind::Microsoft => false,
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
    let id = format!("localfs:{}", crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()));
    let account = accounts::Account {
        id: id.clone(),
        email: name,
        added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None,
        meta_label: None,
        llm: accounts::LlmConfig::default(),
        backend_kind: accounts::BackendKind::LocalFs,
        root_dir: Some(path.clone()),
        status: accounts::AccountStatus::Active,
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
) -> Result<accounts::Account, String> {
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
        // is_ready_local() dispatches per BackendKind — Gmail checks the keychain
        // for a refresh token; LocalFs checks that root_dir exists on disk.
        // Neither path touches the network (data doctrine: readiness ≠ network).
        let has_creds = acct.is_ready_local();
        let has_cache = state.db.has_cached_notes(&acct.id).unwrap_or_else(|e| {
            log!("is_authenticated: has_cached_notes({}) failed: {} — treating as no cache", acct.id, e);
            false
        });
        if account_is_usable(has_cache, has_creds) {
            log!(
                "is_authenticated: {} usable (cache={}, creds={}) → true",
                acct.id, has_cache, has_creds
            );
            return Ok(true);
        }
    }
    log!("is_authenticated: no accounts usable (no cache, no creds) → false");
    Ok(false)
}

// ─── Sync reconciliation ─────────────────────────────────────────────────────
//
// Called for each note that comes back from a Gmail fetch. Decides what to
// do based on the local sync_state AND whether the remote actually changed
// since we last saw it (by comparing remote_version vs the fetched
// backend::Note::version — NOT the fetched id; see that field's doc comment).
//
// See docs/DATA-HANDLING.md §8 (conflict handling) for the design.

/// Compute a short descriptor for the device that generated the local copy.
/// Used in conflict-copy titles so the user can tell which device the
/// remote version came from vs the one currently in front of them.
fn device_label() -> String {
    let os = std::env::consts::OS;
    let pretty = match os {
        "macos" => "Mac",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    pretty.to_string()
}

/// Has the remote copy moved since we last synced this row?
///
/// Split out of `reconcile_one` so it can be tested without a DB, an account
/// or a network. `None` (never synced) counts as changed.
///
/// An empty `fetched_version` ALSO counts as changed, unconditionally — even
/// against a cache that is itself `Some("")`. This is Microsoft-specific:
/// `RawMessage::last_modified_date_time` is `#[serde(default)]`, so a message
/// missing that field decodes to `""` and `to_note` stores it verbatim (see
/// that field's doc comment for why it is NOT run through the same fallback
/// `date` gets). Without this guard such a note would carry `version: ""`
/// forever, `remote_changed` would report `false` on every poll, and an
/// Apple-side edit would be silently overwritten with no conflict copy —
/// which is the exact failure this function exists to prevent, re-entering
/// through a side door. Failing toward "changed" here costs a conflict copy
/// (both sides preserved, the user reconciles); failing toward "unchanged"
/// costs a silently dropped edit. Only the first is acceptable.
fn remote_changed(cached_version: Option<&str>, fetched_version: &str) -> bool {
    if fetched_version.is_empty() {
        return true;
    }
    cached_version != Some(fetched_version)
}

/// Reconcile a single fetched note against the cache. Implements the full
/// Phase 4 decision table. Takes the AppState so it can check whether our
/// own sync worker is mid-push for this uuid (which would make a
/// "remote changed" observation a false alarm).
fn reconcile_one(state: &State<'_, AppState>, account_id: &str, fetched: &gmail::Note) {
    // If our own worker is in the middle of pushing this uuid, the fetched
    // id likely reflects our own in-flight insert — not someone else's
    // edit. Skip reconcile entirely; we'll process this row again on the
    // next list_notes after mark_pushed has updated the cache.
    {
        let pushing = state.pushing.lock().unwrap();
        if pushing.contains(&(account_id.to_string(), fetched.uuid.clone())) {
            return;
        }
    }
    reconcile_one_db(&state.db, account_id, fetched);
}

/// The DB-only body of `reconcile_one` — everything after the in-flight-push
/// check above. Split out so it can be exercised in tests against a plain
/// `db::Db`: `tauri::State` has no public constructor outside a running
/// Tauri app, so nothing that needs it can be unit-tested directly (which is
/// exactly why the regression coverage for the `mark_pushed`/`SavedNote`
/// version-mismatch bug — `localfs_push_reconcile_tests` below — calls this,
/// not `reconcile_one`).
fn reconcile_one_db(db: &db::Db, account_id: &str, fetched: &gmail::Note) {
    let cached = db::CachedNote::from_remote(account_id, fetched);

    // Persist any inline attachments (images) so the save path can re-emit them
    // instead of stripping them on re-save. Upsert-only (never deletes on read);
    // runs regardless of the reconcile decision below, since the bytes are the
    // canonical Gmail-side content keyed by stable cid. Errors are logged, not
    // fatal — attachment capture must never break note sync.
    for att in &fetched.attachments {
        if let Err(e) = db.upsert_attachment(account_id, &cached.uuid, att) {
            log!(
                "reconcile_one: upsert_attachment failed uuid={} cid={}: {}",
                cached.uuid, att.content_id, e
            );
        }
    }

    let existing = match db.get(&cached.uuid, account_id) {
        Ok(x) => x,
        Err(e) => {
            log!("reconcile_one: db.get failed for {}: {}", cached.uuid, e);
            return;
        }
    };

    let Some(existing) = existing else {
        // No row → insert fresh. The note is new to us.
        if let Err(e) = db.upsert_from_remote(&cached) {
            log!("reconcile_one: insert failed for {}: {}", cached.uuid, e);
        }
        return;
    };

    use db::SyncState::*;
    let remote_changed = remote_changed(existing.remote_version.as_deref(), &fetched.version);

    match existing.sync_state {
        // User wants this gone — don't resurrect by pulling.
        DeletedPending => {}
        // Already flagged — don't keep re-creating duplicate "conflict copy"
        // rows on every poll. The user has to resolve manually.
        Conflict => {}
        // Local has unpushed edits. The interesting case.
        Dirty => {
            if remote_changed {
                // CONFLICT detected. The "keep-both" rule, refined per design:
                //
                // The PRIMARY note (uuid=X) converges to the REMOTE state —
                // so all replicas agree on uuid=X's content. The LOCAL
                // content (the one that was about to be overwritten) is
                // preserved as a new conflict-copy note with a fresh uuid.
                //
                // Earlier version of this code did the opposite (kept local
                // on the primary, remote in the copy) but that produced an
                // asymmetry: Apple Notes/Gmail had remote content under
                // uuid=X, Jodd had local content under uuid=X — same
                // identity, different content across replicas. Confusing.
                //
                // Now both replicas show the same picture: primary has
                // remote, conflict-copy has the "device's earlier version".
                let new_uuid = crate::mime822::format_apple_uuid(uuid::Uuid::new_v4());
                let date_str = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
                let suffix = format!(" (conflict from {} {})", device_label(), date_str);
                let dup_title = format!("{}{}", existing.title, suffix);
                let now = db::now_ms();
                let dup = db::CachedNote {
                    uuid: new_uuid,
                    account_id: account_id.to_string(),
                    id: String::new(),
                    title: dup_title,
                    body_html: existing.body_html.clone(),     // LOCAL preserved
                    date: existing.date.clone(),
                    x_mail_created_date: existing.x_mail_created_date.clone(),
                    label: existing.label.clone(),
                    local_version: 1,
                    remote_version: None,
                    sync_state: Dirty,
                    last_synced_at: None,
                    last_local_modified_at: now,
                    last_remote_modified_at: None,
                    // Conflict copies start unpinned regardless of the
                    // primary's pin state — the user is being asked to
                    // pick between two versions, and surfacing the copy
                    // at the top of the list would be misleading. They
                    // can pin the survivor after resolving. No sidecar
                    // until the user explicitly pins the copy.
                    pinned: false,
                    meta_msg_id: None,
                    pin_dirty: false,
                };
                if let Err(e) = db.insert_local_new(&dup) {
                    log!("reconcile_one: insert conflict-copy failed for {}: {}",
                         existing.uuid, e);
                    return;
                }
                // Conflict-copy inherits the primary's tags. Without this the
                // copy starts untagged and whichever side the user picks (by
                // deleting the other) costs them their tag state on that note.
                // Best-effort: a copy failure isn't worth aborting the whole
                // reconcile, but log so we notice if it's a recurring problem.
                if let Err(e) = db.copy_tags(account_id, &existing.uuid, &dup.uuid) {
                    log!("reconcile_one: copy_tags to conflict-copy failed for {} → {}: {}",
                         existing.uuid, dup.uuid, e);
                }
                // Now accept remote into the primary. upsert_from_remote
                // sets sync_state = clean, so the worker won't push the
                // (now-irrelevant) local content under uuid=X. The local
                // content survives in `dup` which the worker WILL push.
                if let Err(e) = db.upsert_from_remote(&cached) {
                    log!("reconcile_one: apply remote on conflict failed for {}: {}",
                         cached.uuid, e);
                } else {
                    log!(
                        "reconcile_one: CONFLICT on uuid={} — saved local content as duplicate uuid={} (\"{}\"), accepted remote into primary",
                        existing.uuid, dup.uuid, dup.title
                    );
                }
            }
            // remote unchanged → keep dirty, worker will push our edits.
        }
        // No pending local intent. Apply remote.
        Clean | PullNeeded => {
            if let Err(e) = db.upsert_from_remote(&cached) {
                log!("reconcile_one: upsert failed for {}: {}", cached.uuid, e);
            }
        }
    }
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

    log!(
        "list_notes: returning {} notes for {}",
        result.len(),
        account_id
    );
    Ok(result)
}

/// Local-first save. Writes to the SQLite replica synchronously and returns
/// immediately. The actual Gmail push happens in the background sync worker.
///
/// What this does NOT do:
///   - Call Gmail. Network round-trip is the worker's job.
///   - Wait for sync to complete. UI gets "Saved" feedback as soon as the
///     local row is committed.
///
/// What the worker eventually does with this row:
///   - Reads `dirty` rows
///   - Calls gmail::save_note (insert new + trash old)
///   - On success: mark_pushed(uuid, new_id) → sync_state = clean, id updated
///   - On failure: leaves dirty, retries next cycle
#[tauri::command]
async fn save_note(
    account_id: String,
    title: String,
    body_html: String,
    // `existing_gmail_id` is no longer used — Rust reads it from cache.
    // Kept as a parameter for backward compat during the migration; will
    // be dropped once the frontend stops sending it.
    #[allow(unused_variables)]
    existing_gmail_id: Option<String>,
    existing_uuid: Option<String>,
    existing_x_mail_created_date: Option<String>,
    label: String,
    // The `local_version` this editor session saw when it loaded (or last
    // successfully saved) this note — NOT re-derived inside this command,
    // which is exactly what made Task 5's first attempt at this guard
    // ineffective (see the Addendum above). `None` means the frontend never
    // learned a version (e.g. it hasn't picked up this feature yet, or this
    // is a brand-new note) — falls back to an unguarded write rather than
    // blocking a save the frontend can't version.
    expected_local_version: Option<i64>,
    state: State<'_, AppState>,
) -> Result<gmail::SavedNote, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let db = state.db.clone();

    // Resolve the canonical UUID. tmp: prefixes from a fresh + click and
    // empty strings both mean "this is a brand-new note — generate one".
    let real_uuid = match existing_uuid.as_deref() {
        Some(u) if !u.is_empty() && !u.starts_with("tmp:") => {
            crate::mime822::canonicalize_uuid(u).unwrap_or_else(|| u.to_string())
        }
        _ => crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()),
    };

    // Apply edit if the row already exists, otherwise insert new.
    let existing = db.get(&real_uuid, &account_id).map_err(|e| e.to_string())?;
    if existing.is_some() {
        let applied = match expected_local_version {
            Some(expected) => db
                .apply_local_edit_versioned(&real_uuid, &account_id, &title, &body_html, &label, expected)
                .map_err(|e| e.to_string())?,
            None => {
                db.apply_local_edit(&real_uuid, &account_id, &title, &body_html, &label)
                    .map_err(|e| e.to_string())?;
                true
            }
        };
        if !applied {
            // A second local writer (another device's Jodd, or jodd-mcp) landed
            // an edit on this note after `expected_local_version` was captured
            // — a full-body replace here would silently discard that edit.
            // Surface it instead of retrying: unlike an append, there's
            // nothing safe to recompute automatically.
            return Err(
                "This note changed elsewhere while you were editing (another device or an MCP agent). \
                 Reload the note and re-apply your changes before saving again.".to_string()
            );
        }
    } else {
        let now = db::now_ms();
        let new_note = db::CachedNote {
            uuid: real_uuid.clone(),
            account_id: account_id.clone(),
            id: String::new(), // no Gmail id yet — worker will fill it in
            title: title.clone(),
            body_html: body_html.clone(),
            // Frontend treats this date as "last modified" — set to now for a
            // new local note. The worker will overwrite with the real Date
            // header when Gmail confirms.
            date: chrono::Local::now().to_rfc2822(),
            x_mail_created_date: existing_x_mail_created_date.clone(),
            label: label.clone(),
            local_version: 1,
            remote_version: None,
            sync_state: db::SyncState::Dirty,
            last_synced_at: None,
            last_local_modified_at: now,
            last_remote_modified_at: None,
            // New notes start unpinned with no sidecar yet. User toggles
            // via set_pin from the menu; the worker creates the sidecar.
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        };
        db.insert_local_new(&new_note).map_err(|e| e.to_string())?;
    }

    // Read back the row so the response reflects current state (most
    // importantly: the cached `id` if any prior push has succeeded).
    let cached = db.get(&real_uuid, &account_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "save_note: row vanished after write".to_string())?;

    // For LocalFS accounts push the file to disk right now, synchronously.
    // A local filesystem write completes in < 1ms — there is no benefit to
    // deferring it to the 5-second worker tick, and deferring creates a race:
    // if the user deletes the note before the worker runs, id is still "" and
    // push_one_deletion drops the DB row without moving anything to .trash/,
    // so the note never appears in "Recently Deleted".
    let acct_kind = state.accounts.lock().unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind.clone());
    if matches!(acct_kind, Some(accounts::BackendKind::LocalFs)) {
        let existing_id = if cached.id.is_empty() { None } else { Some(cached.id.as_str()) };
        let attachments = db.list_attachments(&account_id, &real_uuid).unwrap_or_default();
        let v = vertical_for(&state, &account_id).await?;
        let op = crate::backend::SaveOp {
            title: &cached.title,
            body_html: &cached.body_html,
            existing_remote_id: existing_id,
            existing_uuid: Some(cached.uuid.as_str()),
            existing_created_date: cached.x_mail_created_date.as_deref(),
            label: &cached.label,
        };
        let saved = v.save_note_full(&op, &attachments).await.map_err(|e| e.to_string())?;
        db.mark_pushed(&real_uuid, &account_id, &saved.id, &saved.version, &saved.date, &saved.body_html, cached.local_version)
            .map_err(|e| e.to_string())?;
        log!(
            "save_note (localfs-sync): uuid={} id={}",
            real_uuid, saved.id
        );
        return Ok(gmail::SavedNote {
            id: saved.id,
            version: saved.version,
            uuid: cached.uuid,
            date: saved.date,
            body_html: saved.body_html,
            local_version: cached.local_version,
        });
    }

    log!(
        "save_note (local-first): uuid={} sync_state={:?} id={}",
        real_uuid, cached.sync_state, if cached.id.is_empty() { "<pending>" } else { &cached.id }
    );

    Ok(gmail::SavedNote {
        id: cached.id,
        // Nothing was pushed on this call (the worker will push in the
        // background) — echo the DB's current remote_version, same as
        // `to_frontend_note` does for `Note::version`.
        version: cached.remote_version.clone().unwrap_or_default(),
        uuid: cached.uuid,
        date: cached.date,
        body_html: cached.body_html,
        local_version: cached.local_version,
    })
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
    refuse_write(&state, &account_id, backend::Write::Notes)?;
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
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
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
    refuse_write(&state, &account_id, backend::Write::Notes)?;
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
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };

    if backend_kind == accounts::BackendKind::Gmail {
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
            v.list_index().await.map_err(|e| e.to_string())
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
    log!("list_cached_notes: {} returned {} cached notes", account_id, cached.len());
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

// ─── Background sync worker ──────────────────────────────────────────────────
//
// Scans the SQLite replica for rows that need to be propagated to Gmail
// (sync_state IN ('dirty', 'deleted_pending')) and tries each one. Loops
// forever with a small interval. Designed to be resilient:
//   - If the network is down, push fails → row stays dirty → retried next cycle
//   - If a token has expired, ensure_token refreshes via the keychain RT
//   - If save_note fails for a permanent reason (e.g. invalid label),
//     we log and move on — the row stays dirty so the user has a chance
//     to fix it. We DON'T silently lose data by marking clean on failure.
//
// Future hardening:
//   - Exponential backoff per uuid on repeated failures
//   - Emit "sync-status" events for the UI to show "1 unsynced" etc.
//   - Coalesce rapid edits to the same uuid (push only the latest version)

/// Milliseconds a content-dirty note must be quiet (no local edit) before the
/// worker pushes it — debounces autosave churn. See
/// docs/superpowers/specs/2026-07-21-debounce-note-push-design.md.
const PUSH_SETTLE_MS: i64 = 5_000;

/// Upper bound on how long an already-synced note may keep deferring while it
/// is edited nonstop; past this it is force-pushed once so other devices/Apple
/// don't go stale. Never-synced notes are exempt (SQLite is the source of
/// truth and survives restart), so they push only via the settle branch.
const MAX_DEFER_MS: i64 = 60_000;

/// Should this content-dirty note be pushed to Gmail on the current tick?
/// `settled` = the user has paused editing for at least `settle_ms`.
/// `overdue` = it has been at least `max_defer_ms` since the last successful
/// push (only meaningful once the note has been synced at all).
fn note_push_due(
    now_ms: i64,
    last_local_modified_at: i64,
    last_synced_at: Option<i64>,
    settle_ms: i64,
    max_defer_ms: i64,
) -> bool {
    let settled = now_ms - last_local_modified_at >= settle_ms;
    let overdue = matches!(last_synced_at, Some(s) if now_ms - s >= max_defer_ms);
    settled || overdue
}

const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// RAII guard for one `(account_id, uuid)` entry in `AppState.pushing`.
///
/// `push_one_dirty` needs to extend the in-flight-push marker to a note's
/// NEW uuid partway through the function, when a create on the Microsoft
/// backend rekeys the row — see the call site below. Two fallible steps
/// (`rekey_note_uuid`, `mark_pushed`) sit between that insert and the point
/// where the entry would naturally come out, and an early `?` on either one
/// would return past a bare `.remove(...)` call, leaking the entry forever.
/// A leaked entry makes `reconcile_one` treat every future remote change to
/// that uuid as "our own in-flight push" and silently ignore it — the
/// self-induced false-conflict race fixed in a693d11, reopened by a partial
/// insert with no matching remove. Tying the removal to `Drop` instead makes
/// every exit path — a normal return, an early `?`, even a panic — release
/// the entry, with no second call site to keep in sync with the first.
struct PushingGuard<'a> {
    pushing: &'a Mutex<std::collections::HashSet<(String, String)>>,
    key: (String, String),
}

impl<'a> PushingGuard<'a> {
    fn new(pushing: &'a Mutex<std::collections::HashSet<(String, String)>>, key: (String, String)) -> Self {
        pushing.lock().unwrap().insert(key.clone());
        Self { pushing, key }
    }
}

impl Drop for PushingGuard<'_> {
    fn drop(&mut self) {
        self.pushing.lock().unwrap().remove(&self.key);
    }
}

#[cfg(test)]
mod pushing_guard_tests {
    use super::*;

    #[test]
    fn releases_on_normal_drop() {
        let pushing: Mutex<std::collections::HashSet<(String, String)>> = Mutex::new(Default::default());
        let key = ("acct".to_string(), "new-uuid".to_string());
        {
            let _guard = PushingGuard::new(&pushing, key.clone());
            assert!(pushing.lock().unwrap().contains(&key), "insert must happen on construction");
        }
        assert!(!pushing.lock().unwrap().contains(&key), "drop must release the entry");
    }

    /// The exact shape of the leak this guard exists to prevent: a fallible
    /// step between insert and the natural removal point returns early via
    /// `?`. Without RAII, that `?` skips a bare `.remove(...)` call and the
    /// entry survives forever — reopening a693d11.
    #[test]
    fn releases_on_early_return_via_question_mark() {
        let pushing: Mutex<std::collections::HashSet<(String, String)>> = Mutex::new(Default::default());
        let key = ("acct".to_string(), "new-uuid".to_string());

        fn fallible_step(
            pushing: &Mutex<std::collections::HashSet<(String, String)>>,
            key: (String, String),
        ) -> Result<(), String> {
            let _guard = PushingGuard::new(pushing, key);
            Err("simulated rekey_note_uuid/mark_pushed failure".to_string())?;
            Ok(())
        }

        let res = fallible_step(&pushing, key.clone());
        assert!(res.is_err(), "the simulated failure must actually propagate");
        assert!(
            !pushing.lock().unwrap().contains(&key),
            "an early ? return must not leak the pushing entry"
        );
    }
}

/// Whether a `TransportError::NotFound` from a content push means "the
/// remote object is genuinely gone" rather than a transient hiccup worth
/// retrying.
///
/// Gated on `SaveSemantics::InPlaceUpdateNeedsExplicitMove`, not just
/// `was_create` — a backend with `RelocatesOnContentPush` semantics (Gmail
/// today) can and does reach a `NotFound` on a content push: Gmail's
/// `classify_str` (`backend/gmail/transport.rs`) substring-matches any HTTP
/// 404 embedded in `save_note_full`'s error string, regardless of why the
/// 404 happened. There, `NotFound` is not evidence the note is gone (the
/// insert failed — the old message is untouched), so dropping the row would
/// destroy an unpushed edit for no reason. On an `InPlaceUpdateNeedsExplicit
/// Move` backend (Microsoft) it IS that evidence, because `save_note_full`
/// PATCHes an existing id in place; a 404 there really is "this id no longer
/// exists." Within that gate, only true for an UPDATE (`was_create ==
/// false`, i.e. there was an existing remote id the PATCH 404'd against) — a
/// CREATE 404ing means the destination FOLDER vanished, a different and much
/// rarer failure this fix does not attempt to characterise.
///
/// Takes `Option<SaveSemantics>` rather than `Option<BackendKind>` — reading
/// this decision straight off `Capabilities` (via `push_one_dirty`'s
/// `save_semantics` local) rather than re-deriving it from a raw
/// `backend_kind == Microsoft` check is the fix itself: a future backend
/// declares its semantics once, in `Capabilities::for_backend`, instead of
/// this function (and the move-dispatch gate below it) each needing their
/// own `== Microsoft` updated by hand. See `push_one_dirty`'s call site for
/// the full reasoning (Microsoft fix 3, whole-branch review 2026-08-15;
/// Gmail gate, pre-merge review 2026-08-15; `Capabilities` routing, altitude
/// fix 8).
fn not_found_means_deleted_remotely(save_semantics: Option<backend::SaveSemantics>, was_create: bool) -> bool {
    !was_create && save_semantics == Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove)
}

/// Whether `push_one_dirty` must issue an explicit `Transport::move_note`
/// after a content push, given the label Jodd last confirmed the remote
/// holds (`remote_label`, `None` meaning "never confirmed") and the label
/// the row currently wants (`current_label`). `None` safely defaults to "no
/// move" rather than firing spuriously — see `push_one_dirty`'s call site
/// for why that gap is bounded and self-healing (Microsoft fix 2,
/// whole-branch review 2026-08-15).
fn note_needs_explicit_move(remote_label: Option<&str>, current_label: &str) -> bool {
    remote_label.is_some_and(|prev| prev != current_label)
}

/// Applies an explicit move's `RemoteNoteVersion`, if any, onto the
/// already-pushed `SavedNote` before `push_one_dirty_db`/`mark_pushed` reads
/// it. Isolated as its own function for the same reason
/// `not_found_means_deleted_remotely`/`note_needs_explicit_move` are:
/// `push_one_dirty` needs a live `tauri::State` and cannot be unit-tested
/// directly, so the actual decision this fix depends on — a move's response,
/// when present, must WIN over the earlier content push's version, not be
/// discarded — is pulled out where a test can exercise it directly.
///
/// `None` (Gmail/LocalFs, always) is a no-op: `saved` already carries the
/// content push's version, which is correct on backends where a move never
/// touches the note's own remote object — see `Transport::move_note`'s doc
/// comment.
fn apply_move_version(saved: &mut backend::SavedNote, moved: Option<backend::RemoteNoteVersion>) {
    if let Some(v) = moved {
        saved.version = v.version;
        saved.date = v.date;
    }
}

#[cfg(test)]
mod push_dispatch_decision_tests {
    use super::*;

    #[test]
    fn a_not_found_on_an_in_place_update_backend_update_means_deleted_remotely() {
        assert!(
            not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove), false
            ),
            "existing_remote_id was Some — an UPDATE, on the semantics where PATCH-404 is trustworthy"
        );
    }

    #[test]
    fn a_not_found_on_an_in_place_update_backend_create_is_not_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove), true
            ),
            "no existing_remote_id — the FOLDER 404'd, not the note"
        );
    }

    /// The critical regression this gate exists to prevent: Gmail's
    /// `classify_str` substring-matches any HTTP 404 onto `NotFound`
    /// regardless of cause, so an UPDATE-shaped 404 on a `RelocatesOnContent
    /// Push` backend (Gmail) must NOT be read as "the note is gone" — that
    /// would drop a dirty row (and its tags/attachments/FTS/edges) for what
    /// is, on Gmail, a harmless retry. This is the test that fails if the
    /// `save_semantics` guard is removed or a future backend is wired to
    /// the wrong variant.
    #[test]
    fn a_not_found_on_a_relocates_on_content_push_update_is_never_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(
                Some(backend::SaveSemantics::RelocatesOnContentPush), false
            ),
            "Gmail's classify_str reaches NotFound via substring match on any 404 — \
             it is never trustworthy evidence the note is gone"
        );
    }

    #[test]
    fn a_not_found_with_no_known_save_semantics_is_not_treated_as_a_remote_deletion() {
        assert!(
            !not_found_means_deleted_remotely(None, false),
            "an unresolved account (no save_semantics) must fail closed, not drop the row"
        );
    }

    /// Confirms the routing this fix depends on: `Capabilities::for_backend`
    /// is the single place a `BackendKind` becomes a `SaveSemantics`, so
    /// `not_found_means_deleted_remotely`/the move-dispatch gate in
    /// `push_one_dirty` never need to know about `BackendKind` at all.
    #[test]
    fn capabilities_routes_each_backend_to_the_matching_save_semantics() {
        use backend::{Capabilities, SaveSemantics};
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::Gmail).save_semantics,
            SaveSemantics::RelocatesOnContentPush
        );
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::LocalFs).save_semantics,
            SaveSemantics::RelocatesOnContentPush
        );
        assert_eq!(
            Capabilities::for_backend(accounts::BackendKind::Microsoft).save_semantics,
            SaveSemantics::InPlaceUpdateNeedsExplicitMove
        );
    }

    #[test]
    fn no_move_is_issued_when_the_label_has_not_changed() {
        assert!(!note_needs_explicit_move(Some("Notes"), "Notes"), "must not move on an ordinary content-only save");
    }

    #[test]
    fn a_move_is_issued_when_the_label_changed() {
        assert!(note_needs_explicit_move(Some("Notes"), "Notes/Ideas"));
    }

    #[test]
    fn an_unconfirmed_remote_label_defaults_to_no_move() {
        // Never yet confirmed (pre-migration row, or a row that has never
        // been pushed or pulled) — the safe default is NOT to move, rather
        // than firing spuriously on every edit. Self-heals via the very
        // next upsert_from_remote or mark_pushed.
        assert!(!note_needs_explicit_move(None, "Notes/Ideas"));
    }

    fn saved_note(version: &str, date: &str) -> backend::SavedNote {
        backend::SavedNote {
            id: "MSG-ID".into(),
            version: version.into(),
            uuid: "note-uuid".into(),
            date: date.into(),
            body_html: "<div>b</div>".into(),
            local_version: 0,
        }
    }

    /// The regression this guards: before this fix, a move's response was
    /// discarded and the content push's (now stale) version/date were
    /// persisted regardless — silently desyncing `notes.remote_version` and
    /// risking a spurious conflict copy on the next poll.
    #[test]
    fn a_move_response_overrides_the_content_pushs_version() {
        let mut saved = saved_note("2026-08-16T10:00:00Z", "Sun, 16 Aug 2026 10:00:00 +0000");
        let moved = Some(backend::RemoteNoteVersion {
            version: "2026-08-16T10:05:00Z".into(),
            date: "Sun, 16 Aug 2026 10:05:00 +0000".into(),
        });

        apply_move_version(&mut saved, moved);

        assert_eq!(saved.version, "2026-08-16T10:05:00Z", "the move's version must win");
        assert_eq!(saved.date, "Sun, 16 Aug 2026 10:05:00 +0000", "the move's date must win");
    }

    /// Gmail/LocalFs always return `None` from `move_note` — must be a
    /// pure no-op, leaving the content push's version/date exactly as they
    /// were (those backends' move never touches the note's own object).
    #[test]
    fn no_move_response_leaves_the_content_pushs_version_untouched() {
        let mut saved = saved_note("2026-08-16T10:00:00Z", "Sun, 16 Aug 2026 10:00:00 +0000");
        let original = saved.clone();

        apply_move_version(&mut saved, None);

        assert_eq!(saved.version, original.version);
        assert_eq!(saved.date, original.date);
    }
}

async fn push_one_dirty(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let existing_gmail_id = if n.id.is_empty() { None } else { Some(n.id.as_str()) };
    // Captured before save_note_full runs: whether THIS push is a create
    // (no remote object exists yet) is exactly "no id going in" — the
    // signal push_one_dirty_db needs to decide whether to persist `id`
    // unconditionally afterward. See push_one_dirty_db's doc comment.
    let was_create = existing_gmail_id.is_none();
    let existing_uuid = Some(n.uuid.as_str());
    let existing_x_mail = n.x_mail_created_date.as_deref();
    // Load this note's stored attachments so save_note can re-emit any the body
    // still references (multipart/related) instead of stripping them.
    let attachments = state
        .db
        .list_attachments(&n.account_id, &n.uuid)
        .unwrap_or_default();
    // `Capabilities::for_backend(_).save_semantics`, not the raw
    // `backend_kind` — see `not_found_means_deleted_remotely`'s doc comment
    // for why this indirection is itself the altitude fix, not decoration.
    let save_semantics = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == n.account_id)
            .map(|a| backend::Capabilities::for_backend(a.backend_kind).save_semantics)
    };
    let v = vertical_for(state, &n.account_id).await?;
    let op = backend::SaveOp {
        title: &n.title, body_html: &n.body_html,
        existing_remote_id: existing_gmail_id, existing_uuid,
        existing_created_date: existing_x_mail, label: &n.label,
    };
    let mut saved = match v.save_note_full(&op, &attachments).await {
        Ok(s) => s,
        // The note exists locally and is dirty, but no longer exists
        // remotely — measured on Microsoft: `classify_status` maps a PATCH's
        // 404 to `NotFound`, which is exactly what an Apple-side hard delete
        // (no undo path — `Capabilities::has_trash` is false on measured
        // evidence) leaves behind for a stale id. Left unhandled the row
        // retries every tick forever: `has_pending_pushes` never clears for
        // this uuid, the account can never leave Draining, and
        // `remove_account` refuses a Draining account (gotcha #2) — the
        // account becomes permanently unremovable.
        //
        // MUST be gated to `InPlaceUpdateNeedsExplicitMove`
        // (`not_found_means_deleted_remotely` takes `save_semantics`, not
        // just `was_create`). `RelocatesOnContentPush` backends (Gmail) are
        // NOT exempt by construction: every save failure routes through
        // `classify_str` (`backend/gmail/transport.rs`), a substring test —
        // `err.contains(" 404")` against `format!("Save failed {}: {}",
        // status, text)` (`backend/gmail/wire.rs`) — so any HTTP 404 from
        // `POST /gmail/v1/users/me/messages` (rate limiting, a malformed
        // body, an account-level restriction — nothing to do with the note
        // being gone) yields `TransportError::NotFound` too, with
        // `was_create == false` for any note that already has an id. Without
        // the gate this arm would silently destroy the user's unpushed
        // edit — tags, attachments, FTS, edges, all of it — on Gmail, the
        // product's primary backend, for what was previously a harmless
        // retry.
        //
        // Only on an UPDATE (an existing remote id that's now gone) — a
        // CREATE 404ing means the destination FOLDER vanished, a different
        // and much rarer failure this fix does not attempt to characterise;
        // it falls through to the generic error path below and keeps
        // retrying, same as before this fix.
        Err(backend::TransportError::NotFound) if not_found_means_deleted_remotely(save_semantics, was_create) => {
            // Drop the row rather than re-create it. Re-creating would
            // silently resurrect the note under a fresh remote identity —
            // undoing an Apple-side delete that has, by design, no undo path
            // to offer (measured: Deleted Items held zero items afterward).
            // The pending local edit is lost, but that mirrors what already
            // happened on every OTHER replica the moment the user deleted
            // the note there; clearing `dirty` without deleting the row
            // would be worse — it would mark a note "synced" that was never
            // actually written anywhere.
            log!(
                "push_one_dirty: uuid={} remote id={} 404'd on push — deleted remotely \
                 while dirty locally; dropping the local row rather than retrying forever \
                 or resurrecting a note the user explicitly deleted",
                n.uuid, n.id
            );
            state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
            return Ok(());
        }
        Err(e) => return Err(e.to_string()),
    };

    // Exchange's PATCH is a real in-place update that never touches
    // `parentFolderId` (spec §B1's third dispatch — `POST /me/messages/
    // {id}/move` — was never wired anywhere before this fix). A label change
    // on this backend therefore needs an EXPLICIT move after the PATCH, or
    // the remote note stays in its old folder forever while the local cache
    // believes it moved (Critical 2, whole-branch review 2026-08-15).
    //
    // Scoped to `InPlaceUpdateNeedsExplicitMove` backends, not dispatched
    // generically for every backend: `RelocatesOnContentPush` backends
    // (Gmail, LocalFs) already relocate the note as an intrinsic part of
    // `save_note_full` (Gmail inserts under the new label and trashes the
    // old message; LocalFs writes into the new folder's directory and
    // removes the old file), so an extra `move_note` call there would be
    // redundant at best — and for Gmail specifically WRONG, since its
    // `Transport::move_note` expects resolved Gmail label ids (see
    // `restore_note`'s `id_of` resolution), not the Jodd PATH this check
    // has on hand.
    //
    // "Previous remote folder" comes from the cached row's `remote_label`
    // (db.rs migration #16) rather than a fetch or a `folder_ids` reverse
    // lookup: a fetch would cost a full mailbox scan on EVERY content edit
    // (`fetch_note`'s own doc comment prices that at "an explicit user
    // action", which a 5-second worker tick is not), and `folder_ids` is
    // Microsoft-internal to the vertical with no path here to read it from.
    // `remote_label` is a plain local-first SQLite read, and it doubles as
    // the "did the label actually change" signal: `None` (never confirmed,
    // e.g. before this migration ships or before the row's first push)
    // safely defaults to "no move" rather than firing on every edit — the
    // gap is bounded to one push and self-heals via the very next
    // `upsert_from_remote` or `mark_pushed`. Only runs on an UPDATE: a
    // CREATE already lands directly in the right folder via `folder_ids`
    // inside `save_note_full`, so there is nothing to move.
    if !was_create && save_semantics == Some(backend::SaveSemantics::InPlaceUpdateNeedsExplicitMove) {
        let remote_label = state.db.get_remote_label(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
        if note_needs_explicit_move(remote_label.as_deref(), &n.label) {
            // The move PATCHes the SAME message the content push above may
            // have just PATCHed — its response, not the content push's, is
            // the authoritative version afterward. See `apply_move_version`.
            let moved = v.move_note(&saved.id, std::slice::from_ref(&n.label), &[])
                .await
                .map_err(|e| e.to_string())?;
            apply_move_version(&mut saved, moved);
        }
    }

    // The backend may assign an identity Jodd cannot choose, and the cache
    // must converge to it. Exchange does this on every create
    // (internetMessageId); Gmail does it too whenever the cached uuid fails
    // to canonicalize and `resolve_uuid_for_save` (gmail/wire.rs) mints a
    // fresh one — so this is backend-agnostic, not Microsoft-only, even
    // though Microsoft's create path is the only place it fires today in
    // practice. Guarding on is_empty keeps LocalFs (which may not fill uuid)
    // on its current path.
    let pushed_uuid = if saved.uuid.is_empty() { n.uuid.clone() } else { saved.uuid.clone() };
    let rekeyed = pushed_uuid != n.uuid;

    // Extend the in-flight guard to the NEW uuid before the row moves — see
    // PushingGuard's doc comment for the race this closes. `_guard` releases
    // on every exit from this scope, including the `?`s inside
    // push_one_dirty_db, so no fallible step between here and mark_pushed
    // can leak the entry.
    let _guard = rekeyed.then(|| {
        PushingGuard::new(&state.pushing, (n.account_id.clone(), pushed_uuid.clone()))
    });

    push_one_dirty_db(&state.db, n, &saved, &pushed_uuid, rekeyed, was_create)
}

/// The DB-only tail of `push_one_dirty` — rekey (if the backend assigned a
/// new identity), persist the remote id unconditionally when this push was a
/// CREATE, then the optimistic-locked `mark_pushed`. Split out the same way
/// `reconcile_one_db` is (see its doc comment): `tauri::State` has no public
/// constructor outside a running Tauri app, so this is what makes the
/// sequence testable at all — including the duplicate-create race a
/// fix-round-1 review on Task 6 caught (`push_one_dirty_db_tests` below).
fn push_one_dirty_db(
    db: &db::Db,
    n: &db::CachedNote,
    saved: &backend::SavedNote,
    pushed_uuid: &str,
    rekeyed: bool,
    was_create: bool,
) -> Result<(), String> {
    if rekeyed {
        db.rekey_note_uuid(&n.uuid, pushed_uuid, &n.account_id).map_err(|e| e.to_string())?;
    }

    if was_create {
        // Unconditional, deliberately NOT gated by mark_pushed's
        // local_version guard below — see `Db::set_remote_id`'s doc comment.
        // Without this, a local edit landing mid-flight makes mark_pushed's
        // guarded UPDATE a no-op, `id` stays empty, and the NEXT tick reads
        // `existing_remote_id: None` again — taking the CREATE branch a
        // second time. On Microsoft that mints a second, genuinely distinct
        // `internetMessageId`: a visible duplicate note with no dedupe to
        // collapse it (`find_ids_for_uuid` is still unimplemented,
        // `DedupSummary` is always empty for this backend).
        db.set_remote_id(pushed_uuid, &n.account_id, &saved.id).map_err(|e| e.to_string())?;
    }

    // No separate re-derive call follows: `rekey_note_uuid` already moves
    // `note_tags`/`tag_tombstones`/`attachments`/`edges` to `pushed_uuid` and
    // re-indexes FTS under it, all inside its own transaction (see that
    // function's doc comment in db.rs). `saved.body_html` is byte-identical
    // to `n.body_html` here (save_note_full always returns the editor-view
    // input unchanged), so the content `rekey_note_uuid` just re-derived FTS
    // from is exactly what `mark_pushed` below writes back — nothing for a
    // second derivation pass to catch that the first one missed.
    db.mark_pushed(
        pushed_uuid,
        &n.account_id,
        &saved.id,
        &saved.version,
        &saved.date,
        &saved.body_html,
        n.local_version,
    ).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod push_one_dirty_db_tests {
    use super::*;

    fn saved_note(id: &str, uuid: &str, version: &str, body_html: &str) -> backend::SavedNote {
        backend::SavedNote {
            id: id.to_string(),
            version: version.to_string(),
            uuid: uuid.to_string(),
            date: "Mon, 14 Aug 2026 10:00:00 +0700".to_string(),
            body_html: body_html.to_string(),
            local_version: 0,
        }
    }

    /// The exact race fix-round-1's review (finding 1) caught, reproduced at
    /// the DB layer `reconcile_one_db` already proved out is testable
    /// without `tauri::State`.
    ///
    /// A note is created locally (dirty, `id=""`, `local_version=1`). The
    /// worker starts pushing it — captures `n.local_version=1` — but before
    /// the push's DB tail runs, a second local edit lands and bumps
    /// `local_version` to 2. The push then succeeds remotely (Exchange
    /// assigns `saved.id`), so `push_one_dirty_db` runs with the STALE
    /// `n.local_version=1` a caller captured before that edit.
    ///
    /// `mark_pushed`'s optimistic lock correctly refuses to match (the row
    /// really has moved past what this push reflects) — the row must stay
    /// `Dirty` with the newer body intact. But `id` must NOT stay empty:
    /// that would make the next tick's `existing_remote_id` read `None`
    /// again and re-CREATE, minting a second Exchange identity for what the
    /// user experiences as one note.
    #[test]
    fn a_concurrent_edit_during_a_create_does_not_leave_id_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let n0 = db::CachedNote {
            uuid: "placeholder-uuid".to_string(),
            account_id: "acc".to_string(),
            id: String::new(),
            title: "Title".to_string(),
            body_html: "<p>v1</p>".to_string(),
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
        };
        db.insert_local_new(&n0).unwrap();
        // The worker's snapshot of the row (n), captured before the push —
        // still local_version 1.
        let n = db.get(&n0.uuid, "acc").unwrap().unwrap();
        assert_eq!(n.local_version, 1);

        // A second edit lands mid-flight: local_version -> 2, still dirty.
        db.apply_local_edit(&n0.uuid, "acc", "Title", "<p>v1</p><p>appended</p>", "Notes").unwrap();

        // The push completes: Exchange assigned a real identity, different
        // from the local placeholder — a create, so `was_create=true` and
        // `rekeyed=true`.
        let saved = saved_note("GRAPH-ID", "real@exchange", "2026-08-14T10:00:00Z", "<p>v1</p>");
        push_one_dirty_db(&db, &n, &saved, "real@exchange", true, true).unwrap();

        let row = db.get("real@exchange", "acc").unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-ID",
            "id must be remembered even though the concurrent edit made mark_pushed's guard miss — \
             otherwise the next tick reads existing_remote_id=None and creates a SECOND note");
        assert_eq!(row.sync_state, db::SyncState::Dirty,
            "mark_pushed's guard must still refuse to clean the row — the newer edit isn't pushed yet");
        assert_eq!(row.body_html, "<p>v1</p><p>appended</p>",
            "the newer local content must survive, not the stale pushed body");
    }

    /// Companion to the race test above: when nothing races the push (the
    /// common case), the DB tail still ends up fully converged — rekeyed,
    /// `id`/`remote_version` stamped, and clean.
    #[test]
    fn an_uncontested_create_ends_up_clean_with_the_real_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let n0 = db::CachedNote {
            uuid: "placeholder-uuid".to_string(),
            account_id: "acc".to_string(),
            id: String::new(),
            title: "Title".to_string(),
            body_html: "<p>v1</p>".to_string(),
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
        };
        db.insert_local_new(&n0).unwrap();
        let n = db.get(&n0.uuid, "acc").unwrap().unwrap();

        let saved = saved_note("GRAPH-ID", "real@exchange", "2026-08-14T10:00:00Z", "<p>v1</p>");
        push_one_dirty_db(&db, &n, &saved, "real@exchange", true, true).unwrap();

        let row = db.get("real@exchange", "acc").unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-ID");
        assert_eq!(row.remote_version, Some("2026-08-14T10:00:00Z".to_string()));
        assert_eq!(row.sync_state, db::SyncState::Clean);
        assert!(db.get(&n0.uuid, "acc").unwrap().is_none(), "the placeholder uuid must be gone after rekey");
    }
}

#[cfg(test)]
mod sidecar_gate_tests {
    use super::*;

    fn dirty_note(uuid: &str, account_id: &str) -> db::CachedNote {
        db::CachedNote {
            uuid: uuid.to_string(),
            account_id: account_id.to_string(),
            id: "remote-id".to_string(),
            title: "Title".to_string(),
            body_html: "<p>#work</p>".to_string(),
            date: "Mon, 14 Aug 2026 09:00:00 +0700".to_string(),
            x_mail_created_date: None,
            label: "Notes".to_string(),
            local_version: 1,
            remote_version: Some("v1".to_string()),
            sync_state: db::SyncState::Clean,
            last_synced_at: None,
            last_local_modified_at: db::now_ms(),
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
        }
    }

    /// Before M4, a Microsoft account's `pin_dirty` row (set via `set_pin`)
    /// would retry `put_sidecar` forever (`Err(milestone_2())`), so this
    /// short-circuit cleared it instead of pushing — otherwise
    /// `has_pending_pushes` would never clear, permanently blocking Draining
    /// -> Inactive and so `remove_account`. M4 turned
    /// `Capabilities::for_backend(Microsoft).writes.sidecars` on (a named
    /// MAPI property on the note itself, not a second message), so
    /// `sidecars_supported` now reports Microsoft as supported and this
    /// short-circuit must NOT fire — the row is correctly left for the real
    /// push (`push_one_pin`, `wire::put_pin`). Mirrors
    /// `a_gmail_pin_dirty_row_is_left_for_the_real_push` below.
    #[test]
    fn a_microsoft_pin_dirty_row_is_now_left_for_the_real_push_too() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u3", "ms-acct-2");
        n.pin_dirty = true;
        n.pinned = true;
        db.insert_local_new(&n).unwrap();
        assert!(db.has_pending_pushes("ms-acct-2").unwrap());

        let handled = clear_pin_if_sidecars_unsupported(&db, Some(accounts::BackendKind::Microsoft), &n)
            .unwrap();

        assert!(!handled, "M4: Microsoft has a sidecar store now — the drain must attempt the real push");
        assert!(db.has_pending_pushes("ms-acct-2").unwrap(), "pin_dirty must stay set for the real push");
        let row = db.get("u3", "ms-acct-2").unwrap().unwrap();
        assert!(row.pin_dirty, "must not have been cleared out from under the real push path");
        assert!(row.pinned);
    }

    /// Negative case: Gmail and LocalFs DO have a sidecar store, so a
    /// pin_dirty row must NOT be short-circuited — it has to stay dirty for
    /// `push_one_pin`'s real vertical call to handle.
    #[test]
    fn a_gmail_pin_dirty_row_is_left_for_the_real_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u2", "gmail-acct");
        n.pin_dirty = true;
        n.pinned = true;
        db.insert_local_new(&n).unwrap();

        let handled = clear_pin_if_sidecars_unsupported(&db, Some(accounts::BackendKind::Gmail), &n)
            .unwrap();

        assert!(!handled, "Gmail has a sidecar store — the drain must still attempt the real push");
        let row = db.get("u2", "gmail-acct").unwrap().unwrap();
        assert!(row.pin_dirty, "must not have been cleared out from under the real push path");
    }

    /// `None` (account not found) reads as supported, matching
    /// `write_refusal_for`'s "defer to the caller's own not-found path"
    /// convention. Never exercised in practice — the sync worker's drain
    /// loops already filter to accounts present in `state.accounts` before
    /// calling this — but pinning the default matters since a silent flip
    /// here would clear a real backend's dirty row.
    #[test]
    fn unknown_account_defers_rather_than_clearing() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let mut n = dirty_note("u4", "ghost-acct");
        n.pin_dirty = true;
        db.insert_local_new(&n).unwrap();

        assert!(!clear_pin_if_sidecars_unsupported(&db, None, &n).unwrap());
    }
}

async fn push_one_deletion(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    // If the note never reached Gmail (no remote_version), just drop the row.
    // Nothing on the server to trash. Sidecars only exist after a push, and
    // a push can only happen after Gmail has a copy of the note, so no
    // sidecar can exist for a never-pushed note either.
    if n.id.is_empty() {
        state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
        return Ok(());
    }
    let v = vertical_for(state, &n.account_id).await?;
    v.delete(&n.id).await.map_err(|e| e.to_string())?;
    // Best-effort trash of any pin sidecar in Notes-Meta. Without this,
    // deleted notes leave an orphan metadata message that accumulates over
    // time. Failure is logged but doesn't fail the deletion — the user's
    // intent ("remove this note") is more important than sidecar hygiene,
    // and the next sync_pin_state pass will notice the orphan (it has no
    // matching note locally) and the user can clean it up via the
    // dup-cleanup flow.
    if let Some(pin_sidecar) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = v.remove_sidecar(pin_sidecar).await {
            log!("push_one_deletion: trash pin sidecar {} failed: {}", pin_sidecar, e);
        }
    }
    state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
    Ok(())
}

/// The DB-only tail of the sidecars-unsupported short-circuit used by
/// `push_one_pin` — no `State`, no vertical, so it's testable without a
/// running Tauri app (same reason `push_one_dirty_db` is split out).
/// Clearing `pin_dirty` here (rather than leaving it to retry `put_sidecar`
/// forever) is the truthful state, not a fudge: a backend with no sidecar
/// store genuinely has nothing pending to push, and the pin stays
/// Jodd-local — already how the compatibility tiers describe it. Returns
/// `true` if it handled the row (caller should return early).
fn clear_pin_if_sidecars_unsupported(
    db: &db::Db,
    kind: Option<accounts::BackendKind>,
    n: &db::CachedNote,
) -> Result<bool, String> {
    if sidecars_supported(kind) {
        return Ok(false);
    }
    db.mark_pin_pushed(&n.uuid, &n.account_id, None, n.pinned, None).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Push one note's pin sidecar to Gmail. Reads the configured meta_label
/// from the account's settings, ensures the label exists (creates it on
/// first push for this account), then either:
///   - pinned=true  → insert a new sidecar message (Subject "___<uuid>"),
///                    trash the previous one if `meta_msg_id` is set
///   - pinned=false → trash the existing sidecar (if any), set
///                    meta_msg_id=NULL on success
/// After the network round-trip, calls mark_pin_pushed which conditionally
/// clears pin_dirty IF the SQLite-side pinned value still equals what we
/// just pushed (a concurrent re-toggle mid-push leaves pin_dirty=1 so the
/// worker re-pushes next tick). Short-circuits first via
/// `clear_pin_if_sidecars_unsupported` when this backend has no sidecar
/// store at all — see that function's doc comment.
///
/// **Threads the sidecar write's `RemoteNoteVersion` through to
/// `mark_pin_pushed`.** On Microsoft the pin lives on the note's own remote
/// object, so the PATCH that (un)pins it also advances that object's
/// `lastModifiedDateTime` — discarding that response used to leave
/// `notes.remote_version` stale, so a fast follow-up local edit would read
/// as a remote conflict that never happened (see `RemoteNoteVersion`'s doc
/// comment). On Gmail/LocalFs the sidecar is a separate object and this is
/// always `None`, so `mark_pin_pushed` leaves `remote_version` untouched —
/// exactly the prior behavior for those backends.
async fn push_one_pin(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == n.account_id).map(|a| a.backend_kind)
    };
    if clear_pin_if_sidecars_unsupported(&state.db, kind, n)? {
        return Ok(());
    }
    let v = vertical_for(state, &n.account_id).await?;

    let mut new_note_version: Option<backend::RemoteNoteVersion> = None;
    let new_meta_id: Option<String> = if n.pinned {
        let payload = serde_json::json!({ "pinned": true }).to_string();
        let (id, version) = v.put_sidecar(&n.uuid, SidecarKind::Pin, Some(payload.as_bytes()), n.meta_msg_id.as_deref()).await.map_err(|e| e.to_string())?;
        new_note_version = version;
        Some(id)
    } else {
        if let Some(old) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Best-effort trash. If the sidecar was already trashed by
            // another Jodd instance we still want to clear meta_msg_id
            // locally — mark_pin_pushed runs regardless.
            match v.remove_sidecar(old).await {
                Ok(version) => new_note_version = version,
                Err(e) => log!("push_one_pin: trash sidecar {} failed: {}", old, e),
            }
        }
        None
    };
    let _ = state.db.mark_pin_pushed(
        &n.uuid,
        &n.account_id,
        new_meta_id.as_deref(),
        n.pinned,
        new_note_version.as_ref().map(|v| (v.version.as_str(), v.date.as_str())),
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// Worker-level backstop for `Capabilities::for_backend(_).writes.folders`,
/// mirroring `clear_pin_if_sidecars_unsupported` (see that function's doc
/// comment for why the execution point needs its own guard rather than
/// trusting every caller to have checked `refuse_write` first).
///
/// Today every path that can insert a `dirty_new`/`dirty_renamed`/
/// `deleted_pending` folder row already calls `refuse_write(Write::Folders)`
/// first, so this should never actually fire — but `push_one_folder` is the
/// point that turns such a row into a real Graph call, and on Microsoft that
/// call is not merely rejected, it *succeeds* (per M3: Graph accepts a
/// `POST /me/mailFolders/{root}/childFolders` and returns 201) while
/// producing a folder Apple's sync will never show, with no error surfaced
/// anywhere. A caller-side gate that gets bypassed by a future command, a
/// migration, or a reordered check would fail silently without this. Drop
/// the row rather than retry: retrying forever would count against
/// `has_pending_pushes` and wedge a Draining account's path to Inactive,
/// same reasoning as the `Permanent`-error handling below. Returns `true`
/// if it handled the row (caller should return early).
fn drop_folder_row_if_writes_unsupported(
    db: &db::Db,
    kind: Option<accounts::BackendKind>,
    f: &db::CachedFolder,
) -> Result<bool, String> {
    if write_refusal_for(kind, backend::Write::Folders).is_none() {
        return Ok(false);
    }
    log!(
        "push_folder: dropping '{}' for account {} — folder writes are not \
         supported on this backend (Capabilities::writes.folders is false)",
        f.path, f.account_id
    );
    db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
    Ok(true)
}

async fn push_one_folder(
    state: &State<'_, AppState>,
    f: &db::CachedFolder,
) -> Result<(), String> {
    use db::FolderSyncState::*;
    let kind = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == f.account_id).map(|a| a.backend_kind)
    };
    if drop_folder_row_if_writes_unsupported(&state.db, kind, f)? {
        return Ok(());
    }
    let v = vertical_for(state, &f.account_id).await?;
    match f.sync_state {
        DirtyNew => {
            // Create on the backend. Returns the new label_id.
            match v.create_folder(&f.path).await {
                Ok(folder) => {
                    state.db.mark_folder_created(&f.account_id, &f.path, &folder.id)
                        .map_err(|e| e.to_string())?;
                    // Invalidate label_map cache so subsequent note saves see
                    // the new label (they look up label_id by path in that map).
                    invalidate_label_cache(state, &f.account_id);
                }
                // `Permanent` means retrying THIS operation will never
                // succeed (see `TransportError`'s doc comment / gmail
                // transport's `classify_str`) — e.g. Microsoft's create_folder
                // refuses outright when the Notes root has no reachable
                // Exchange id (gotcha #12: no note has ever been filed
                // directly in Notes, so there is nothing to derive a parent
                // id from). Retrying forever would leave this row `dirty_new`
                // permanently — `has_pending_pushes` counts it, so a Draining
                // account could never reach Inactive and `remove_account`
                // would refuse it forever. Same wedge the sidecar fix
                // (`clear_pin_if_sidecars_unsupported`) closes one layer up.
                // Drop the row and name the failure loudly instead: the user
                // loses a folder they just created in this rare case, which
                // is strictly better than an account that can never be
                // removed. Mirrors `DirtyRenamed`'s `NotFound` handling below.
                Err(e @ crate::backend::TransportError::Permanent { .. }) => {
                    log!(
                        "push_folder: dropping stale dirty_new '{}' — create refused \
                         permanently: {}",
                        f.path, e
                    );
                    state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                    invalidate_label_cache(state, &f.account_id);
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        DirtyRenamed => {
            // Need label_id. If we don't have one yet, the folder was
            // created locally and the create push hasn't fired yet — skip
            // this tick; we'll be back.
            let Some(label_id) = f.label_id.as_deref() else {
                return Err("rename pending but label_id is None — wait for create push".into());
            };
            // Empty label_id is a stale artifact from pre-fix builds where
            // reconcile_folders_from_paths passed "" as the label_id. There is
            // no valid source path to rename from. Mark clean so the retry loop
            // stops; the next full-sync reconcile re-derives the correct state.
            if label_id.is_empty() {
                log!("push_folder: dirty_renamed '{}' has empty label_id (stale row) — marking clean", f.path);
                state.db.mark_folder_renamed(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                invalidate_label_cache(state, &f.account_id);
                return Ok(());
            }
            match v.rename_folder(label_id, &f.path).await {
                Ok(()) => {}
                Err(crate::backend::TransportError::NotFound) => {
                    // Source gone AND destination absent — the folder was deleted
                    // externally while a rename was pending. Drop the stale row
                    // instead of retrying forever.
                    log!(
                        "push_folder: stale dirty_renamed '{}' (source '{}' gone) — dropping row",
                        f.path, label_id
                    );
                    state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                    invalidate_label_cache(state, &f.account_id);
                    return Ok(());
                }
                Err(e) => return Err(e.to_string()),
            }
            // For LocalFs, note `id` = file path, so renaming the folder
            // changes the path. Update cached IDs so future fetches/saves
            // use the correct path. For Gmail this is a safe no-op (Gmail
            // message IDs don't follow the `Notes/<folder>/…` path pattern).
            if let Err(e) = state.db.rename_note_ids_for_folder(&f.account_id, label_id, &f.path) {
                log!("push_folder: rename_note_ids_for_folder failed: {}", e);
            }
            state.db.mark_folder_renamed(&f.account_id, &f.path)
                .map_err(|e| e.to_string())?;
            invalidate_label_cache(state, &f.account_id);
        }
        DeletedPending => {
            // If no label_id, this folder was created locally and never pushed —
            // the mark_folder_deleted helper already dropped the row in that
            // case, so we shouldn't see it here. Belt-and-suspenders: handle
            // gracefully.
            let Some(label_id) = f.label_id.as_deref() else {
                state.db.drop_folder_row(&f.account_id, &f.path)
                    .map_err(|e| e.to_string())?;
                return Ok(());
            };
            // Empty label_id is as dangerous as None here: delete_folder("") on
            // LocalFs resolves to notes_dir() and would wipe the entire vault.
            // Drop the stale row instead.
            if label_id.is_empty() {
                log!("push_folder: deleted_pending '{}' has empty label_id — dropping stale row", f.path);
                state.db.drop_folder_row(&f.account_id, &f.path).map_err(|e| e.to_string())?;
                invalidate_label_cache(state, &f.account_id);
                return Ok(());
            }
            v.delete_folder(label_id).await.map_err(|e| e.to_string())?;
            state.db.drop_folder_row(&f.account_id, &f.path)
                .map_err(|e| e.to_string())?;
            invalidate_label_cache(state, &f.account_id);
        }
        Clean => {} // shouldn't get here — list_dirty_folders filters Clean
    }
    Ok(())
}

#[cfg(test)]
mod folder_write_guard_tests {
    use super::*;

    /// The regression this guard exists to prevent: a `dirty_new` folder row
    /// for a Microsoft account reaching the worker must never turn into a
    /// real `create_folder` Graph call — M3 proved that call succeeds (201)
    /// while producing a folder Apple's sync will never show, silently.
    #[test]
    fn a_microsoft_dirty_folder_row_is_dropped_not_pushed() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("ms-acct", "Notes/Ideas").unwrap();
        assert!(db.has_pending_pushes("ms-acct").unwrap());
        let f = db.get_folder("ms-acct", "Notes/Ideas").unwrap().unwrap();

        let handled =
            drop_folder_row_if_writes_unsupported(&db, Some(accounts::BackendKind::Microsoft), &f)
                .unwrap();

        assert!(handled, "folder writes are permanently unsupported on Microsoft");
        assert!(db.get_folder("ms-acct", "Notes/Ideas").unwrap().is_none(), "row must be dropped");
        assert!(!db.has_pending_pushes("ms-acct").unwrap(), "must not wedge Draining -> Inactive");
    }

    /// Negative case: Gmail and LocalFs DO support folder writes, so a dirty
    /// row must be left alone for `push_one_folder`'s real vertical call.
    #[test]
    fn a_gmail_dirty_folder_row_is_left_for_the_real_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("gmail-acct", "Notes/Ideas").unwrap();
        let f = db.get_folder("gmail-acct", "Notes/Ideas").unwrap().unwrap();

        let handled =
            drop_folder_row_if_writes_unsupported(&db, Some(accounts::BackendKind::Gmail), &f)
                .unwrap();

        assert!(!handled, "Gmail supports folder writes — the drain must still attempt the real push");
        assert!(db.get_folder("gmail-acct", "Notes/Ideas").unwrap().is_some(), "row must survive");
    }

    /// `None` (account not found) reads as supported, matching
    /// `write_refusal_for`'s "defer to the caller's own not-found path"
    /// convention — same reasoning as the pin guard's equivalent test.
    #[test]
    fn unknown_account_defers_rather_than_dropping() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        db.create_folder_local_new("ghost-acct", "Notes/Ideas").unwrap();
        let f = db.get_folder("ghost-acct", "Notes/Ideas").unwrap().unwrap();

        assert!(!drop_folder_row_if_writes_unsupported(&db, None, &f).unwrap());
    }
}

async fn sync_worker_tick(app: &AppHandle) {
    let state = app.state::<AppState>();

    // Serialize the whole tick body against any other concurrent caller
    // (the periodic loop below, or `flush_sync` firing from a visibility
    // change at the same moment). See `AppState.sync_tick_lock` for why
    // this is load-bearing, not defensive: without it, two overlapping
    // ticks can both read the same dirty row before either has written
    // `mark_pushed`, and both push it — producing an orphaned duplicate
    // Gmail message. Held for the tick's full duration (guard scoped to
    // the function), so a tick that starts while another is in flight
    // waits for it rather than racing it.
    let _tick_guard = state.sync_tick_lock.lock().await;

    // Snapshot live accounts at the top of the tick. Dirty/deletion rows
    // for an account that no longer exists (signed out mid-cycle) would
    // otherwise generate ~5 errors per tick until the next index sweep —
    // and would burn refresh-token lookups against the keychain for
    // accounts the user has explicitly removed. Skip them silently.
    //
    // Accounts the worker will still push for. Inactive accounts are excluded
    // outright; Draining accounts stay IN, because draining is the whole point
    // of that state — they are hidden from the user but still owe the backend
    // whatever was queued before they were dismissed.
    let live_accts: std::collections::HashSet<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.status != accounts::AccountStatus::Inactive)
        .map(|a| a.id.clone())
        .collect();

    // FOLDERS FIRST. Creates must reach Gmail before notes that target the
    // new label try to save (otherwise save_note's label_map lookup fails
    // and the note falls back to "Notes" root). Renames must propagate
    // before list_notes sees a stale name. Deletions go last (deepest
    // first via list_dirty_folders' ordering) so children are cleared
    // before parents.
    let dirty_folders = match state.db.list_dirty_folders() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_dirty_folders failed: {}", e); vec![] }
    };
    for f in dirty_folders {
        if !live_accts.contains(&f.account_id) {
            log!(
                "sync_worker: skipping folder '{}' — account {} no longer exists",
                f.path, f.account_id
            );
            continue;
        }
        if let Err(e) = push_one_folder(&state, &f).await {
            log!(
                "sync_worker: push folder '{}' ({:?}) failed: {}",
                f.path, f.sync_state, e
            );
        } else {
            log!(
                "sync_worker: pushed folder '{}' ({:?})",
                f.path, f.sync_state
            );
        }
    }

    // Drain dirty rows first (creates/edits), then deletions. Order matters
    // a little: pushing edits before deletions means that if the user
    // edits-then-deletes the same uuid in quick succession, the edit's
    // network call still goes (and gets trashed by the delete). Harmless.
    let dirty = match state.db.list_dirty() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_dirty failed: {}", e); vec![] }
    };
    for n in dirty {
        if !live_accts.contains(&n.account_id) {
            log!(
                "sync_worker: skipping dirty uuid={} — account {} no longer exists",
                n.uuid, n.account_id
            );
            continue;
        }
        // Debounce: don't re-push a note that is still being actively edited.
        // Push once it has settled (quiet >= PUSH_SETTLE_MS) or is overdue
        // (synced-note cap). Cuts insert-new/trash-old churn that wedges
        // Apple Notes' per-mailbox sync. See note_push_due.
        if !note_push_due(
            db::now_ms(),
            n.last_local_modified_at,
            n.last_synced_at,
            PUSH_SETTLE_MS,
            MAX_DEFER_MS,
        ) {
            continue;
        }
        // Mark in-flight BEFORE gmail::save_note so any concurrent poll/
        // reconcile sees this push as "ours, don't conflict" — closes the
        // race that caused spurious self-conflicts.
        let key = (n.account_id.clone(), n.uuid.clone());
        state.pushing.lock().unwrap().insert(key.clone());
        let res = push_one_dirty(&state, &n).await;
        state.pushing.lock().unwrap().remove(&key);
        if let Err(e) = res {
            log!("sync_worker: push dirty uuid={} failed: {}", n.uuid, e);
        } else {
            log!("sync_worker: pushed dirty uuid={}", n.uuid);
        }
    }
    let deleted = match state.db.list_deleted_pending() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_deleted_pending failed: {}", e); vec![] }
    };
    for n in deleted {
        if !live_accts.contains(&n.account_id) {
            log!(
                "sync_worker: skipping deleted uuid={} — account {} no longer exists",
                n.uuid, n.account_id
            );
            continue;
        }
        // Same in-flight tracking applies to deletes — a poll during trash
        // would see the message has not yet been trashed (if Apple Notes/
        // Gmail web hasn't refreshed) and could incorrectly re-upsert it.
        // For deletions the row is already in deleted_pending state, which
        // reconcile_one skips anyway, but we mark it for symmetry and
        // future-proofing.
        let key = (n.account_id.clone(), n.uuid.clone());
        state.pushing.lock().unwrap().insert(key.clone());
        let res = push_one_deletion(&state, &n).await;
        state.pushing.lock().unwrap().remove(&key);
        if let Err(e) = res {
            log!("sync_worker: push deletion uuid={} failed: {}", n.uuid, e);
        } else {
            log!("sync_worker: trashed + removed cached row uuid={}", n.uuid);
        }
    }

    // Drain pin sidecars. Independent of content-dirty / deleted_pending:
    // a row can be content-dirty AND pin-dirty in the same tick and both
    // push paths run for it (the sidecar lives in a different label, so
    // there's no Gmail-side ordering constraint). We drain AFTER content
    // and deletes only because pin-sync is the lowest-priority operation
    // (purely UX, not correctness) and starving it briefly is fine if a
    // large content backlog is in flight.
    let dirty_pin = match state.db.list_pin_dirty() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_pin_dirty failed: {}", e); vec![] }
    };
    for n in dirty_pin {
        if !live_accts.contains(&n.account_id) {
            log!(
                "sync_worker: skipping pin-dirty uuid={} — account {} no longer exists",
                n.uuid, n.account_id
            );
            continue;
        }
        if let Err(e) = push_one_pin(&state, &n).await {
            log!("sync_worker: push pin uuid={} failed: {}", n.uuid, e);
        } else {
            log!(
                "sync_worker: pushed pin sidecar uuid={} pinned={}",
                n.uuid, n.pinned
            );
        }
    }

    // A Draining account with every queue empty has finished quiescing. This
    // is the ONLY transition into Inactive, which is what lets the rest of the
    // app treat Inactive as "nothing is pending" rather than a mere label.
    let draining: Vec<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.status == accounts::AccountStatus::Draining)
        .map(|a| a.id.clone())
        .collect();
    for id in draining {
        match state.db.has_pending_pushes(&id) {
            Ok(false) => {
                let mut list = state.accounts.lock().unwrap();
                if let Some(a) = list.iter_mut().find(|a| a.id == id) {
                    // Re-check under the lock: the `draining` snapshot above
                    // is from earlier in the tick, and a reactivate
                    // (Inactive -> Active, Task 5) can land in between.
                    // Flipping unconditionally would clobber the user's
                    // Active back to Inactive.
                    if should_flip_to_inactive(a.status, true) {
                        a.status = accounts::AccountStatus::Inactive;
                        if let Err(e) = accounts::save_accounts(&list) {
                            log!("sync_worker: saving drained status for {} failed: {}", id, e);
                        }
                        log!("sync_worker: {} finished draining — now inactive", id);
                    }
                }
            }
            Ok(true) => {}
            Err(e) => log!("sync_worker: has_pending_pushes({}) failed: {}", id, e),
        }
    }
}

/// Run one sync tick immediately instead of waiting out the worker's sleep.
/// Called by the frontend on visibility transitions: on Android the worker
/// loop stops entirely once the OS freezes a backgrounded process, so without
/// this a note edited just before switching apps would sit dirty. See spec §5.
///
/// Safe to call while the periodic loop's own tick is in flight —
/// `sync_worker_tick` serializes on `AppState.sync_tick_lock`, so this call
/// either runs immediately or waits for the in-progress tick to finish
/// before running its own, never overlapping it.
#[tauri::command]
async fn flush_sync(app: AppHandle) -> Result<(), String> {
    sync_worker_tick(&app).await;
    Ok(())
}

fn spawn_sync_worker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        log!("sync worker: starting, interval={:?}", SYNC_INTERVAL);
        loop {
            tokio::time::sleep(SYNC_INTERVAL).await;
            sync_worker_tick(&app).await;
        }
    });
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
    let rt_to_save = token_data.refresh_token.unwrap_or(rt);
    let _ = accounts::save_refresh_token(&email, &rt_to_save);

    // Persist the account record.
    let mut list = state.accounts.lock().unwrap();
    list.push(Account {
        id: email.clone(),
        email: email.clone(),
        added_at: chrono::Utc::now().to_rfc3339(),
        notes_label: None,
        meta_label: None,
        llm: Default::default(),
        backend_kind: Default::default(), // Gmail
        root_dir: None,
        status: accounts::AccountStatus::Active,
    });
    let _ = accounts::save_accounts(&list);

    // Cache the live access token so the user doesn't see a sign-in screen.
    let mut states = state.account_states.lock().unwrap();
    let entry = states.entry(email).or_default();
    entry.access_token = Some(token_data.access_token);
    entry.token_expires_at = Some(token_deadline_from_expires_in(token_data.expires_in));

    log!("migrate: legacy account migrated successfully");
}

// ─── Lesson extraction ───────────────────────────────────────────────────────

/// Workflow entry point: take a chunk of source text, ship it to the configured
/// LLM provider, render the structured response into a Lessons note, and persist
/// it locally. Returns the new note's UUID. On LLM failure, creates a fallback
/// note containing only the verbatim source so the paste is never lost.
#[tauri::command]
async fn extract_note(
    account_id: String,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    refuse_write(&state, &account_id, backend::Write::Folders)?;
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
    let provider = crate::llm::resolve::resolve_provider_for_account(&account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    // Call LLM. The provider races its I/O against `cancel` and returns
    // ExtractError::Cancelled if the token fires.
    let envelope = match provider.extract(&source_text, cancel).await {
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
            log!("extract_note: provider error {e:?} — creating fallback note");
            let uuid = create_fallback_source_note(&state, &account_id, &source_text)?;
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            return Err(format!(
                "LLM call failed; source preserved in note {uuid}. {e}"
            ));
        }
    };

    // Assemble note body.
    let body_html = crate::llm::markdown::assemble_note_body(&envelope, &source_text);

    // Derive title: override → envelope.title → first H2 from markdown → date.
    let title = title_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| envelope.title.clone())
        .or_else(|| derive_title_from_markdown(&envelope.lessons_markdown))
        .unwrap_or_else(|| format!("Extract — {}", chrono::Local::now().format("%Y-%m-%d")));

    // Ensure the workflow folder exists.
    let folder = state
        .db
        .ensure_workflow_folder(&account_id, WORKFLOW_FOLDER_EXTRACTS)
        .map_err(|e| format!("ensure folder: {e}"))?;

    // Create a brand-new local note. apply_local_edit is UPDATE-only — we have
    // to insert_local_new for a freshly-generated UUID.
    let uuid = crate::mime822::format_apple_uuid(uuid::Uuid::new_v4());
    let now = db::now_ms();
    let new_note = db::CachedNote {
        uuid: uuid.clone(),
        account_id: account_id.clone(),
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
    };
    state
        .db
        .insert_local_new(&new_note)
        .map_err(|e| format!("insert_local_new: {e}"))?;

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
    state.in_flight_extracts.lock().unwrap().remove(&request_id);

    log!(
        "extract_note: created note uuid={uuid} in {folder} with {} body-derived tag(s)",
        envelope.tags.len()
    );
    Ok(uuid)
}

/// Answer a question from the local cache. Read-only: touches no note,
/// folder, edge, or sidecar, and never calls Gmail.
#[tauri::command]
async fn ask_jodd(
    scope: ask::AskScope,
    turns: Vec<crate::llm::provider::ChatTurn>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<ask::AskAnswer, String> {
    log!("ask_jodd: turns={} request_id={}", turns.len(), request_id);

    let cancel = tokio_util::sync::CancellationToken::new();
    state
        .in_flight_asks
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    let provider = crate::llm::resolve::resolve_app_provider().map_err(|e| {
        state.in_flight_asks.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    let result = ask::run::run_ask(&state.db, provider.as_ref(), &scope, &turns, cancel, &hidden_account_ids(&state)).await;

    // Always drop the token: a late cancel then becomes a no-op, which is the
    // correct semantics once the turn has finished.
    state.in_flight_asks.lock().unwrap().remove(&request_id);

    let answer = result.map_err(|e| e.to_string())?;
    log!(
        "ask_jodd: in_scope={} considered={} used={} trimmed={} dropped_citations={}",
        answer.notes_in_scope,
        answer.notes_considered,
        answer.notes_used,
        answer.trimmed,
        answer.dropped_citations
    );
    Ok(answer)
}

#[tauri::command]
fn cancel_ask(request_id: String, state: State<'_, AppState>) {
    if let Some(tok) = state.in_flight_asks.lock().unwrap().remove(&request_id) {
        log!("cancel_ask: cancelling request_id={}", request_id);
        tok.cancel();
    }
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
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Notes)?;
    refuse_write(&state, &account_id, backend::Write::Folders)?;
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
    let provider = crate::llm::resolve::resolve_provider_for_account(&account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    let envelope = match provider.extract(&source_text, cancel).await {
        Ok(env) => env,
        Err(crate::llm::provider::ExtractError::Cancelled) => {
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("append_extract_note: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("append_extract_note: provider error {e:?} — creating fallback note");
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
        let fallback = create_fallback_source_note(&state, &account_id, &source_text);
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        let uuid = fallback?;
        return Err(format!(
            "target note was deleted while extracting; source preserved in note {uuid}"
        ));
    }

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
    uuid: String,
    title: String,
    addition_text: String,
}

#[derive(serde::Serialize)]
struct LinkSuggestionsResponse {
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
    state: State<'_, AppState>,
) -> Result<LinkSuggestionsResponse, String> {
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
    let provider = crate::llm::resolve::resolve_provider_for_account(&account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    let result = crate::llm::autolink::suggest_links(
        provider.as_ref(),
        &state.db,
        &account_id,
        exclude_uuid.as_deref(),
        &new_note_title,
        &new_note_uuid,
        &text,
        cancel,
    )
    .await;

    state.in_flight_extracts.lock().unwrap().remove(&request_id);

    match result {
        Ok(suggestions) => Ok(LinkSuggestionsResponse {
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
                .map(|a| ProposedAppendDto { uuid: a.uuid, title: a.title, addition_text: a.addition_text })
                .collect(),
        }),
        Err(crate::llm::provider::ExtractError::Cancelled) => Err("cancelled".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(serde::Deserialize)]
struct ConfirmedAppend {
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
/// note. Creates a NEW note (does NOT overwrite the original) so the user can
/// compare and delete whichever version they prefer.
#[tauri::command]
async fn re_extract_note(
    account_id: String,
    uuid: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let note = state
        .db
        .get(&uuid, &account_id)
        .map_err(|e| format!("get note: {e}"))?
        .ok_or_else(|| format!("note not found: {uuid}"))?;

    let source = crate::llm::markdown::extract_source(&note.body_html)
        .ok_or_else(|| "note has no Source section to re-extract from".to_string())?;

    extract_note(account_id, source, None, request_id, state).await
}

#[derive(serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    /// First ~500 chars of whatever came back, for diagnosing a Custom spec.
    pub raw_head: String,
}

/// Round-trip a configured provider with a tiny fixed prompt.
///
/// The dominant failure mode for a Custom agent-CLI spec is wrong flags: the
/// CLI waits for interactive input and the user sees "spun for two minutes,
/// then failed" *after* pasting real source text. A short fixed-prompt probe
/// turns each config iteration into seconds. Works for any provider kind,
/// since it goes through the same resolvers the real workflows use.
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
    let provider = match account_id {
        None => crate::llm::resolve::resolve_app_provider().map_err(|e| e.to_string())?,
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
            crate::llm::resolve::resolve_provider_for_account(&account)
                .map_err(|e| e.to_string())?
        }
    };

    let started = std::time::Instant::now();
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = provider
        .extract("Jodd connection test. Reply with one short lesson.", cancel)
        .await;
    let elapsed_ms = started.elapsed().as_millis() as u64;

    Ok(match result {
        Ok(env) => LlmTestResult {
            ok: true,
            elapsed_ms,
            error: None,
            raw_head: env.lessons_markdown.chars().take(500).collect(),
        },
        Err(e) => {
            let raw_head = match &e {
                crate::llm::provider::ExtractError::MalformedEnvelope { raw, .. } => {
                    raw.chars().take(500).collect()
                }
                _ => String::new(),
            };
            LlmTestResult {
                ok: false,
                elapsed_ms,
                error: Some(e.to_string()),
                raw_head,
            }
        }
    })
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
    cfg: crate::accounts::LlmConfig,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Mutate in-memory under the lock, clone for I/O, release lock before
    // touching disk. Local-first doctrine: in-memory state is updated
    // synchronously; the disk write follows.
    let snapshot = {
        let mut list = state.accounts.lock().unwrap();
        let acct = list
            .iter_mut()
            .find(|a| a.id == account_id)
            .ok_or_else(|| format!("account not found: {account_id}"))?;
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
    let cfg = oauth_config::load();
    let client_id = cfg.as_ref().map(|c| c.client_id.clone()).unwrap_or_default();
    let has_secret = oauth_config::load_secret().is_some();
    let credentials_available = !auth::client_id().is_empty() && !auth::client_secret().is_empty();
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

/// `api_key`: Some("") clears the stored key, None leaves it untouched
/// (so the UI can save other fields without re-entering the secret).
#[tauri::command]
fn set_app_llm_config(
    cfg: app_llm_config::AppLlmConfig,
    api_key: Option<String>,
) -> Result<(), String> {
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

/// Walk the rendered markdown looking for the first `## ` heading. Strip the
/// "Lesson N — " prefix if present so the note title reads as the lesson title
/// itself rather than its ordinal label.
fn derive_title_from_markdown(md: &str) -> Option<String> {
    for line in md.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            // The current prompt instructs the LLM to use "## <topic>" headings
            // directly (no "Lesson N — " prefix). Older notes extracted before
            // the prompt broadening (commit f39d656 → ?) used "## Lesson N — <topic>";
            // strip that prefix if we encounter it, but the new prompt makes
            // this branch unreachable for fresh extractions.
            let candidate = if let Some(rest) = stripped.strip_prefix("Lesson ") {
                rest.splitn(2, " — ").nth(1).unwrap_or(rest).trim()
            } else {
                stripped.trim()
            };
            if !candidate.is_empty() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

/// Doctrine compliance: if the LLM call fails, we still owe the user a note
/// preserving the verbatim source paste so they can retry or recover by hand.
fn create_fallback_source_note(
    state: &AppState,
    account_id: &str,
    source: &str,
) -> Result<String, String> {
    let folder = state
        .db
        .ensure_workflow_folder(account_id, WORKFLOW_FOLDER_EXTRACTS)
        .map_err(|e| format!("ensure folder: {e}"))?;
    let body = format!(
        "<p><em>Extraction failed. Source preserved below.</em></p>\n<hr>\n\
         <details open>\n<summary>Source (verbatim)</summary>\n<pre>{}</pre>\n</details>\n",
        crate::llm::markdown::escape_html(source)
    );
    let uuid = crate::mime822::format_apple_uuid(uuid::Uuid::new_v4());
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
    };
    state
        .db
        .insert_local_new(&new_note)
        .map_err(|e| format!("insert_local_new: {e}"))?;
    Ok(uuid)
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

            let accounts_list = accounts::load_accounts();
            log!("loaded {} account(s) from persistence", accounts_list.len());

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
            app.manage(AppState {
                accounts: Mutex::new(accounts_list),
                account_states: Mutex::new(HashMap::new()),
                pending_pkce: Mutex::new(None),
                pending_backend: Mutex::new(accounts::BackendKind::default()),
                db,
                pushing: Mutex::new(std::collections::HashSet::new()),
                dup_stats: Mutex::new(HashMap::new()),
                in_flight_extracts: Mutex::new(HashMap::new()),
                in_flight_asks: Mutex::new(HashMap::new()),
                oauth_cancel: Mutex::new(None),
                sync_tick_lock: tokio::sync::Mutex::new(()),
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
            rename_local_account,
            count_pending_pushes,
            set_account_status,
            list_notes,
            list_notes_in_folder,
            list_cached_notes_in_folder,
            refetch_note,
            get_trashed_note_preview,
            list_cached_notes,
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
            append_extract_note,
            ask_jodd,
            cancel_ask,
            suggest_wiki_links,
            apply_wiki_link_appends,
            cancel_extraction,
            get_llm_settings,
            list_agent_cli_presets,
            test_llm_provider,
            update_llm_settings,
            get_oauth_config,
            save_oauth_config,
            clear_oauth_config,
            get_app_llm_config,
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
            .map(str::to_string)
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
        let known_dynamic_commands = ["list_orphaned_notes", "list_stale_notes"];
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
        reconcile_one_db(&db, acct, &fetched);

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
            backend::Writes { notes: true, folders: false, sidecars: true },
            "notes writable (M2), folders permanently refused — never reach Apple (M3), \
             sidecars writable via a named property on the note itself (M4)"
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
        let src = include_str!("lib.rs");
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
                ["pub async fn ", "async fn ", "pub fn ", "fn "]
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
            ("save_note", Notes), ("delete_note", Notes),
            ("move_notes_batch", Notes), ("delete_notes_batch", Notes),
            ("restore_note", Notes), ("apply_wiki_link_appends", Notes),
            ("create_folder", Folders), ("rename_folder", Folders),
            ("delete_folder", Folders), ("move_folder", Folders),
            ("set_pin", Sidecars), ("set_pin_batch", Sidecars),
            ("add_tag", Sidecars), ("remove_tag", Sidecars),
            ("rename_tag", Sidecars), ("delete_tag", Sidecars),
            // These write a note AND call ensure_workflow_folder, so they need both.
            ("extract_note", Notes), ("extract_note", Folders),
            ("append_extract_note", Notes), ("append_extract_note", Folders),
        ];
        assert_eq!(mapping.len(), 20, "18 commands, two of which need two kinds");

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
        assert_eq!(backend_kind_for_signin(None, false).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(backend_kind_for_signin(Some("gmail"), false).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(
            backend_kind_for_signin(Some("microsoft"), false).unwrap(),
            accounts::BackendKind::Microsoft
        );
        // Never silently fall back — that would sign the user into the wrong
        // provider and persist the account under it.
        assert!(backend_kind_for_signin(Some("local_fs"), false).is_err());
        assert!(backend_kind_for_signin(Some("Microsoft"), false).is_err());
    }

    #[test]
    fn android_refuses_microsoft_rather_than_opening_a_browser_that_never_returns() {
        let err = backend_kind_for_signin(Some("microsoft"), true).unwrap_err();
        assert!(err.contains("Android"), "the message must say where it failed: {err}");
        // Gmail's App Links path is unaffected.
        assert_eq!(backend_kind_for_signin(None, true).unwrap(), accounts::BackendKind::Gmail);
        assert_eq!(backend_kind_for_signin(Some("gmail"), true).unwrap(), accounts::BackendKind::Gmail);
    }

    #[test]
    fn a_missing_client_id_is_refused_by_name_before_any_browser_opens() {
        // `MS_CLIENT_ID` is unset in the test environment, which is exactly the
        // fresh-checkout case: without this check the auth URL ships
        // `client_id=`, Microsoft answers AADSTS900144 in the browser, and the
        // loopback listener hangs for its full timeout with nothing emitted.
        if auth_ms::client_id().trim().is_empty() {
            let err = refuse_missing_client_id(accounts::BackendKind::Microsoft).unwrap_err();
            assert!(err.contains("MS_CLIENT_ID"), "must name the variable to set: {err}");
            assert!(err.contains(".env"), "must say where to set it: {err}");
        }
        // LocalFs has no OAuth flow and must never be gated on a client id.
        assert!(refuse_missing_client_id(accounts::BackendKind::LocalFs).is_ok());
    }
}
