pub mod accounts;
pub mod applog;
pub mod app_llm_config;
pub mod ask;
pub mod auth;
pub mod auth_ms;
pub mod backend;
pub mod db;
pub mod provenance;
pub mod db_crypto;
pub mod folder_label;
pub mod folder_scope;
pub mod mcp_scope;
pub mod privacy;
pub mod agent_memory;
pub mod agent_files;
pub mod curate;
pub mod studio;
pub mod cache_lock;
mod data_since;
pub mod capture;
pub mod sync_schedule;
mod backup;
mod backup_commands;
mod tv_commands;
mod merge;
mod reconcile;
mod note_mutations;
mod folder_commands;
use folder_commands::{invalidate_label_cache, label_cache_lacks};
mod note_commands;
mod tag_commands;
use note_commands::*;
mod android_storage;
mod app_update;
mod auth_commands;
mod account_commands;
mod listing_commands;
mod ai_commands;
mod ingest_commands;
mod ai_settings_commands;
pub(crate) use ai_commands::{
    account_can_create_folders, actor_for, apply_local_edit_with_retry, ask_preflight,
    checked_account_provider, invalidate_ai, open_ask_session, validate_ai_result, AiRequestCleanup,
    AskSessionInfo, ExtractedNoteDto,
};
pub(crate) use ingest_commands::run_ingest_command;
// Reached only through a `crate::`/`super::` path, which `unused_imports`
// does not count as a use — removing either breaks the build.
#[allow(unused_imports)]
pub(crate) use {ai_commands::actor_for_result, ingest_commands::may_transcribe};
pub(crate) use listing_commands::{
    cached_note_db, index_account, list_notes, merge_onto_row_db, note_persistence_db,
    record_backend_block, MovedRowMerge, NotePersistence,
};
// Reached only through `use super::*` (`sync_worker.rs`, the test modules
// below) or a `crate::` path, which `unused_imports` does not count as a use —
// removing any of these breaks the build.
#[allow(unused_imports)]
pub(crate) use listing_commands::{
    append_unpushed_notes, delete_note_db, link_preview_first_line, overlay_cached_row, restore_kind,
    RestoreKind,
};
use account_commands::perform_account_removal;
use auth_commands::{complete_oauth, parse_oauth_callback};
#[cfg(test)]
use auth_commands::{
    backend_kind_for_signin, refuse_empty_client_id, refuse_missing_client_id, resolve_pending_signin,
};
mod ssh_commands;
mod ai_host_commands;
mod agent_workspace_commands;
mod agent_files_commands;
mod ask_history_commands;
mod orphan_commands;
mod curate_commands;
mod settings_commands;
mod studio_commands;
mod capture_commands;
mod sync_worker;
mod transfer;
mod transfer_commands;
mod icloud_diag_commands;
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
pub mod stt_config;
pub mod youtube_key;
pub mod tv_link;
pub mod tv_now;
pub mod link_settings;
#[cfg(test)]
mod test_support;

use crate::backend::gmail::wire as gmail;
use crate::provenance::Actor;
// `normalize_tag` lives in db.rs: iCloud tag derivation normalizes span text with it.
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

/// A frontend decision the user can see but the log cannot: the editor's
/// "Edited on another device" banner. Its reason lived only in the webview
/// console, which a release build does not expose, so five fixes to it were
/// each a guess about which pass raised it (2026-09-30). The caller sends
/// lengths and hashes, never note content. One line, capped, newlines
/// flattened — a line in this file is a record, and a caller must not be able
/// to forge a second one.
#[tauri::command]
fn editor_diagnostic(line: String) {
    let flat: String = line.chars().take(2000).map(|c| if c.is_control() { ' ' } else { c }).collect();
    log!("editor: {}", flat);
}
/// Run `f` against the database on Tokio's blocking pool and wait for it
/// there, never on the main thread. For an `async` command the frontend
/// calls on a timer or while the user types: a synchronous command runs on
/// the main thread, which on macOS also takes keystrokes, so waiting there
/// for `Db`'s lock while the sync worker holds it is typing lag
/// (`main_thread_command_tests`).
pub(crate) async fn db_blocking<T, F>(state: &AppState, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&db::Db) -> Result<T, String> + Send + 'static,
{
    let db = state.db.clone();
    tokio::task::spawn_blocking(move || f(&db))
        .await
        .map_err(|e| format!("database task failed: {e}"))?
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

    /// Share to Jodd: captures waiting for the person to tap Save, Summarize
    /// or Discard. In memory on purpose — see `capture::Queue`.
    pub captures: Mutex<capture::Queue>,
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

/// The `blocked_reason` of an account whose refresh token cannot be read.
///
/// That used to end in removal: the frontend called `remove_account`, which
/// wipes the account's rows, unsent edits included — and a Keychain prompt
/// that was denied or never answered reads exactly like a missing token. Now
/// the account stays, readable and editable here, and this reason is the
/// persistent signal: the Sidebar shows it as a row under the account, and
/// the incremental pull skips blocked accounts.
pub(crate) const SIGNED_OUT_REASON: &str =
    "Signed out. Jodd can't read this account's sign-in; its notes and unsent changes are kept on this device. Sign in again to reconnect.";

/// What `blocked_reason` should become after a refresh-token read, or `None`
/// to leave it alone. Clears only its own reason: iCloud's Advanced Data
/// Protection block belongs to another producer.
fn signed_out_reason_after_read(current: Option<&str>, token_readable: bool) -> Option<Option<String>> {
    let ours = current == Some(SIGNED_OUT_REASON);
    match (token_readable, ours) {
        (false, true) | (true, false) => None,
        (false, false) => Some(Some(SIGNED_OUT_REASON.to_string())),
        (true, true) => Some(None),
    }
}

/// Apply `signed_out_reason_after_read` to the account. Writes (and so
/// reaches the UI through `list_accounts`) only on a change.
fn record_token_readability(state: &State<'_, AppState>, account_id: &str, token_readable: bool) {
    let current = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.blocked_reason.clone());
    let Some(current) = current else { return };
    if let Some(next) = signed_out_reason_after_read(current.as_deref(), token_readable) {
        record_backend_block(state, account_id, next);
    }
}

fn no_refresh_token_error(account_id: &str) -> String {
    // The frontend recognises auth loss by this text (controllers/authLoss.ts).
    format!("no refresh token in keychain for {}", account_id)
}

/// Sets `pending_removal` on the account with this id, if it exists. Pure
/// data mutation, no I/O — so the gentler branch `remove_account` takes for a
/// `Draining` account (queue instead of refuse) is testable without a live
/// `State`. Returns whether the account was found.
/// A queued removal completes later, from the sync worker, which cannot know
/// that the user unticked "Also remove Jodd's key from the server". For a
/// Managed SSH account that choice is written into its key directory, where
/// `setup::retire_managed_key` reads it. Written before the removal is queued,
/// so a failure here leaves nothing queued with the choice lost.
fn record_keep_key_choice(accounts: &[accounts::Account], account_id: &str, revoke_key: Option<bool>) -> Result<(), String> {
    if revoke_key != Some(false) {
        return Ok(());
    }
    let Some(key_dir) = accounts.iter().find(|a| a.id == account_id).and_then(|a| a.ssh_key.as_deref()) else {
        return Ok(());
    };
    match crate::paths::data_base().map(|d| d.join("jodd").join("ssh").join(key_dir)) {
        Some(dir) => backend::ssh::setup::write_keep_key_marker(&dir),
        None => Ok(()),
    }
}

/// Every AI host key directory a saved setting names: each account's own
/// `llm.agent_host` (a `pending_removal` account still has its row) and the
/// app default's. These two are the only places an `AgentHost` is persisted
/// — the frontend holds a just-set-up host only until Save. What the AI host
/// key sweep in `.setup()` must keep.
pub(crate) fn ai_host_keep_set(
    accounts: &[accounts::Account],
    app: Option<&accounts::LlmConfig>,
) -> std::collections::HashSet<String> {
    accounts
        .iter()
        .map(|a| &a.llm)
        .chain(app)
        .filter_map(|llm| llm.agent_host.as_ref())
        .map(|h| h.key_dir.clone())
        .collect()
}

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

/// The accounts a Views read covers (spec 2026-11-13, "All accounts"):
/// `Some(id)` is that account alone, as before; `None` is every **active**
/// account — the complement of `hidden_account_ids`, so a Draining or
/// Inactive account's notes stay out of All accounts exactly as they stay out
/// of the cross-account tag filter. Signed-out and blocked accounts are
/// active: their notes are on this device.
fn accounts_in_scope(accounts: &[accounts::Account], account_id: Option<String>) -> Vec<String> {
    match account_id {
        Some(id) => vec![id],
        None => accounts.iter().filter(|a| a.is_active()).map(|a| a.id.clone()).collect(),
    }
}

/// A Views read over `accounts_in_scope`: the per-account query, once per
/// account, concatenated. Order across accounts does not matter here — the
/// list sorts every source itself (`noteListing.ts`: pinned, then newest).
fn read_views<T>(
    state: &State<'_, AppState>,
    account_id: Option<String>,
    mut read: impl FnMut(&str) -> rusqlite::Result<Vec<T>>,
) -> Result<Vec<T>, String> {
    let ids = accounts_in_scope(&state.accounts.lock().unwrap(), account_id);
    let mut out = Vec::new();
    for id in ids {
        out.extend(read(&id).map_err(|e| e.to_string())?);
    }
    Ok(out)
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
    let (kind, root_dir, ssh_target, status) = {
        let list = state.accounts.lock().unwrap();
        let a = list.iter().find(|a| a.id == account_id)
            .ok_or_else(|| format!("account {} not found", account_id))?;
        (a.backend_kind, a.root_dir.clone(), a.ssh_target.clone(), a.status)
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
            vertical_from_parts(state, account_id, token, label_map)
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
        accounts::BackendKind::Ssh => {
            let root = root_dir.ok_or_else(|| format!("ssh account {account_id} missing root_dir"))?;
            let target = ssh_target.ok_or_else(|| format!("ssh account {account_id} missing ssh_target"))?;
            let (opts, managed) = {
                let list = state.accounts.lock().unwrap();
                let a = list.iter().find(|a| a.id == account_id)
                    .ok_or_else(|| format!("account {account_id} not found"))?;
                let managed = backend::ssh::is_managed(a);
                (backend::ssh::managed_options(a), managed)
            };
            let session = backend::ssh::session_for(account_id, &target, &opts);
            Ok(Box::new(backend::ssh::SshVertical::new(session, target, root, account_id.to_string()).managed(managed)))
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
/// resolving the rest from the account row. For call sites that already hold
/// token + label_map for other use and shouldn't re-fetch — and the Gmail arm
/// of `vertical_for`, so there is one construction site. Returns
/// Box<dyn Vertical> so all dispatch is uniform.
fn vertical_from_parts(
    state: &State<'_, AppState>,
    account_id: &str,
    token: String,
    label_map: std::collections::HashMap<String, String>,
) -> Result<Box<dyn Vertical>, String> {
    let vertical = {
        let list = state.accounts.lock().unwrap();
        let a = list.iter().find(|a| a.id == account_id)
            .ok_or_else(|| format!("account {} not found", account_id))?;
        gmail_vertical(a, token, label_map)
    };
    Ok(Box::new(vertical))
}

/// The one place a `GmailVertical` is built from an account row.
///
/// Its `user_email` is `Account::email`, the bare address — it becomes the
/// note's `From:` header. Until 2026-09-30 both callers passed the account id
/// (`gmail:user@…`, minted by `account_id_for`), and every note Jodd pushed
/// carried `From: gmail:user@…`. Taking the row, not two strings, is what
/// stops the two from being swapped again.
fn gmail_vertical(
    account: &Account,
    token: String,
    label_map: std::collections::HashMap<String, String>,
) -> backend::gmail::GmailVertical {
    backend::gmail::GmailVertical::new(
        token,
        label_map,
        account.email.clone(),
        account.effective_meta_label().to_string(),
    )
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
    //
    // Already found unreadable this process: answer from the latch, without
    // touching the credential store (see `AccountState::refresh_token_unreadable`).
    let latched = state
        .account_states
        .lock()
        .unwrap()
        .get(account_id)
        .is_some_and(|s| s.refresh_token_unreadable);
    if latched {
        return Err(no_refresh_token_error(account_id));
    }
    let Some(rt) = accounts::load_refresh_token(account_id) else {
        log!("ensure_token: {} refresh token unreadable — marking signed out, keeping its notes", account_id);
        state
            .account_states
            .lock()
            .unwrap()
            .entry(account_id.to_string())
            .or_default()
            .refresh_token_unreadable = true;
        record_token_readability(state, account_id, false);
        return Err(no_refresh_token_error(account_id));
    };
    // Readable again (e.g. the Keychain prompt was allowed after a restart):
    // take the signed-out row down.
    record_token_readability(state, account_id, true);
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
        // Not fatal — this access token is good and the stored token usually
        // still refreshes — but a provider that retires the old one signs the
        // account out later, and without this line nothing says why.
        if let Err(e) = accounts::save_refresh_token(account_id, &new_rt) {
            log!("ensure_token: {} could not store the rotated refresh token: {}", account_id, e);
        }
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
        match db.prune_clean_folders(account_id, &remote_folder_paths, db::prune_horizon_ms()) {
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
        match db.prune_clean_folders(account_id, &keep, db::prune_horizon_ms()) {
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
        match db.prune_clean_folders(account_id, &keep, db::prune_horizon_ms()) {
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
        // A directory listing, same as LocalFs: `list_folders` returns
        // directory paths under the vault root, and a path IS the folder id.
        accounts::BackendKind::Ssh => FolderSourceKind::Paths,
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

/// Apply one pin-sidecar listing to the cache: pin every row a sidecar names,
/// then unpin every clean pinned row no sidecar names. Returns
/// `(applied, cleared)`. Shared by `list_notes` and `sync_pin_state` so the
/// two pulls cannot disagree.
///
/// **A sidecar whose note has no row here is left alone — never removed.**
/// Until 2026-11-12 `sync_pin_state` trashed it as an "orphan", but a missing
/// row only means THIS device has not pulled the note yet: cold start runs
/// this after the index pass and before the folder sweep, and a freshly
/// added account has no rows at all. Trashing the sidecar unpinned the note
/// on every device (measured on Android: note 80D6C8E1's Notes-Meta message
/// was in Trash, and the next pull everywhere cleared the pin). A row that
/// is `pin_dirty` also touches nothing here, and its sidecar is the one the
/// worker is about to replace. Deleting a note already trashes its sidecar
/// (`push_one_deletion`), so nothing needs this sweep for hygiene. SSH made
/// the same call for the same reason (I-6(b)); now every backend does.
///
/// The listing must be complete: a partial one would unpin every note it
/// missed (gotcha #43). `list_sidecars` returns `Err` rather than a short
/// list, and callers skip this on `Err`/`None`.
fn apply_pin_sidecars_db(
    db: &db::Db,
    account_id: &str,
    sidecars: &[backend::SidecarRecord],
) -> (usize, usize) {
    apply_pin_sidecars_at(db, account_id, sidecars, db::prune_horizon_ms())
}

/// `apply_pin_sidecars_db` with the horizon passed in, so a test can step past it.
fn apply_pin_sidecars_at(
    db: &db::Db,
    account_id: &str,
    sidecars: &[backend::SidecarRecord],
    horizon_ms: i64,
) -> (usize, usize) {
    let mut applied = 0usize;
    let mut keep: Vec<String> = Vec::with_capacity(sidecars.len());
    for s in sidecars {
        // Existence == pinned (see the SIDECAR doc in gmail/wire.rs).
        applied += db.apply_remote_pin(&s.note_uuid, account_id, true, &s.id, horizon_ms).unwrap_or(0);
        keep.push(s.note_uuid.clone());
    }
    let cleared = match db.clear_pins_not_in(account_id, &keep, horizon_ms) {
        Ok(n) => n,
        Err(e) => {
            log!("apply_pin_sidecars: clear_pins_not_in failed for {}: {}", account_id, e);
            0
        }
    };
    (applied, cleared)
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
        | accounts::BackendKind::ICloud
        // No insert-new/trash-old dance on this backend either — a save
        // relocates by writing into the new directory and removing the old
        // file, so there are no transient duplicate messages to clean up.
        | accounts::BackendKind::Ssh => false,
    }
}

#[cfg(test)]
mod accounts_in_scope_tests {
    use super::*;

    fn acct(id: &str, status: accounts::AccountStatus, blocked: Option<&str>) -> accounts::Account {
        accounts::Account {
            id: id.into(),
            email: id.into(),
            added_at: String::new(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: accounts::BackendKind::Gmail,
            root_dir: None,
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
            icloud_session_established: false,
            blocked_reason: blocked.map(Into::into),
            sync_cursor: None,
            icloud_replica_id: None,
            status,
            pending_removal: false,
        }
    }

    #[test]
    fn one_account_is_that_account_alone() {
        let list = vec![acct("gmail:a", accounts::AccountStatus::Active, None)];
        assert_eq!(accounts_in_scope(&list, Some("gmail:a".into())), vec!["gmail:a".to_string()]);
    }

    /// All accounts = every active account, signed out included (its notes
    /// are local); Draining and Inactive stay out, as `hidden_account_ids`
    /// keeps them out of the cross-account tag filter.
    #[test]
    fn all_accounts_is_every_active_account() {
        let list = vec![
            acct("gmail:a", accounts::AccountStatus::Active, None),
            acct("microsoft:b", accounts::AccountStatus::Active, Some("Signed out. x")),
            acct("icloud:c", accounts::AccountStatus::Draining, None),
            acct("local_fs:d", accounts::AccountStatus::Inactive, None),
        ];
        assert_eq!(
            accounts_in_scope(&list, None),
            vec!["gmail:a".to_string(), "microsoft:b".to_string()],
        );
    }
}

#[cfg(test)]
mod ssh_backend_kind_tests {
    use super::*;

    #[test]
    fn ssh_routes_like_a_path_backend_with_no_oauth_and_no_detector() {
        let k = accounts::BackendKind::Ssh;
        assert_eq!(folder_source_kind(k), FolderSourceKind::Paths);
        assert!(matches!(restore_kind(k), Some(RestoreKind::UntrashToEncodedPath)));
        assert!(!orphan_cleanup_supported(k));
        assert!(refuse_missing_client_id(k).is_ok());
        assert!(!crate::sync_worker::incremental_pull_supported(k));
        assert_eq!(db::remote_pin_policy(k), db::RemotePin::LocalWins);
    }
}

#[cfg(test)]
mod pin_sidecar_pull_tests {
    use super::*;

    const ACC: &str = "gmail:someone@example.com";
    const PINNED: &str = "80D6C8E1-C49F-42F6-9859-540845D6359C";

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    fn pulled(db: &db::Db, uuid: &str) {
        let n: gmail::Note = serde_json::from_value(serde_json::json!({
            "id": format!("msg-{uuid}"), "uuid": uuid, "title": "t", "body_html": "<div>t</div>",
            "date": "Thu, 12 Nov 2026 19:46:26 +0700", "version": "v1", "label": "Notes",
        }))
        .unwrap();
        db.upsert_from_remote(&db::CachedNote::from_remote(ACC, &n), db::RemotePin::LocalWins).unwrap();
    }

    fn sidecar(uuid: &str) -> backend::SidecarRecord {
        backend::SidecarRecord {
            id: format!("meta-{uuid}"),
            note_uuid: uuid.to_string(),
            kind: backend::SidecarKind::Pin,
            body: None,
        }
    }

    fn pinned(db: &db::Db, uuid: &str) -> bool {
        db.get(uuid, ACC).unwrap().unwrap().pinned
    }

    /// The Android report (2026-11-12): a cold start ran the pin pull before
    /// the note's row existed here, `sync_pin_state` called the sidecar an
    /// orphan and trashed it, and the pin vanished everywhere. The helper has
    /// no way to remove a sidecar at all now; what it must still do is let
    /// the pin arrive once the note is pulled.
    #[test]
    fn a_pin_for_a_note_not_pulled_yet_lands_when_the_note_does() {
        let db = temp_db();
        let listing = [sidecar(PINNED)];

        assert_eq!(apply_pin_sidecars_db(&db, ACC, &listing), (0, 0), "no row yet — nothing applied");

        pulled(&db, PINNED);
        let (applied, _) = apply_pin_sidecars_db(&db, ACC, &listing);
        assert_eq!(applied, 1);
        assert!(pinned(&db, PINNED), "the same sidecar pins the note once it is here");
    }

    /// The other half: a clean pinned row whose sidecar is gone is unpinned,
    /// and a pin_dirty row (the user's pending toggle) is left alone.
    #[test]
    fn absence_unpins_clean_rows_but_not_a_pending_toggle() {
        let db = temp_db();
        pulled(&db, "A");
        pulled(&db, "B");
        apply_pin_sidecars_db(&db, ACC, &[sidecar("A")]);
        assert!(pinned(&db, "A"));

        db.set_pin("B", ACC, true).unwrap(); // pin_dirty, not pushed yet
        let (_, cleared) = apply_pin_sidecars_db(&db, ACC, &[]);
        assert_eq!(cleared, 1);
        assert!(!pinned(&db, "A"), "sidecar gone → unpinned");
        assert!(pinned(&db, "B"), "a pending local pin is not cleared by a listing that predates it");
    }

    /// "The pin doesn't stay" (2026-11-12): a pull reads the sidecars, the
    /// user pins, the worker pushes and `mark_pin_pushed` clears pin_dirty,
    /// THEN the pull applies its listing — which has no sidecar for the note
    /// and used to unpin it. Gmail's list lagging a just-inserted sidecar
    /// does the same. Inside the horizon the local pin wins; past it the
    /// listing is authoritative again.
    #[test]
    fn a_listing_read_before_the_pin_push_does_not_unpin_it() {
        let db = temp_db();
        pulled(&db, PINNED);
        db.set_pin(PINNED, ACC, true).unwrap();
        db.mark_pin_pushed(PINNED, ACC, Some("meta-new"), true, None).unwrap();
        assert!(!db.get(PINNED, ACC).unwrap().unwrap().pin_dirty, "the push landed");

        let stale: [backend::SidecarRecord; 0] = [];
        assert_eq!(apply_pin_sidecars_db(&db, ACC, &stale), (0, 0));
        assert!(pinned(&db, PINNED), "a listing older than the push must not unpin");

        let past = db::now_ms() + 1;
        assert_eq!(apply_pin_sidecars_at(&db, ACC, &stale, past), (0, 1));
        assert!(!pinned(&db, PINNED), "past the horizon a missing sidecar unpins again");
    }

    /// The same race for an unpin: the trash lands after the listing was
    /// read, and the listing's sidecar used to re-pin the note.
    #[test]
    fn a_listing_read_before_the_unpin_push_does_not_repin_it() {
        let db = temp_db();
        pulled(&db, PINNED);
        let listed = [sidecar(PINNED)];
        apply_pin_sidecars_db(&db, ACC, &listed);
        assert!(pinned(&db, PINNED));

        db.set_pin(PINNED, ACC, false).unwrap();
        db.mark_pin_pushed(PINNED, ACC, None, false, None).unwrap();

        assert_eq!(apply_pin_sidecars_db(&db, ACC, &listed).0, 0);
        assert!(!pinned(&db, PINNED), "a listing older than the unpin must not re-pin");

        let past = db::now_ms() + 1;
        assert_eq!(apply_pin_sidecars_at(&db, ACC, &listed, past).0, 1);
        assert!(pinned(&db, PINNED), "past the horizon a live sidecar pins again");
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
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
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

    /// Gotcha #36. Tauri builds every config window whose `create` is true
    /// BEFORE it calls this hook (tauri 2.11 `app.rs` `setup()`), and the
    /// page's first load asks WebKit to launch its WebContent/GPU/Networking
    /// processes — a handshake whose UI half runs on the main thread. This
    /// hook blocks the main thread on the keychain read inside `Db::open`
    /// for as long as the macOS prompt sits unanswered. Over hours those
    /// half-launched processes die, and the launch fails when the hook
    /// returns ("WebContent process took
    /// 13527 seconds to launch … web process failed to launch"), WebKit's
    /// automatic reload finds "no current item to reload", and the window
    /// stays white for the life of the process.
    #[test]
    fn no_config_window_is_created_before_the_setup_hook_runs() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json parses");
        let windows = conf["app"]["windows"].as_array().expect("app.windows is an array");
        assert!(!windows.is_empty(), "the main window must still be declared in tauri.conf.json");
        for w in windows {
            assert_eq!(
                w["create"],
                serde_json::Value::Bool(false),
                "window {:?} has `create` unset or true, so Tauri builds it before .setup() — \
                 a page load started while setup blocks on the keychain can never recover",
                w["label"]
            );
        }
    }

    /// The policy that contains note HTML. Each directive here is load-bearing
    /// and none is covered by `default-src`'s fallback the way it looks.
    #[test]
    fn the_csp_contains_note_html() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json parses");
        let csp = conf["app"]["security"]["csp"].as_str().expect("a CSP is set");
        let directive = |name: &str| {
            csp.split(';')
                .map(str::trim)
                .find(|d| d.split_whitespace().next() == Some(name))
                .unwrap_or_else(|| panic!("CSP has no {name} directive: {csp}"))
                .to_string()
        };
        // No inline script, no remote script.
        assert_eq!(directive("script-src"), "script-src 'self'");
        // No web images: a note written elsewhere must not be able to report
        // that it was opened. Attachments render from data: and blob:.
        let img = directive("img-src");
        assert!(!img.contains("http"), "img-src must not allow web images: {img}");
        // Not covered by default-src at all.
        assert_eq!(directive("base-uri"), "base-uri 'none'");
        assert_eq!(directive("form-action"), "form-action 'none'");
    }

    #[test]
    fn the_main_window_stays_on_the_app() {
        let may = |u: &str| crate::main_window_may_navigate_to(&tauri::Url::parse(u).unwrap());
        // The app's own page on every platform, and the dev server.
        for own in [
            "tauri://localhost/",
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/assets/index.js",
            "http://localhost:1420/",
            "http://ipc.localhost/plugin",
            "about:blank",
        ] {
            assert!(may(own), "{own} is the app and must load");
        }
        // Where a note's markup could try to send the window.
        for away in [
            "https://example.com/",
            "http://example.com/login",
            "https://accounts.google.com.example.net/",
            "https://localhost.example.com/",
            "https://tauri.localhost.example.com/",
        ] {
            assert!(!may(away), "{away} must be refused");
        }
    }

    #[test]
    fn setup_builds_the_main_window_after_the_cache_opens_and_state_exists() {
        let code = setup_hook_code();
        let build_at = code
            .find("WebviewWindowBuilder::from_config(")
            .expect("setup must build the main window itself (the config window is `create: false`)");
        let open_at = code.find("db::Db::open(").expect("setup opens the cache");
        let manage_at = code.find("app.manage(AppState").expect("setup manages AppState");
        let worker_at = code
            .find("spawn_sync_worker(")
            .expect("setup spawns the sync worker");
        assert!(
            build_at > open_at,
            "the main window must be built after db::Db::open, whose keychain read can block \
             the main thread for hours — a page load started before it never recovers"
        );
        assert!(
            build_at > manage_at,
            "the main window must be built after AppState is managed, so no IPC from the page \
             can ever reach a command before its state exists"
        );
        assert!(
            build_at < worker_at,
            "the main window must exist before the sync worker starts: iCloud paths the worker \
             reaches look the \"main\" window up"
        );
    }

    /// The search map's check reads every row of the search index — 40–50 ms
    /// on 3,000 notes on a Mac (debug, 2026-11-18), more on a phone. Inside
    /// `Db::open` that is launch time before the window exists (gotcha #36),
    /// spent on a check that almost never finds anything. It runs on the
    /// blocking pool once the window is up, and not at all while the cache
    /// is locked, when `db` is a throwaway.
    #[test]
    fn the_search_map_check_runs_after_launch_not_while_opening() {
        let db_src = include_str!("db.rs");
        for sig in ["fn open(", "fn open_unencrypted("] {
            let body = crate::test_support::extract_fn_body(db_src, sig);
            assert!(!body.contains("fts_repair_rowid_map"), "Db::{sig} must not check the search map");
        }
        let code = setup_hook_code();
        let build_at = code.find("WebviewWindowBuilder::from_config(").expect("setup builds the window");
        let locked_return_at = code.find("cache locked: legacy keychain migration").expect("the cache-lock return");
        let repair_at = code
            .find("spawn_blocking(move || db.fts_repair_rowid_map_logged())")
            .expect("setup checks the search map on the blocking pool");
        assert!(repair_at > build_at, "the search map check must wait for the window");
        assert!(repair_at > locked_return_at, "a locked cache's throwaway db needs no check");
    }

    /// 2026-10-26: the generic open failure ran the whole app beside the real
    /// accounts.json and keychain, and a Remove click deleted a live
    /// account's refresh token. The arm must now lock the process, and the
    /// lock must be in force before anything reads accounts.json.
    #[test]
    fn a_failed_cache_open_locks_before_accounts_are_read() {
        let code = setup_hook_code();
        let arm = code.find("Err(e) =>").expect("the generic DbOpenError arm exists");
        let engage = code.find("cache_lock::engage(").expect("the generic arm must lock the app");
        let load = code.find("accounts::load_accounts()").expect("setup loads accounts");
        assert!(engage > arm, "the lock is engaged in the generic open-failure arm");
        assert!(engage < load, "the lock must be engaged before accounts.json is read");
        let guard = code.find("cache_lock::engaged()").expect("setup checks the lock");
        assert!(
            guard < load,
            "accounts.json must be read only after checking the lock — a locked process \
             shows no accounts at all"
        );
    }

    #[test]
    fn a_locked_launch_starts_neither_the_sync_worker_nor_the_keychain_migration() {
        let code = setup_hook_code();
        let migrate = code.find("migrate_legacy_keychain(").expect("setup migrates legacy keychain");
        let worker = code.find("spawn_sync_worker(").expect("setup spawns the worker");
        let live = code.find("spawn_live_sync(").expect("setup starts the Claude Code bridge's live sync");
        let early = code
            .find("if cache_locked.is_some() {\n                log!(\"cache locked: legacy")
            .expect("setup returns early while locked, before the keychain migration and worker");
        assert!(early < migrate && early < worker && early < live);
        assert!(code[early..migrate].contains("return Ok(());"));
    }

    #[test]
    fn a_locked_launch_never_sweeps_ssh_keys_against_its_empty_account_list() {
        let code = setup_hook_code();
        let sweep = code.find("sweep_orphan_key_dirs(").expect("setup sweeps ssh keys");
        let skip = code
            .find("skipping ssh key sweep: cache locked")
            .expect("the sweep is skipped while locked");
        assert!(skip < sweep);
    }

    /// The AI host sweep deletes keys that working hosts use; it runs only
    /// behind the same guards as the ssh sweep, plus an app config that read.
    #[test]
    fn the_ai_host_key_sweep_is_skipped_on_any_doubt() {
        let code = setup_hook_code();
        let sweep = code.find("sweep_orphan_dirs(").expect("setup sweeps AI host keys");
        for skip in [
            "skipping AI host key sweep: cache locked",
            "skipping AI host key sweep: accounts.json could not be read",
            "skipping AI host key sweep: the app's AI settings could not be read",
        ] {
            assert!(code.find(skip).unwrap_or_else(|| panic!("missing guard: {skip}")) < sweep, "{skip}");
        }
    }

    #[test]
    fn every_command_passes_the_cache_lock_gate() {
        let src = include_str!("lib.rs");
        // Split so this test's own text is not a match for `ipc_contract`,
        // which parses the FIRST `generate_handler` + `![` in this file.
        assert!(
            src.contains(concat!(".invoke_handler(cache_lock::gate(", "tauri::generate_handler", "![")),
            "generate_handler! must be wrapped in cache_lock::gate, or a locked process runs \
             every command beside a throwaway cache"
        );
    }
}



// ─── Legacy migration ────────────────────────────────────────────────────────

/// Whether the main window may load `url`.
///
/// The main window only ever shows the app itself: links open in the system
/// browser, OAuth runs there too, and iCloud sign-in and the private reader
/// have windows of their own. So a navigation to a web page is never Jodd's
/// doing — it is markup in a note.
///
/// Refuses exactly one thing: an `http`/`https` URL whose host is not the
/// app's own. Everything else is allowed on purpose — `tauri://localhost`
/// (macOS, Linux), `http(s)://tauri.localhost` (Windows, Android),
/// `http://localhost:1420` (the dev server), `about:blank` — because a guard
/// that wrongly refuses the app's own page is a white window on every launch,
/// on a platform this was not run on.
fn main_window_may_navigate_to(url: &tauri::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    match url.host_str() {
        Some(host) => {
            host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host.ends_with(".localhost")
        }
        None => true,
    }
}

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
    if let Err(e) = accounts::save_refresh_token(&account_id, &rt_to_save) {
        log!("migrate: could not store the refresh token for {}: {}", account_id, e);
    }

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
        ssh_target: None,
        ssh_port: None,
        ssh_key: None,
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

    // Development only. A release binary carries its credentials from
    // `build.rs`; reading `.env` there meant a path on the BUILD machine and
    // whatever directory the app happened to be started from.
    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".env");
    #[cfg(debug_assertions)]
    {
        dotenv::from_path(&env_path).ok();
        dotenv::dotenv().ok();
    }

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

    let builder = tauri::Builder::default();
    // Share to Jodd: FIRST plugin, as tauri-plugin-single-instance requires. A
    // second launch (Windows opens a new process per `jodd://` link) hands its
    // argv to this process and exits; the `deep-link` feature turns that into
    // an `on_open_url` event, so the capture arrives through the same
    // dispatcher as every other platform. The callback only raises the window.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        capture_commands::focus_main_window(app);
    }));
    // In-app updates (spec 2026-11-11-app-updater-design.md §5.2). Driven only
    // from app_update::desktop; capabilities grant the webview no `updater:`.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
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
            // A private reader mid-read is hidden too; it must not hold the app open.
            for (label, w) in app.webview_windows() {
                if label.starts_with(crate::ingest::render::READER_LABEL_PREFIX) {
                    let _ = w.destroy();
                }
            }
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(android_storage::init())
        .plugin(app_update::init())
        // One line per page load. A window that stays blank logs NO line after
        // setup returns, which is the only trace that failure leaves in this
        // file (gotcha #36). Origin only: the iCloud sign-in webviews pass
        // through here too, and their full URLs can carry query parameters.
        .on_page_load(|webview, payload| {
            let url = payload.url();
            log!(
                "page load {:?}: '{}' {}://{}",
                payload.event(),
                webview.label(),
                url.scheme(),
                url.host_str().unwrap_or("")
            );
        })
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
            // `cache_dir()` for the same reason: on desktop it is exactly
            // `dirs::cache_dir()`. On Android it is the EXTERNAL cache
            // (Tauri calls getExternalCacheDir), not activity.cacheDir.
            let cache_base = app
                .path()
                .cache_dir()
                .map_err(|e| format!("cache_dir: {e}"))?;
            paths::init(config_base, data_base, cache_base);
            // Before anything below can write a settings file, so a fresh
            // folder is recognised as fresh.
            data_since::ensure();
            // Media downloaded by URL ingest lives under the cache dir; a crash
            // or kill mid-download leaves it behind.
            paths::sweep_ingest_temp();

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
            // Reproduction hook for gotcha #36, debug builds only: stands in for
            // an unanswered keychain prompt by blocking right where `Db::open`
            // would. `echo 90 > <data dir>/jodd/probe_setup_delay` arms it
            // for every launch until the file is removed.
            #[cfg(debug_assertions)]
            if let Ok(s) = std::fs::read_to_string(data_dir.join("probe_setup_delay")) {
                let secs: u64 = s.trim().parse().unwrap_or(30);
                log!("probe: blocking setup for {secs}s BEFORE the cache opens");
                std::thread::sleep(std::time::Duration::from_secs(secs));
                log!("probe: setup unblocked");
            }
            let db = match db::Db::open(&data_dir) {
                Ok(d) => {
                    log!("local cache opened at {}", data_dir.display());
                    // Saved Ask Jodd answers from before the 40-character title cut.
                    match ask::save::shorten_saved_titles(&d) {
                        Ok(0) => {}
                        Ok(n) => log!("shortened {n} saved Ask Jodd title(s)"),
                        Err(e) => log!("shortening saved Ask Jodd titles failed: {e}"),
                    }
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
                        // LOCK, not "carry on with a temp dir". The temp-dir
                        // cache below only satisfies `AppState.db`'s type:
                        // with the lock engaged nothing reads accounts.json,
                        // no command but Try again runs, and the window shows
                        // the unlock screen. Before 2026-10-26 this arm ran
                        // the full app beside the real accounts.json and
                        // keychain, and a Remove click on an account showing
                        // "0 notes" deleted its refresh token — see
                        // `cache_lock`'s module doc. Fail closed: an error
                        // with no lock of its own still locks.
                        log!("FATAL: failed to open local cache: {} — locking the app", e);
                        cache_lock::engage(cache_lock::CacheLock::for_open_error(&e).unwrap_or(
                            cache_lock::CacheLock {
                                kind: cache_lock::LockKind::OpenFailed,
                                detail: e.to_string(),
                            },
                        ));
                        let tmp = std::env::temp_dir().join("jodd");
                        // An error, not `expect`: release builds abort on
                        // panic, which would hide both failures.
                        Arc::new(db::Db::open_unencrypted(&tmp).map_err(|tmp_err| {
                            format!("failed to open local cache ({e}), and the placeholder cache at {} ({tmp_err})", tmp.display())
                        })?)
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
            //
            // Not at all while the cache is locked: the list would put real
            // accounts beside a throwaway cache, which is the incident
            // `cache_lock` exists for.
            let cache_locked = cache_lock::engaged();
            let accounts_list = if cache_locked.is_some() {
                log!("cache locked: not reading accounts.json");
                Vec::new()
            } else {
                accounts::load_accounts()
            };
            log!(
                "loaded {} from persistence",
                accounts::account_census(&accounts_list)
            );

            // A Managed SSH setup killed mid-way (Android does this to a
            // backgrounded app) leaves a key directory with no account. But
            // `accounts_list` above came back empty both when this user
            // genuinely has no accounts AND when accounts.json could not be
            // read or parsed — and sweeping on the latter would delete every
            // live Managed account's private key. Skip the sweep entirely
            // when the load itself failed.
            // While locked `accounts_list` is empty by choice, and sweeping
            // against it would delete every Managed account's key.
            if cache_locked.is_some() {
                log!("skipping ssh key sweep: cache locked");
            } else if accounts::last_load_failed() {
                log!("skipping ssh key sweep: accounts.json could not be read");
            } else if let Some(ssh_dir) = paths::data_base().map(|d| d.join("jodd").join("ssh")) {
                let n = backend::ssh::setup::sweep_orphan_key_dirs(&ssh_dir, &accounts_list, backend::ssh::setup::ORPHAN_MIN_AGE);
                if n > 0 {
                    log!("swept {n} orphaned ssh key director{}", if n == 1 { "y" } else { "ies" });
                }
            }

            // The same for AI host keys (`<data>/jodd/ai-hosts`), left by a
            // setup that never reached Save or by a "Run a command" setup
            // the user walked away from. Their names live in accounts.json
            // AND in the app's AI settings, so a file that did not read —
            // either one — means no sweep: a wrongly deleted key silently
            // breaks a working AI host.
            if cache_locked.is_some() {
                log!("skipping AI host key sweep: cache locked");
            } else if accounts::last_load_failed() {
                log!("skipping AI host key sweep: accounts.json could not be read");
            } else {
                let app = match app_llm_config::load_status() {
                    app_llm_config::Loaded::Missing => Some(None),
                    app_llm_config::Loaded::Config(c) => Some(Some(c.llm)),
                    app_llm_config::Loaded::Unreadable(_) => None,
                };
                match (app, paths::data_base()) {
                    (None, _) => log!("skipping AI host key sweep: the app's AI settings could not be read"),
                    (Some(app), Some(base)) => {
                        let names = ai_host_keep_set(&accounts_list, app.as_ref());
                        let keep: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
                        let dir = base.join("jodd").join("ai-hosts");
                        let n = backend::ssh::setup::sweep_orphan_dirs(&dir, &keep, backend::ssh::setup::ORPHAN_MIN_AGE);
                        if n > 0 {
                            log!("swept {n} orphaned AI host key director{}", if n == 1 { "y" } else { "ies" });
                        }
                    }
                    (Some(_), None) => {}
                }
            }

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
                captures: Mutex::new(capture::Queue::default()),
            });

            // The main window is built HERE, not by Tauri from the config
            // (`"create": false` in tauri.conf.json) — gotcha #36. Tauri builds
            // config windows before it calls this hook, and the page's first
            // load asks WebKit to launch its WebContent/GPU/Networking
            // processes, a handshake whose UI half needs the main thread. The
            // `Db::open` above blocks that thread on the keychain read for as
            // long as the macOS prompt sits unanswered — after every update,
            // because the binary is ad-hoc signed. Over a block of hours the
            // half-launched processes die (what kills them is not pinned; six
            // minutes alone does not); when this hook returns WebKit logs "web
            // process failed to launch", its automatic reload finds "no current
            // item to reload", and the window stays white until the app quits.
            //
            // Built after `app.manage` so no IPC from the page can reach a
            // command before its state exists, and before the sync worker,
            // whose iCloud paths look the "main" window up.
            let main_window = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .cloned()
                .ok_or("tauri.conf.json declares no \"main\" window")?;
            let mut main_builder = tauri::WebviewWindowBuilder::from_config(app.handle(), &main_window)?;
            // Locked: tell the page before it loads, so main.ts mounts the
            // unlock screen instead of the app (no IPC needed to decide).
            if let Some(lock) = &cache_locked {
                main_builder = main_builder.initialization_script(lock.init_script());
            }
            // The main window shows note bodies, and a note body is HTML from
            // somewhere else. The CSP stops it running script; this stops it
            // taking the window to a web page (a `<meta http-equiv=refresh>`
            // in a body needs no script and no click).
            main_builder = main_builder.on_navigation(|u| {
                let ok = main_window_may_navigate_to(u);
                if !ok {
                    log!("main window: refused a navigation to {}://{}", u.scheme(), u.host_str().unwrap_or(""));
                }
                ok
            });
            main_builder.build()?;

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
            //
            // Since Share to Jodd (spec 2026-10-06-share-to-jodd) this block runs
            // on EVERY platform: the same two entry points also deliver
            // `jodd://capture` links — from the Android share sheet (rewritten
            // in MainActivity.kt), and on desktop from the OS (macOS Apple
            // Event, Windows argv forwarded by tauri-plugin-single-instance).
            // `capture::route` decides which is which, by scheme first, so a
            // `jodd:` link can never reach `complete_oauth`. OAuth by deep link
            // stays Android-only: desktop signs in through the loopback
            // listener, and registers no scheme that could carry a callback.
            // A capture delivered through both entry points is the same
            // payload twice, which `capture::Queue`'s dedupe window absorbs.
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let seen_codes: Arc<Mutex<std::collections::HashSet<String>>> =
                    Arc::new(Mutex::new(std::collections::HashSet::new()));
                let dispatch = {
                    let seen_codes = seen_codes.clone();
                    move |handle: &tauri::AppHandle, url: &str| {
                        let seen = seen_codes.clone();
                        let h = handle.clone();
                        capture_commands::handle_deep_link(handle, url, move |code, state| {
                            // `cfg!`, not `#[cfg]`: one path that type-checks
                            // the same on every target — complete_oauth exists
                            // everywhere, and this host can't compile Android.
                            if !cfg!(target_os = "android") {
                                log!("deep link: ignored an OAuth-shaped link (desktop signs in via loopback)");
                            } else if seen.lock().unwrap().insert(code.clone()) {
                                tauri::async_runtime::spawn(complete_oauth(h, code, state));
                            }
                        });
                    }
                };

                let handle = app.handle().clone();
                let on_open = dispatch.clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        on_open(&handle, url.as_str());
                    }
                });
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    for url in urls {
                        dispatch(app.handle(), url.as_str());
                    }
                }
                // Windows: a portable or dev run has no installer to write the
                // scheme's registry key; register it for this user. Harmless
                // when the installer already did.
                #[cfg(windows)]
                if let Err(e) = app.deep_link().register_all() {
                    log!("deep link: register_all failed: {e}");
                }
            }

            // Both touch the keychain and accounts.json (the worker also
            // rotates refresh tokens), so neither starts while locked.
            if cache_locked.is_some() {
                log!("cache locked: legacy keychain migration and sync worker not started");
                return Ok(());
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                migrate_legacy_keychain(&state).await;
            });
            // Repairs the search map an older Jodd or jodd-mcp left stale.
            // It reads the whole index, so not in `Db::open` (launch time,
            // gotcha #36) — saves made before it ends are cleaned up by it.
            let db = app.state::<AppState>().db.clone();
            tauri::async_runtime::spawn_blocking(move || db.fts_repair_rowid_map_logged());
            spawn_sync_worker(app.handle().clone());
            // The Claude Code bridge's live sync (roadmap #20, Phase 3). After
            // the cache-lock return above, like the sync worker.
            #[cfg(not(target_os = "android"))]
            agent_files_commands::spawn_live_sync(app.handle().clone());
            Ok(())
        })
        // Every command passes the cache lock first — see `cache_lock`. While
        // the cache is locked only `retry_cache_unlock` runs.
        .invoke_handler(cache_lock::gate(tauri::generate_handler![
            cache_lock::retry_cache_unlock,
            tv_commands::list_tvs,
            tv_commands::link_tv,
            tv_commands::repair_tv,
            tv_commands::refresh_tv,
            tv_commands::rename_tv,
            tv_commands::set_default_tv,
            tv_commands::unpair_tv,
            tv_commands::tv_now_playing,
            backup_commands::backup_destinations,
            backup_commands::backup_add_local_destination,
            backup_commands::backup_remove_destination,
            backup_commands::backup_run,
            backup_commands::backup_statuses,
            backup_commands::backup_list_slugs,
            backup_commands::backup_list_versions,
            backup_commands::backup_delete_version,
            backup_commands::restore_preview,
            backup_commands::restore_apply,
            transfer_commands::preview_note_transfer,
            transfer_commands::transfer_notes,
            capture_commands::take_pending_captures,
            capture_commands::save_capture,
            capture_commands::append_capture,
            capture_commands::save_text_note,
            capture_commands::append_text_note,
            capture_commands::list_capture_folders,
            capture_commands::peek_link_metadata,
            capture_commands::discard_capture,
            capture_commands::complete_capture,
            capture_commands::install_share_shortcut,
            curate_commands::curate_scan,
            curate_commands::curate_list,
            curate_commands::curate_preview,
            curate_commands::curate_apply,
            curate_commands::curate_dismiss,
            studio_commands::studio_plan,
            studio_commands::studio_related,
            studio_commands::generate_studio_note,
            agent_workspace_commands::agent_workspace_status,
            agent_workspace_commands::enable_agent_workspace,
            agent_workspace_commands::set_folder_hidden_from_agents,
            agent_files_commands::agent_files_status,
            agent_files_commands::set_agent_mirror,
            agent_files_commands::sync_agent_files_now,
            agent_files_commands::agent_skills_view,
            agent_files_commands::set_agent_skill,
            agent_files_commands::remove_agent_skill,
            llm::budget::get_ai_limits,
            llm::budget::set_ai_limits,
            llm::receipts::list_ai_receipts,
            llm::receipts::delete_ai_receipts,
            llm::receipts::get_ai_receipt_retention,
            llm::receipts::set_ai_receipt_retention,
            llm::receipts::export_ai_receipts,

            auth_commands::platform_name,
            auth_commands::get_auth_url,
            auth_commands::open_auth_url,
            account_commands::is_authenticated,
            account_commands::list_accounts,
            account_commands::remove_account,
            account_commands::get_account_settings,
            account_commands::backend_capabilities,
            account_commands::update_account_settings,
            account_commands::add_local_account,
            android_storage::local_folder_access,
            android_storage::request_local_folder_access,
            android_storage::pick_local_folder,
            app_update::check_app_update,
            app_update::download_app_update,
            app_update::install_app_update,
            app_update::request_install_permission,
            ssh_commands::add_ssh_account,
            ssh_commands::list_remote_dirs,
            ssh_commands::ssh_fingerprint,
            ssh_commands::ssh_setup_managed,
            ssh_commands::ssh_setup_prepare_command,
            ssh_commands::ssh_setup_finish_command,
            ssh_commands::ssh_setup_cancel_command,
            ai_host_commands::ai_host_setup,
            ai_host_commands::ai_host_status,
            ai_host_commands::ai_host_update,
            ai_host_commands::ai_host_forget,
            ai_host_commands::ai_host_prepare_command,
            ai_host_commands::ai_host_finish_command,
            ai_host_commands::ai_host_cancel_command,
            ai_host_commands::ai_host_reinstall_commands,
            ai_host_commands::ai_host_pinned_key_matches,
            account_commands::icloud_sign_in,
            account_commands::icloud_reauthenticate,
            icloud_diag_commands::icloud_census,
            icloud_diag_commands::icloud_write_selftest,
            icloud_diag_commands::icloud_relocation_selftest,
            icloud_diag_commands::icloud_content_write_selftest,
            icloud_diag_commands::icloud_debug_note_history,
            icloud_diag_commands::icloud_debug_record_lookup,
            account_commands::rename_local_account,
            account_commands::count_pending_pushes,
            account_commands::retry_blocked_push,
            account_commands::list_sync_blocks,
            account_commands::retry_blocked_pin,
            account_commands::retry_blocked_folder,
            account_commands::set_account_status,
            listing_commands::list_notes,
            listing_commands::list_notes_in_folder,
            listing_commands::list_cached_notes_in_folder,
            listing_commands::refetch_note,
            listing_commands::get_trashed_note_preview,
            listing_commands::list_cached_notes,
            note_persistence,
            get_cached_note,
            merge_onto_row,
            editor_diagnostic,
            listing_commands::index_account,
            listing_commands::needs_reindex_after_recovery,
            listing_commands::clear_reindex_marker,
            listing_commands::sync_pin_state,
            listing_commands::sync_tag_state,
            save_note,
            listing_commands::delete_note,
            listing_commands::move_notes_batch,
            listing_commands::delete_notes_batch,
            tag_commands::set_pin,
            tag_commands::set_pin_batch,
            listing_commands::get_note_attachments,
            listing_commands::list_trashed_notes,
            listing_commands::restore_note,
            listing_commands::search_notes,
            listing_commands::note_connections,
            listing_commands::resolve_note_link,
            listing_commands::note_citations,
            listing_commands::check_duplicate_citations,
            listing_commands::search_note_links,
            tag_commands::add_tag,
            tag_commands::remove_tag,
            tag_commands::list_tags,
            tag_commands::list_note_tags,
            tag_commands::list_cached_notes_with_tags,
            tag_commands::rename_tag,
            tag_commands::delete_tag,
            listing_commands::list_folders,
            listing_commands::list_folder_kinds,
            listing_commands::list_orphaned_notes,
            listing_commands::list_stale_notes,
            listing_commands::list_unreviewed_notes,
            listing_commands::list_unreviewed_trust,
            listing_commands::verify_note,
            listing_commands::list_extract_notes,
            folder_commands::create_folder,
            folder_commands::rename_folder,
            folder_commands::delete_folder,
            folder_commands::move_folder,
            orphan_commands::cleanup_orphans,
            orphan_commands::get_dup_stats,
            orphan_commands::preview_orphans,
            orphan_commands::trash_specific_messages,
            ai_commands::extract_note,
            ai_commands::re_extract_note,
            ingest_commands::analyze_ingest_sources,
            ingest_commands::discover_link_candidates,
            ingest_commands::pasted_link_candidates,
            ingest_commands::pick_link_candidates,
            ingest_commands::render_source_for_ingest,
            ingest_commands::ingest_sources,
            ai_commands::append_extract_note,
            ai_commands::run_llm_workflow,
            ai_commands::append_llm_workflow_note,
            ai_commands::preview_action_items,
            ai_commands::apply_action_items,
            ai_commands::discard_action_items,
            ai_commands::ask_jodd,
            ai_commands::begin_ask,
            ai_commands::end_ask,
            ask_history_commands::resume_ask,
            ask_history_commands::save_ask_answer,
            ask_history_commands::list_ask_conversations,
            ask_history_commands::load_ask_conversation,
            ask_history_commands::rename_ask_conversation,
            ask_history_commands::set_ask_conversation_archived,
            ask_history_commands::delete_ask_conversation,
            ask_history_commands::clear_ask_history,
            ask_history_commands::get_ask_history_enabled,
            ask_history_commands::set_ask_history_enabled,
            ai_commands::cancel_ask,
            ai_commands::suggest_wiki_links,
            ai_commands::suggest_note_folder,
            ai_commands::apply_wiki_link_appends,
            ai_commands::cancel_extraction,
            ai_settings_commands::get_llm_settings,
            ai_settings_commands::list_agent_cli_presets,
            ai_settings_commands::test_llm_provider,
            ai_settings_commands::update_llm_settings,
            ai_settings_commands::allow_ai_for_account,
            ai_settings_commands::deny_ai_for_account,
            settings_commands::get_oauth_config,
            settings_commands::get_oauth_settings,
            settings_commands::save_oauth_config,
            settings_commands::clear_oauth_config,
            settings_commands::get_ms_oauth_config,
            settings_commands::save_ms_oauth_config,
            settings_commands::get_app_llm_config,
            settings_commands::check_app_llm_key,
            settings_commands::delete_app_llm_key,
            settings_commands::get_app_llm_provider,
            settings_commands::get_data_since,
            settings_commands::set_app_llm_config,
            settings_commands::get_stt_config,
            settings_commands::set_stt_config,
            settings_commands::get_youtube_key_status,
            settings_commands::get_link_settings,
            settings_commands::set_link_settings,
            settings_commands::set_youtube_key,
            settings_commands::clear_youtube_key,
            settings_commands::test_stt,
            settings_commands::get_log_settings,
            settings_commands::set_file_logging_enabled,
            settings_commands::clear_log_file,
            listing_commands::flush_sync,
        ]))
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
    fn backup_commands_are_registered() {
        let registered = registered_commands();
        let missing: Vec<&str> = [
            "backup_destinations",
            "backup_add_local_destination",
            "backup_remove_destination",
            "backup_run",
            "backup_statuses",
            "backup_list_slugs",
            "backup_list_versions",
            "backup_delete_version",
            "restore_preview",
            "restore_apply",
        ]
        .into_iter()
        .filter(|name| !registered.contains(*name))
        .collect();
        assert!(missing.is_empty(), "not in generate_handler!: {missing:?}");
    }

    #[test]
    fn tv_commands_are_registered() {
        let registered = registered_commands();
        let missing: Vec<&str> = [
            "list_tvs",
            "link_tv",
            "repair_tv",
            "refresh_tv",
            "rename_tv",
            "set_default_tv",
            "unpair_tv",
            "tv_now_playing",
        ]
        .into_iter()
        .filter(|name| !registered.contains(*name))
        .collect();
        assert!(missing.is_empty(), "not in generate_handler!: {missing:?}");
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
        let known_dynamic_commands = ["list_orphaned_notes", "list_stale_notes", "list_extract_notes", "list_unreviewed_notes"];
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
        push_blocked_by_remote: false,
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

        db.insert_local_new(&mk_note(acct, uuid, "<p>hello</p>"), &crate::provenance::Actor::test()).unwrap();

        // Worker pushes it — the same calls push_one_dirty makes.
        let op = SaveOp {
            title: "T",
            body_html: "<p>hello</p>",
            existing_remote_id: None,
            existing_uuid: Some(uuid),
            existing_created_date: None,
            label: "Notes",
            base_version: None,
        };
        let saved = v.save_note_full(&op, &[]).await.unwrap();
        db.mark_pushed(uuid, acct, &saved.id, &saved.version, &saved.date, &saved.body_html, 1)
            .unwrap();

        // "Keep typing": a second local edit lands before the next poll — the
        // ordinary flow, and the one that puts the row back in Dirty while
        // the stale-vs-correct remote_version question is what matters.
        db.apply_local_edit(uuid, acct, "T", "<p>hello more</p>", "Notes", &crate::provenance::Actor::test()).unwrap();

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

    /// A keep-both reconcile replaces the primary's CONTENT with the remote's,
    /// and an editor still open on that note is holding text based on the
    /// content that was replaced. Its next save carries the `local_version` it
    /// rendered from, and that save must be refused — otherwise it lands on the
    /// primary and the other device's edit is overwritten remotely.
    ///
    /// `upsert_from_remote` leaves `local_version` alone (it counts LOCAL
    /// writes), so before this guard the number the editor held and the number
    /// the row held were the same after a keep-both, and the compare-and-swap
    /// in `apply_local_edit_versioned` had nothing to compare.
    #[tokio::test]
    async fn a_keep_both_reconcile_refuses_a_save_based_on_the_replaced_content() {
        let db = temp_db();
        let acct = "a@example.com";
        let uuid = "BBBBBBBB-1111-2222-3333-444444444444";
        let vault = tempfile::tempdir().unwrap();
        let v = LocalFsVertical::new(vault.path().to_path_buf(), acct.to_string());

        db.insert_local_new(&mk_note(acct, uuid, "<p>base</p>"), &crate::provenance::Actor::test()).unwrap();
        let op = |body: &'static str, id: Option<&'static str>| SaveOp {
            title: "T",
            body_html: body,
            existing_remote_id: id,
            existing_uuid: Some(uuid),
            existing_created_date: None,
            label: "Notes",
            base_version: None,
        };
        let saved = v.save_note_full(&op("<p>base</p>", None), &[]).await.unwrap();
        db.mark_pushed(uuid, acct, &saved.id, &saved.version, &saved.date, &saved.body_html, 1)
            .unwrap();

        // This device's editor saves an edit locally (Dirty, not yet pushed).
        db.apply_local_edit(uuid, acct, "T", "<p>base, mine</p>", "Notes", &crate::provenance::Actor::test()).unwrap();
        let rendered = db.get(uuid, acct).unwrap().unwrap().local_version;

        // Another device edits the same note. LocalFs's version is the Date
        // header at second resolution, so wait one out or the edit is invisible.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let theirs_op = SaveOp { existing_remote_id: Some(saved.id.as_str()), ..op("<p>base, theirs</p>", None) };
        let theirs = v.save_note_full(&theirs_op, &[]).await.unwrap();
        let fetched = v.fetch_note(&theirs.id).await.unwrap();
        reconcile_one_db(&db, acct, &fetched, db::RemotePin::LocalWins, "CONFLICT-COPY-UUID");
        assert!(
            db.get("CONFLICT-COPY-UUID", acct).unwrap().is_some(),
            "precondition: this must be the keep-both branch"
        );

        // The editor, still open, saves on top of what it rendered.
        let applied = db
            .apply_local_edit_versioned(uuid, acct, "T", "<p>base, mine, more</p>", "Notes", rendered, &crate::provenance::Actor::test())
            .unwrap();
        assert!(!applied, "a save based on the replaced content must be refused");
        let row = db.get(uuid, acct).unwrap().unwrap();
        assert!(row.body_html.contains("theirs"), "the other device's edit must survive: {}", row.body_html);
        assert_eq!(row.sync_state, db::SyncState::Clean, "the primary holds the remote state, unchanged");
    }
}

/// Gotcha #38: a conflict copy is a CREATE, and a CREATE carries hashtag spans
/// as plain text (Jodd cannot mint Apple's tag objects). The copy must hold that
/// shape locally from the start, or it shows tags that vanish on the next pull.
#[cfg(test)]
mod icloud_conflict_copy_tag_tests {
    use super::*;

    const ICLOUD: &str = "icloud:someone@me.com";
    const UUID: &str = "c7793f2b-c4c3-4b28-82e3-d2248bd4899b";
    const SPAN: &str = r#"<span data-jodd-inline="hashtag" data-ref="ia-1" contenteditable="false">#lablower</span>"#;

    fn temp_db() -> db::Db {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::mem::forget(dir);
        db::Db::open_unencrypted(&path).expect("open temp db")
    }

    fn pulled(body_html: &str, version: &str) -> crate::backend::Note {
        serde_json::from_value(serde_json::json!({
            "id": UUID, "uuid": UUID, "title": "T", "body_html": body_html,
            "date": "Sun, 11 Oct 2026 10:00:00 +0700", "version": version, "label": "Notes",
        }))
        .unwrap()
    }

    #[test]
    fn an_icloud_conflict_copy_carries_tag_text_not_tags() {
        let db = temp_db();
        let pin = db::RemotePin::RemoteWins;
        reconcile_one_db(&db, ICLOUD, &pulled(&format!("<div>T</div><div>{SPAN} base</div>"), "v1"), pin, "unused");
        db.apply_local_edit(UUID, ICLOUD, "T", &format!("<div>T</div><div>{SPAN} mine</div>"), "Notes", &Actor::test())
            .unwrap();

        // Another device edits the same note: keep-both.
        reconcile_one_db(&db, ICLOUD, &pulled(&format!("<div>T</div><div>{SPAN} theirs</div>"), "v2"), pin, "copy-1");

        let copy = db.get("copy-1", ICLOUD).unwrap().expect("a conflict copy");
        assert!(!copy.body_html.contains("data-jodd-inline"), "{}", copy.body_html);
        assert!(copy.body_html.contains("#lablower mine"), "the local text survives: {}", copy.body_html);
        assert!(db.list_tags_for(ICLOUD, "copy-1").unwrap().is_empty(), "tag text is not a tag on iCloud");
        // The primary took the remote, whose tag is a real object.
        assert_eq!(db.list_tags_for(ICLOUD, UUID).unwrap(), vec!["lablower"]);
    }

    /// Other backends keep the copy byte-for-byte, tags included.
    #[test]
    fn a_gmail_conflict_copy_keeps_its_body_and_tags() {
        let db = temp_db();
        let acct = "gmail:someone@gmail.com";
        let pin = db::RemotePin::LocalWins;
        let note = |body: &str, version: &str| {
            let mut n = pulled(body, version);
            n.uuid = "AAAAAAAA-1111-2222-3333-444444444444".into();
            n
        };
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        reconcile_one_db(&db, acct, &note("<div>T</div><div>#work base</div>", "v1"), pin, "unused");
        db.apply_local_edit(uuid, acct, "T", "<div>T</div><div>#work mine</div>", "Notes", &Actor::test()).unwrap();
        reconcile_one_db(&db, acct, &note("<div>T</div><div>theirs</div>", "v2"), pin, "COPY-1");

        let copy = db.get("COPY-1", acct).unwrap().expect("a conflict copy");
        assert_eq!(copy.body_html, "<div>T</div><div>#work mine</div>");
        assert_eq!(db.list_tags_for(acct, "COPY-1").unwrap(), vec!["work"]);
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
mod gmail_from_header_tests {
    use crate::accounts::{self, Account, BackendKind};
    use crate::backend::SaveOp;
    use std::collections::HashMap;

    /// Regression: Gmail message 1a0f261f5a4de2c7 (read RAW 2026-09-30) had
    /// `From: gmail:alex@example.com` — the account id where the bare
    /// address belongs. Starts from an account row, as `vertical_from_parts`
    /// does, and reads the bytes `save_note_full` would insert.
    #[test]
    fn a_gmail_note_is_from_the_bare_address_not_the_account_id() {
        let id = accounts::account_id_for(BackendKind::Gmail, "a@b.c");
        assert_eq!(id, "gmail:a@b.c", "fixture: the id must carry the prefix");
        let account = Account {
            id,
            email: "a@b.c".into(),
            added_at: "2026-09-30T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: Default::default(),
            backend_kind: BackendKind::Gmail,
            root_dir: None,
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: Default::default(),
            pending_removal: false,
        };
        let v = super::gmail_vertical(&account, "t".into(), HashMap::new());
        let op = SaveOp {
            title: "T",
            body_html: "<div>body</div>",
            existing_remote_id: None,
            existing_uuid: None,
            existing_created_date: None,
            label: "Notes",
            base_version: None,
        };
        let raw = v.compose_note(&op, &[]).raw;
        let headers = raw.split("\r\n\r\n").next().unwrap();
        let from: Vec<&str> = headers.split("\r\n").filter(|l| l.starts_with("From:")).collect();
        assert_eq!(from, ["From: a@b.c"], "headers were:\n{headers}");
        // The Message-Id domain was already right (it splits on '@'); pin it.
        let msg_id = headers.split("\r\n").find(|l| l.starts_with("Message-Id:")).unwrap();
        assert!(msg_id.ends_with("@b.c>"), "{msg_id}");
        // Pin sidecars read the same field for their own From header.
        assert_eq!(v.user_email, "a@b.c");
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
        let src = concat!(
            include_str!("lib.rs"),
            "\n",
            include_str!("note_commands.rs"),
            "\n",
            include_str!("tag_commands.rs"),
            "\n",
            include_str!("folder_commands.rs"),
            "\n",
            include_str!("listing_commands.rs"),
            "\n",
            include_str!("ai_commands.rs"),
            "\n",
            include_str!("ingest_commands.rs")
        );
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
        // alex.h@outlook.example, and must NOT be refused. Folders are refused —
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
/// `vertical_for` is the only place an SSH vertical learns it is Managed;
/// without `.managed(..)` every Managed user sees `ssh-add` / terminal advice
/// for a key Jodd holds. Nothing else exercises `vertical_for` (it needs a
/// Tauri `State`), so this pins the wiring at the source.
#[cfg(test)]
mod ssh_vertical_wiring_tests {
    #[test]
    fn the_ssh_arm_marks_managed_accounts_managed() {
        let body = crate::test_support::extract_fn_body(include_str!("lib.rs"), "async fn vertical_for(");
        let arm = &body[body.find("accounts::BackendKind::Ssh =>").expect("an Ssh arm")..];
        assert!(arm.contains("let managed = backend::ssh::is_managed(a);"), "managed must come from is_managed:\n{arm}");
        assert!(arm.contains(".managed(managed)"), "the vertical must be told:\n{arm}");
    }
}

#[cfg(test)]
mod pending_removal_tests {
    use super::*;
    use crate::accounts::AccountStatus::{Active, Draining, Inactive};
    use crate::accounts::{Account, AccountStatus};

    /// A queued removal completes from the sync worker with `revoke_key =
    /// true`; the user's "keep the key" choice survives only if EVERY queue
    /// site records it before queueing. Two sites exist (before and after the
    /// exclusive lease).
    #[test]
    fn every_queue_site_records_the_keep_key_choice_first() {
        let body = crate::test_support::extract_fn_body(include_str!("account_commands.rs"), "async fn remove_account(");
        let queues: Vec<usize> = body.match_indices("queue_removal(&mut list").map(|(i, _)| i).collect();
        assert_eq!(queues.len(), 2, "expected both queue sites");
        for q in queues {
            let record = body[..q].rfind("record_keep_key_choice(").expect("a record before every queue");
            assert!(body[record..q].matches("queue_removal(").count() == 0, "the record must precede THIS queue site");
        }
    }

    /// Local-first: removal must not wait on the server. The key is retired
    /// on a spawned task, never awaited inline.
    #[test]
    fn removal_retires_the_ssh_key_off_the_critical_path() {
        let body = crate::test_support::extract_fn_body(include_str!("account_commands.rs"), "async fn perform_account_removal(");
        let retire = body.find("retire_managed_key(").expect("removal retires a Managed key");
        let spawn = body[..retire].rfind("spawn_blocking(").expect("inside a spawned task");
        assert!(!body[spawn..].contains("}).await"), "the retire task must not be awaited");
    }

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
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
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
            push_blocked_by_remote: false,
        };
        db.insert_local_new(&note, &crate::provenance::Actor::test()).unwrap();

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
        push_blocked_by_remote: false,
        }
    }

    #[test]
    fn apply_local_edit_with_retry_recomputes_against_fresh_state_after_a_lost_race() {
        let db = temp_db();
        let uuid = "AAAAAAAA-1111-2222-3333-444444444444";
        db.insert_local_new(&mk_note("a@x.com", uuid, "T", "<p>base</p>"), &crate::provenance::Actor::test()).unwrap();

        let mut first_attempt = true;
        apply_local_edit_with_retry(&db, "a@x.com", uuid, 5, &crate::provenance::Actor::test(), |existing| {
            if first_attempt {
                first_attempt = false;
                db.apply_local_edit(uuid, "a@x.com", &existing.title, "<p>base</p><p>racer</p>", &existing.label, &crate::provenance::Actor::test())
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

        // A folder synced within the grace window survives a listing that
        // lacks it: the listing may predate the push (db::prune_clean docs).
        let after = StubTransport { folders: vec![folder("id-a", "Notes")] };
        reconcile_folders_from_vertical(&db, "acct", &after, true).await.unwrap();
        assert_eq!(db.list_folders("acct").unwrap().len(), 2, "a just-synced folder must not be pruned");

        db.age_sync_for_test("acct", db::PRUNE_GRACE_MS + 1_000);
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
            let err = refuse_empty_client_id(empty, "MS_CLIENT_ID", "Microsoft").unwrap_err();
            // The developer half: which variable, and where it goes.
            assert!(err.contains("MS_CLIENT_ID"), "must name the variable to set: {err}");
            assert!(err.contains(".env"), "must say where to set it: {err}");
            // The release half (2026-10-11): v0.33.0 downloaded from the release
            // told its user to edit a `.env` they do not have. A downloaded
            // build must be told the fault is the build's, and where to report it.
            assert!(err.contains("built without a Microsoft client id"), "must blame the build: {err}");
            assert!(err.contains("If you downloaded Jodd"), "must address a release user: {err}");
            assert!(err.contains("Jodd-public/issues"), "must say where to report it: {err}");
        }
        assert!(refuse_empty_client_id("f95a0627-not-a-real-id", "MS_CLIENT_ID", "Microsoft").is_ok());
        // Both providers route through the same check, so both name their own var.
        let err = refuse_empty_client_id("", "GOOGLE_CLIENT_ID", "Google").unwrap_err();
        assert!(err.contains("GOOGLE_CLIENT_ID"), "must name the variable to set: {err}");
        assert!(err.contains("built without a Google client id"), "must name its own provider: {err}");
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
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
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

// Stays in lib.rs: it asserts `fn short_reason` is ABSENT from
// studio_commands.rs, which cannot hold if the test lives in that file.
#[cfg(test)]
mod studio_commands_tests {
    use crate::test_support::extract_fn_body;

    const SRC: &str = include_str!("studio_commands.rs");

    fn generate_body() -> String {
        extract_fn_body(SRC, "pub async fn generate_studio_note(")
    }

    #[test]
    fn studio_generate_checks_everything_before_building_a_provider() {
        let body = generate_body();
        let at = |needle: &str| body.find(needle).unwrap_or_else(|| panic!("generate_studio_note must call `{needle}`"));
        let order = [at("refuse_write("), at("AiScope::load()"), at("load_sources("), at("estimate("), at("checked_account_provider(")];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "wrong order: {order:?}");
    }

    #[test]
    fn studio_generate_runs_inside_one_budget_admission() {
        let body = generate_body();
        let receipts = body.find("receipts::run(").expect("receipts::run");
        let generate = body.find("generate(").expect("generate(");
        assert!(receipts < generate, "the run is admitted before the AI work");
        assert!(body.contains("\"studio_generate\""), "receipt kind");
        assert!(body[generate..].contains("}).await"), "generate( sits inside the admitted future");
        assert_eq!(body.matches("receipts::run(").count(), 1, "one admission, not one per call");
    }

    #[test]
    fn studio_generate_always_removes_its_in_flight_entry() {
        let body = generate_body();
        let insert = body.find("in_flight_extracts").expect("registers the cancel token");
        let generate = body[insert..].find("generate(").map(|i| i + insert).expect("generate(");
        let remove = body[generate..].find(".remove(").map(|i| i + generate).expect("removes the entry after generate(");
        let between = &body[insert..remove];
        assert!(!between.contains('?'), "no `?` between the insert and the removal: {between}");
        assert_eq!(body.matches(".insert(request_id").count(), 1);
    }

    #[test]
    fn studio_plan_never_builds_a_provider() {
        let body = extract_fn_body(SRC, "pub async fn studio_plan(");
        for banned in ["provider", "run_workflow", "checked_account", "receipts::run"] {
            assert!(!body.contains(banned), "studio_plan must not touch `{banned}`");
        }
        assert!(body.contains("AiScope::load()"));
    }

    #[test]
    fn studio_errors_are_shaped_for_the_ui() {
        let src = SRC;
        assert!(src.contains("StudioError::Cancelled => \"cancelled\""));
        assert!(src.contains("crate::ingest::run::short_reason"), "reuse ingest's rule; do not duplicate it");
        assert!(!src.contains("fn short_reason"), "short_reason is shared, not copied");
        assert!(src.contains("Could not save the note. Try again."));
    }

    #[test]
    fn studio_related_never_builds_a_provider() {
        let body = extract_fn_body(SRC, "pub async fn studio_related(");
        for banned in ["provider", "run_workflow", "checked_account", "receipts::run"] {
            assert!(!body.contains(banned), "studio_related must not touch `{banned}`");
        }
        assert!(body.contains("AiScope::load()"));
        assert!(body.contains("find_related("));
    }

    #[test]
    fn studio_commands_are_registered() {
        let lib = include_str!("lib.rs");
        assert!(lib.contains("studio_commands::studio_plan,"));
        assert!(lib.contains("studio_commands::studio_related,"));
        assert!(lib.contains("studio_commands::generate_studio_note,"));
        assert!(lib.contains("\nmod studio_commands;"));
    }
}

#[cfg(test)]
mod ai_scope_tripwire {
    /// Spec 2026-10-08 §4.4: every function that builds an AI provider loads
    /// the hidden-folder scope FIRST. Commands can't be invoked in tests (no
    /// `State` harness), so this reads the source — the same pattern as
    /// `run_llm_workflow_tests`. A new AI command that skips the scope fails
    /// here, not in a user's vault (gotcha #6's shape).
    /// Every file that builds an LLM provider. A command moved out of
    /// `lib.rs` must be added here, or both tests below stop seeing it.
    const SOURCES: [(&str, &str); 6] = [("lib.rs", include_str!("lib.rs")), ("curate_commands.rs", include_str!("curate_commands.rs")), ("studio_commands.rs", include_str!("studio_commands.rs")), ("ai_commands.rs", include_str!("ai_commands.rs")), ("ingest_commands.rs", include_str!("ingest_commands.rs")), ("ai_settings_commands.rs", include_str!("ai_settings_commands.rs"))];

    const EXEMPT: &[(&str, &str)] = &[
        ("fn test_llm_provider", "sends only CONNECTION_TEST_SAMPLE"),
    ];

    /// Every top-level fn in `src` that builds a provider, with its body.
    fn provider_fns(src: &str) -> Vec<(String, &str)> {
        let prefixes = ["fn ", "async fn ", "pub fn ", "pub async fn ", "pub(crate) fn ", "pub(crate) async fn "];
        let mut out = Vec::new();
        for (i, _) in src.match_indices('\n') {
            let rest = &src[i + 1..];
            if !prefixes.iter().any(|p| rest.starts_with(p)) {
                continue;
            }
            let body = &rest[..rest.find("\n}\n").expect("top-level fn is closed")];
            if body.contains("checked_account_provider(&") || body.contains("resolve_app_provider(") {
                out.push((body.lines().next().unwrap().to_string(), body));
            }
        }
        out
    }

    /// M6 (final review): the AI tag vocabulary comes from
    /// `AiScope::tags_visible_to_ai`, never the unfiltered sidebar query.
    #[test]
    fn no_provider_site_reads_the_unfiltered_tag_list() {
        let sites: Vec<_> = SOURCES.iter().flat_map(|(_, src)| provider_fns(src)).collect();
        assert!(sites.len() >= 9, "scan broke: {} provider fns", sites.len());
        for (header, body) in sites {
            assert!(!body.contains("list_all_tags("), "`{header}` reads list_all_tags; use AiScope::tags_visible_to_ai");
        }
    }

    /// (header, where the provider is built, where the scope is loaded).
    /// `ai_scope_for_source(` loads it too (the Extract-family helper).
    fn provider_sites(src: &str) -> Vec<(String, usize, Option<usize>)> {
        provider_fns(src)
            .into_iter()
            .map(|(header, body)| {
                let build = ["checked_account_provider(&", "resolve_app_provider("].iter().filter_map(|n| body.find(n)).min().unwrap();
                let scope = ["AiScope::load(", "ai_scope_for_source("].iter().filter_map(|n| body.find(n)).min();
                (header, build, scope)
            })
            .collect()
    }

    #[test]
    fn every_provider_construction_loads_the_ai_scope_first() {
        let mut checked = 0;
        for (file, src) in SOURCES {
            for (header, build, scope) in provider_sites(src) {
                if EXEMPT.iter().any(|(sig, _)| header.contains(sig)) {
                    continue;
                }
                let scope = scope.unwrap_or_else(|| panic!("{file}: `{header}` builds a provider without AiScope::load"));
                assert!(scope < build, "{file}: `{header}` must load AiScope before building the provider");
                checked += 1;
            }
        }
        // 8 in lib.rs (extract_note_into, preview_action_items,
        // run_workflow_note_into, append_llm_workflow_note,
        // append_extract_note, suggest_wiki_links, suggest_note_folder,
        // ask_jodd) + curate_scan. Fewer means the scan itself broke.
        assert!(checked >= 9, "tripwire saw only {checked} provider sites");
    }

    /// I1 (final review): in existing-note mode `context_text` and the URLs
    /// are derived from a note's body. `run_ingest_command` loads the scope for
    /// its destination only; what it reads is checked by its callers —
    /// `ingest_sources` (`check_source`) and `re_extract_note`
    /// (`plan_re_extract`) — pinned by the tests below.
    #[test]
    fn the_private_reader_is_desktop_only_and_for_web_pages_only() {
        let body = crate::test_support::extract_fn_body(include_str!("ingest_commands.rs"), "async fn render_source_for_ingest(");
        let mobile = body.find("#[cfg(mobile)]").expect("an Android arm");
        assert!(body[mobile..].contains("DESKTOP_ONLY"), "Android must say desktop only (spec §7.3)");
        let kind = body.find("UrlKind::Web").expect("checks the link is a web page");
        let render = body.find("render::render(").expect("renders");
        assert!(kind < render, "the kind check must come before the webview opens");
        // Cancellable like every other ingest step.
        assert!(body.contains("in_flight_extracts"));
    }

    /// Owner's override of Ruling 15: the one path every ingest takes — fresh
    /// (`ingest_sources`) and Redo (`re_extract_note`) — checks the AI scope
    /// of its destination before building the provider, so before any fetch
    /// (`ingest_to_note`) or provider call.
    #[test]
    fn run_ingest_command_checks_the_destination_before_anything_is_sent() {
        let body = crate::test_support::extract_fn_body(include_str!("ingest_commands.rs"), "async fn run_ingest_command(");
        let check = body.find("check_destination_scope(").expect("run_ingest_command checks its destination");
        assert!(check < body.find("checked_account_provider(").unwrap());
        assert!(check < body.find("ingest_to_note(").unwrap());
    }

    #[test]
    fn ingest_sources_checks_its_source_before_delegating() {
        let body = crate::test_support::extract_fn_body(include_str!("ingest_commands.rs"), "async fn ingest_sources(");
        let check = body.find("check_source(").expect("ingest_sources checks its AiSource");
        assert!(check < body.find("run_ingest_command(").unwrap());
    }

    /// M1 (final review): a hidden note is refused before the provider is
    /// built, not only inside `filing::suggest_folder` afterwards.
    #[test]
    fn suggest_note_folder_checks_the_note_before_building_the_provider() {
        let body = crate::test_support::extract_fn_body(include_str!("ai_commands.rs"), "async fn suggest_note_folder(");
        let check = body.find("check_source(").expect("suggest_note_folder checks the note's folder");
        assert!(check < body.find("checked_account_provider(").unwrap());
    }

    #[test]
    fn re_extract_checks_the_note_before_either_branch() {
        let body = crate::test_support::extract_fn_body(include_str!("ai_commands.rs"), "async fn re_extract_note(");
        // The checks live in the planner (unit-tested in `re_extract_plan_tests`).
        let check = body.find("plan_re_extract(").expect("re_extract_note plans (and checks) before running");
        assert!(check < body.find("run_ingest_command(").unwrap());
        assert!(check < body.find("extract_note_into(").unwrap());
        let plan = crate::test_support::extract_fn_body(include_str!("ai_commands.rs"), "fn plan_re_extract(");
        let note_check = plan.find("check_label(account_id, &note.label)").expect("the plan checks the note's own folder");
        assert!(note_check < plan.find("extract_source(").unwrap(), "before reading any branch's source");
    }
}

#[cfg(test)]
mod signed_out_tests {
    use super::{signed_out_reason_after_read, SIGNED_OUT_REASON};

    #[test]
    fn an_unreadable_token_marks_the_account_signed_out() {
        assert_eq!(
            signed_out_reason_after_read(None, false),
            Some(Some(SIGNED_OUT_REASON.to_string()))
        );
    }

    #[test]
    fn a_readable_token_clears_only_the_signed_out_reason() {
        assert_eq!(signed_out_reason_after_read(Some(SIGNED_OUT_REASON), true), Some(None));
        // Another producer's reason (iCloud ADP today) is not this read's to clear.
        assert_eq!(signed_out_reason_after_read(Some("Advanced Data Protection"), true), None);
        assert_eq!(signed_out_reason_after_read(None, true), None);
    }

    #[test]
    fn an_already_signed_out_account_is_not_rewritten() {
        // record_backend_block writes accounts.json on a change only; asking
        // for the same value again must be "no change".
        assert_eq!(signed_out_reason_after_read(Some(SIGNED_OUT_REASON), false), None);
    }

    #[test]
    fn signed_out_outranks_another_reason() {
        // Nothing else about an account can be repaired before Jodd can sign
        // in to it, so the sign-in is what the row should ask for.
        assert_eq!(
            signed_out_reason_after_read(Some("Advanced Data Protection"), false),
            Some(Some(SIGNED_OUT_REASON.to_string()))
        );
    }

    /// The data-loss guard: auth loss used to make the frontend call
    /// `remove_account`, which wipes the account's rows — unsent edits
    /// included. Nothing on the auth-loss path may remove an account now.
    #[test]
    fn the_auth_loss_path_never_removes_the_account() {
        let src = include_str!("../../src/lib/controllers/authLoss.ts");
        // No IPC at all: the handler reaches only the deps App hands it.
        assert!(!src.contains("invoke("), "authLoss.ts must not call the backend");
        assert!(!src.contains("'remove_account'"), "authLoss.ts must not remove the account");
        let app = include_str!("../../src/App.svelte");
        let start = app.find("handleAuthLoss").expect("App.svelte still routes auth loss");
        assert!(
            !app[start..].contains("invoke('remove_account'"),
            "App.svelte must not remove an account after auth loss"
        );
    }
}

/// Commands the frontend calls on a timer or while the user types must not
/// run on the main thread. Tauri runs a synchronous `#[tauri::command] fn` on
/// it, and on macOS that is also the thread that takes keystrokes and applies
/// the webview's layer updates: a command waiting there for `Db`'s one lock
/// while the sync worker holds it is typing lag. On 2026-11-18 a 45 s sample
/// of typing found 2.3 s of the main thread waiting in `count_pending_pushes`.
#[cfg(test)]
mod main_thread_command_tests {
    #[test]
    fn commands_the_ui_polls_do_their_database_work_off_the_main_thread() {
        for (src, sig) in [
            (include_str!("account_commands.rs"), "async fn count_pending_pushes("),
            (include_str!("note_commands.rs"), "async fn note_persistence("),
            (include_str!("listing_commands.rs"), "async fn note_connections("),
            (include_str!("listing_commands.rs"), "async fn search_note_links("),
        ] {
            let body = crate::test_support::extract_fn_body(src, sig);
            assert!(body.contains("db_blocking("), "{sig} must reach the database through db_blocking");
        }
    }
}
