pub mod accounts;
pub mod applog;
pub mod auth;
pub mod backend;
pub mod db;
mod lessons;
pub mod mime822;
pub mod oauth_config;

use crate::backend::gmail::wire as gmail;
use accounts::{Account, AccountId, AccountState};
use crate::backend::{Vertical, SidecarKind};
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
    // CancellationTokens for in-flight extract_lessons calls, keyed by the
    // request_id that extract_lessons returns to the frontend at call start.
    // Frontend triggers abort by invoking cancel_extraction(request_id), which
    // cancels the token; extract_lessons's tokio::select! sees that and the
    // provider unwinds (HTTP: drops the in-flight reqwest future; Claude CLI:
    // kills the child process). Entries clean themselves up via a guard in
    // extract_lessons so a normal completion or error path doesn't leak.
    pub in_flight_extracts: Mutex<HashMap<String, tokio_util::sync::CancellationToken>>,
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

/// Construct the backend vertical via dynamic dispatch. Dispatches on the
/// account's `backend_kind`: Gmail accounts fetch a token + label_map and
/// return a `GmailVertical`; LocalFs accounts resolve `root_dir` and return a
/// `LocalFsVertical`. All call sites are independent of the concrete type.
async fn vertical_for(state: &State<'_, AppState>, account_id: &str) -> Result<Box<dyn Vertical>, String> {
    let (kind, root_dir, meta_label) = {
        let list = state.accounts.lock().unwrap();
        let a = list.iter().find(|a| a.id == account_id)
            .ok_or_else(|| format!("account {} not found", account_id))?;
        (a.backend_kind, a.root_dir.clone(), a.effective_meta_label().to_string())
    };
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

    // Slow path: refresh from keychain.
    let rt = accounts::load_refresh_token(account_id)
        .ok_or_else(|| format!("no refresh token in keychain for {}", account_id))?;
    let token_data = auth::refresh_access_token(&rt).await?;
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

// ─── Auth / Add Account ──────────────────────────────────────────────────────

#[tauri::command]
async fn get_auth_url(state: State<'_, AppState>) -> Result<String, String> {
    let pair = auth::PkcePair::generate();
    let url = auth::get_auth_url(&pair);
    *state.pending_pkce.lock().unwrap() = Some(pair);
    Ok(url)
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

    log!("open_auth_url: browser launched, waiting for callback on :8080");
    let app_clone = app.clone();
    tokio::spawn(async move {
        match auth::wait_for_callback().await {
            Ok(cb) => {
                log!("open_auth_url: received auth code (len={})", cb.code.len());
                let state = app_clone.state::<AppState>();
                let pkce = state.pending_pkce.lock().unwrap().take();
                let Some(pkce) = pkce else {
                    log!("open_auth_url: PKCE verifier MISSING");
                    let _ = app_clone.emit("oauth-error", "PKCE verifier missing");
                    return;
                };
                // OAuth `state` CSRF check (RFC 6749 §10.12). Constant-time
                // compare so a timing oracle can't be used to fish out the
                // expected value byte-by-byte.
                if !constant_time_eq(cb.state.as_bytes(), pkce.state.as_bytes()) {
                    log!("open_auth_url: state mismatch — possible CSRF, aborting");
                    let _ = app_clone.emit("oauth-error", "OAuth state mismatch — request rejected");
                    return;
                }
                let token_data = match auth::exchange_code(&cb.code, &pkce.verifier).await {
                    Ok(td) => td,
                    Err(e) => {
                        log!("open_auth_url: token exchange FAILED: {}", e);
                        let _ = app_clone.emit("oauth-error", e);
                        return;
                    }
                };
                log!(
                    "open_auth_url: token exchange OK (refresh_token present={})",
                    token_data.refresh_token.is_some()
                );

                // Look up the user's email so we can persist this account.
                let email = match gmail::get_user_email(&token_data.access_token).await {
                    Ok(e) => e,
                    Err(e) => {
                        log!("open_auth_url: getProfile failed: {}", e);
                        let _ = app_clone.emit("oauth-error", format!("get user profile: {}", e));
                        return;
                    }
                };
                log!("open_auth_url: resolved account email = {}", email);

                // Persist refresh token to keychain under per-account key.
                if let Some(rt) = token_data.refresh_token.as_ref() {
                    if let Err(e) = accounts::save_refresh_token(&email, rt) {
                        log!("open_auth_url: keychain write failed: {}", e);
                    } else {
                        log!("open_auth_url: refresh token saved for {}", email);
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
                            backend_kind: Default::default(), // Gmail
                            root_dir: None,
                        });
                        if let Err(e) = accounts::save_accounts(&list) {
                            log!("open_auth_url: save_accounts failed: {}", e);
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

                log!("open_auth_url: emitting oauth-success");
                let _ = app_clone.emit("oauth-success", email);
            }
            Err(e) => {
                log!("open_auth_url: wait_for_callback FAILED: {}", e);
                let _ = app_clone.emit("oauth-error", e);
            }
        }
    });

    Ok(())
}

// ─── Account management ──────────────────────────────────────────────────────

#[tauri::command]
fn list_accounts(state: State<'_, AppState>) -> Vec<Account> {
    state.accounts.lock().unwrap().clone()
}

#[tauri::command]
async fn remove_account(account_id: String, state: State<'_, AppState>) -> Result<(), String> {
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
// since we last saw it (by comparing remote_version vs the fetched id).
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

/// Reconcile a single fetched note against the cache. Implements the full
/// Phase 4 decision table. Takes the AppState so it can check whether our
/// own sync worker is mid-push for this uuid (which would make a
/// "remote changed" observation a false alarm).
fn reconcile_one(state: &State<'_, AppState>, account_id: &str, fetched: &gmail::Note) {
    let db = &state.db;
    let cached = db::CachedNote::from_remote(account_id, fetched);

    // If our own worker is in the middle of pushing this uuid, the fetched
    // id likely reflects our own in-flight insert — not someone else's
    // edit. Skip reconcile entirely; we'll process this row again on the
    // next list_notes after mark_pushed has updated the cache.
    {
        let pushing = state.pushing.lock().unwrap();
        if pushing.contains(&(account_id.to_string(), cached.uuid.clone())) {
            return;
        }
    }

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
    let remote_changed = existing.remote_version.as_deref() != Some(&fetched.id);

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
                    // Conflict copies don't auto-inherit a tag sidecar
                    // either — copy_tags below populates the local
                    // note_tags rows, and the worker writes a fresh
                    // sidecar for the new uuid on first push.
                    tags_meta_msg_id: None,
                    tags_dirty: true,
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
    // LocalFs: no token/keychain — vertical_for resolves root_dir and
    //          the filesystem vertical handles list_all_notes directly.
    let (result, dedup, v, folder_paths_for_reconcile) = if backend_kind == accounts::BackendKind::Gmail {
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
        // Collect the Notes/* folder paths for folder reconciliation below.
        let folder_paths: Vec<String> = label_map
            .values()
            .filter(|n| *n == "Notes" || n.starts_with("Notes/"))
            .cloned()
            .collect();
        (result, dedup, v_tail, Some(("gmail".to_string(), folder_paths, label_map)))
    } else {
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
        let v2 = vertical_for(&state, &account_id).await?;
        (result, dedup, v2, Some(("localfs".to_string(), fs_folders, HashMap::new())))
    };

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
    if let Some((backend, folder_paths, label_map)) = folder_paths_for_reconcile {
        if backend == "gmail" {
            reconcile_folders_from_labels(&state.db, &account_id, &label_map, true);
        } else {
            reconcile_folders_from_paths(&state.db, &account_id, &folder_paths, true);
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
    state: State<'_, AppState>,
) -> Result<gmail::SavedNote, String> {
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
        db.apply_local_edit(&real_uuid, &account_id, &title, &body_html, &label)
            .map_err(|e| e.to_string())?;
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
            // No tags yet → no sidecar yet. First add_tag flips tags_dirty
            // and the worker materializes the sidecar.
            tags_meta_msg_id: None,
            tags_dirty: false,
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
        db.mark_pushed(&real_uuid, &account_id, &saved.id, &saved.date, &saved.body_html)
            .map_err(|e| e.to_string())?;
        log!(
            "save_note (localfs-sync): uuid={} id={}",
            real_uuid, saved.id
        );
        return Ok(gmail::SavedNote {
            id: saved.id,
            uuid: cached.uuid,
            date: saved.date,
            body_html: saved.body_html,
        });
    }

    log!(
        "save_note (local-first): uuid={} sync_state={:?} id={}",
        real_uuid, cached.sync_state, if cached.id.is_empty() { "<pending>" } else { &cached.id }
    );

    Ok(gmail::SavedNote {
        id: cached.id,
        uuid: cached.uuid,
        date: cached.date,
        body_html: cached.body_html,
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
    let mut out: Vec<gmail::TrashedNote> = by_uuid.into_values().collect();
    out.sort_by(|a, b| rfc2822_ms(&b.date).cmp(&rfc2822_ms(&a.date)));
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
        let v = vertical_from_parts(&state, &account_id, token, label_map.clone())?;
        v.untrash(&id).await.map_err(|e| e.to_string())?;
        // Optionally move to a chosen folder (like a restore-as-move): remove the
        // original Notes label, add the target. Both already exist — the picker
        // only offers existing folders.
        if let Some(target) = target_label.filter(|t| *t != original_label) {
            let id_of = |name: &str| {
                label_map
                    .iter()
                    .find(|(_, n)| n.as_str() == name)
                    .map(|(lid, _)| lid.clone())
            };
            if let Some(target_id) = id_of(&target) {
                let remove: Vec<String> = id_of(&original_label).into_iter().collect();
                v.move_note(&id, &[target_id], &remove).await.map_err(|e| e.to_string())?;
            }
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
        .search_notes(account_id.as_deref(), label.as_deref(), &query)
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
    let norm = normalize_tag(&tag).ok_or_else(|| format!("Invalid tag: {:?}", tag))?;
    state.db.add_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    // Mark the note's tag set as needing a sidecar push so the worker
    // propagates this change to other Jodd instances signed into the
    // same Gmail account. Best-effort: the tag itself is already
    // persisted; a missed dirty flip just delays cross-instance sync.
    let _ = state.db.set_tags_dirty(&account_id, &uuid);
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
    let norm = normalize_tag(&tag).unwrap_or_else(|| tag.clone());
    state.db.remove_tag(&account_id, &uuid, &norm).map_err(|e| e.to_string())?;
    let _ = state.db.set_tags_dirty(&account_id, &uuid);
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
        .list_notes_with_tags(account_id.as_deref(), &norm)
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

fn validate_folder_segment(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if trimmed.contains('/') {
        return Err("Folder name cannot contain '/'".to_string());
    }
    if trimmed == "." || trimmed == ".." {
        return Err("Folder name cannot be '.' or '..'".to_string());
    }
    if trimmed.len() > 200 {
        return Err("Folder name is too long".to_string());
    }
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
            match v.remove_sidecar(&s.id).await {
                Ok(()) => {
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

/// Pull tag state from this account's meta_label. Same shape as
/// `sync_pin_state` with one key difference: tag sidecars carry a JSON
/// body (the tag list), so list_tag_sidecars fetches FULL_CONTENT — a
/// few hundred bytes per sidecar — instead of metadata-only headers.
///
/// Triggered from App.svelte cold start in parallel with sync_pin_state.
/// Returns the count of (apply + clear) for logging; non-zero means the
/// frontend should re-call list_note_tags / list_tags to repaint chips
/// and the sidebar cloud.
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
/// For LocalFs accounts: builds the vertical directly (no token/label_map
/// needed) and calls list_index. The Gmail-only `reconcile_folders_from_labels`
/// step is skipped — LocalFs folder discovery happens entirely through the
/// vertical's list_index + normal reconcile paths.
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
    if backend_kind == accounts::BackendKind::Gmail {
        let token = ensure_token(&state, &account_id).await?;
        let label_map = cached_label_map(&state, &account_id, &token).await?;
        // Populate the folders cache from the full remote label set so EMPTY
        // folders (no notes) show in the sidebar on cold start. list_notes — the
        // only other folder-sync path — is not called on cold start, so without
        // this an empty label like `Notes/play2` stayed invisible until the user
        // navigated. Upsert-only: cold start adds folders but defers removal to
        // the authoritative list_notes pull.
        reconcile_folders_from_labels(&state.db, &account_id, &label_map, false);
        let v = vertical_from_parts(&state, &account_id, token, label_map)?;
        return v.list_index().await.map_err(|e| e.to_string());
    }
    // LocalFs: no token/label_map needed — vertical_for resolves root_dir from
    // the account config. Populate the folders cache from the filesystem so
    // EMPTY sub-folders appear in the sidebar on cold start, mirroring the
    // Gmail reconcile_folders_from_labels(…, false) call above.
    // prune=false on cold start: we don't remove folders on a possibly-partial
    // view; pruning happens on the authoritative list_notes pass.
    let v = vertical_for(&state, &account_id).await?;
    let fs_folders: Vec<String> = v.list_folders().await
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.path)
        .collect();
    reconcile_folders_from_paths(&state.db, &account_id, &fs_folders, false);
    v.list_index().await.map_err(|e| e.to_string())
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
    // Orphan cleanup is Gmail-specific (dup messages, label ids). For LocalFs
    // the filesystem has one file per uuid — there are no orphans.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if a.backend_kind == accounts::BackendKind::LocalFs {
                log!("safe_cleanup_orphans_for_account: skipping LocalFs account {}", account_id);
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
    // Orphan preview is Gmail-specific. For LocalFs return empty — no dups possible.
    {
        let list = state.accounts.lock().unwrap();
        if let Some(a) = list.iter().find(|a| a.id == account_id) {
            if a.backend_kind == accounts::BackendKind::LocalFs {
                log!("preview_orphans: skipping LocalFs account {}", account_id);
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

const SYNC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

async fn push_one_dirty(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let existing_gmail_id = if n.id.is_empty() { None } else { Some(n.id.as_str()) };
    let existing_uuid = Some(n.uuid.as_str());
    let existing_x_mail = n.x_mail_created_date.as_deref();
    // Load this note's stored attachments so save_note can re-emit any the body
    // still references (multipart/related) instead of stripping them.
    let attachments = state
        .db
        .list_attachments(&n.account_id, &n.uuid)
        .unwrap_or_default();
    let v = vertical_for(state, &n.account_id).await?;
    let op = backend::SaveOp {
        title: &n.title, body_html: &n.body_html,
        existing_remote_id: existing_gmail_id, existing_uuid,
        existing_created_date: existing_x_mail, label: &n.label,
    };
    let saved = v.save_note_full(&op, &attachments).await.map_err(|e| e.to_string())?;
    state.db.mark_pushed(
        &n.uuid,
        &n.account_id,
        &saved.id,
        &saved.date,
        &saved.body_html,
    ).map_err(|e| e.to_string())?;
    Ok(())
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
    // Best-effort trash of any sidecars in Notes-Meta. Without these,
    // deleted notes leave orphan metadata messages that accumulate over
    // time. Either failure is logged but doesn't fail the deletion — the
    // user's intent ("remove this note") is more important than sidecar
    // hygiene, and the next sync_pin_state / sync_tag_state pass will
    // notice the orphans (they have no matching note locally) and the
    // user can clean them up via the dup-cleanup flow.
    if let Some(pin_sidecar) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = v.remove_sidecar(pin_sidecar).await {
            log!("push_one_deletion: trash pin sidecar {} failed: {}", pin_sidecar, e);
        }
    }
    if let Some(tag_sidecar) = n.tags_meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = v.remove_sidecar(tag_sidecar).await {
            log!("push_one_deletion: trash tag sidecar {} failed: {}", tag_sidecar, e);
        }
    }
    state.db.delete(&n.uuid, &n.account_id).map_err(|e| e.to_string())?;
    Ok(())
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
/// worker re-pushes next tick).
async fn push_one_pin(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let v = vertical_for(state, &n.account_id).await?;

    let new_meta_id: Option<String> = if n.pinned {
        let payload = serde_json::json!({ "pinned": true }).to_string();
        let id = v.put_sidecar(&n.uuid, SidecarKind::Pin, Some(payload.as_bytes()), n.meta_msg_id.as_deref()).await.map_err(|e| e.to_string())?;
        Some(id)
    } else {
        if let Some(old) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Best-effort trash. If the sidecar was already trashed by
            // another Jodd instance we still want to clear meta_msg_id
            // locally — mark_pin_pushed runs regardless.
            if let Err(e) = v.remove_sidecar(old).await {
                log!("push_one_pin: trash sidecar {} failed: {}", old, e);
            }
        }
        None
    };
    let _ = state.db.mark_pin_pushed(
        &n.uuid,
        &n.account_id,
        new_meta_id.as_deref(),
        n.pinned,
    ).map_err(|e| e.to_string())?;
    Ok(())
}

/// Push one note's tag sidecar to Gmail. Mirrors `push_one_pin` shape but
/// the payload is the full tag set (sorted, normalized) from `note_tags`.
///
/// Behaviour:
///   - tags non-empty → save_tag_sidecar with `{"tags":[…]}` body,
///                      trash the previous sidecar if `tags_meta_msg_id`
///                      was set, store the new sidecar id
///   - tags empty     → trash the existing sidecar (if any), clear
///                      `tags_meta_msg_id`. An empty-tags note doesn't
///                      need a sidecar — absence carries the same meaning.
///
/// Same insert-then-trash sequence as save_note: keep the previous
/// version reachable until the new one is committed on Gmail's side,
/// to avoid losing tag state if the trash succeeds but the insert fails.
async fn push_one_tag_set(
    state: &State<'_, AppState>,
    n: &db::CachedNote,
) -> Result<(), String> {
    let v = vertical_for(state, &n.account_id).await?;

    // Snapshot the current tag set from SQLite. list_tags_for returns
    // them sorted alphabetically so the JSON payload is deterministic.
    let tags = state.db.list_tags_for(&n.account_id, &n.uuid)
        .map_err(|e| e.to_string())?;

    let new_meta_id: Option<String> = if !tags.is_empty() {
        let payload = serde_json::json!({ "tags": tags }).to_string();
        let id = v.put_sidecar(&n.uuid, SidecarKind::Tags, Some(payload.as_bytes()), n.tags_meta_msg_id.as_deref()).await.map_err(|e| e.to_string())?;
        Some(id)
    } else {
        if let Some(old) = n.tags_meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            if let Err(e) = v.remove_sidecar(old).await {
                log!("push_one_tag_set: trash sidecar {} failed: {}", old, e);
            }
        }
        None
    };
    let _ = state.db.mark_tags_pushed(
        &n.uuid,
        &n.account_id,
        new_meta_id.as_deref(),
    ).map_err(|e| e.to_string())?;
    Ok(())
}

async fn push_one_folder(
    state: &State<'_, AppState>,
    f: &db::CachedFolder,
) -> Result<(), String> {
    use db::FolderSyncState::*;
    let v = vertical_for(state, &f.account_id).await?;
    match f.sync_state {
        DirtyNew => {
            // Create on Gmail. Returns the new label_id.
            let folder = v.create_folder(&f.path).await.map_err(|e| e.to_string())?;
            state.db.mark_folder_created(&f.account_id, &f.path, &folder.id)
                .map_err(|e| e.to_string())?;
            // Invalidate label_map cache so subsequent note saves see the
            // new label (they look up label_id by path in that map).
            invalidate_label_cache(state, &f.account_id);
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

async fn sync_worker_tick(app: &AppHandle) {
    let state = app.state::<AppState>();

    // Snapshot live accounts at the top of the tick. Dirty/deletion rows
    // for an account that no longer exists (signed out mid-cycle) would
    // otherwise generate ~5 errors per tick until the next index sweep —
    // and would burn refresh-token lookups against the keychain for
    // accounts the user has explicitly removed. Skip them silently.
    let live_accts: std::collections::HashSet<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
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

    // Drain tag sidecars — same priority logic as pin: orthogonal to
    // content/delete pushes, runs last because it's UX-only. A row can
    // be tags_dirty AND content-dirty AND pin-dirty simultaneously and
    // each path runs independently this tick.
    // Tags are inline #hashtags in the body now, so they ride along with the
    // normal content push and round-trip to Apple — the frontend no longer
    // marks notes tags_dirty, so this legacy sidecar drain finds nothing. Kept
    // so any historical tags_dirty row still flushes once, then stays quiet.
    let dirty_tags = state.db.list_tags_dirty().unwrap_or_default();
    for n in dirty_tags {
        if !live_accts.contains(&n.account_id) {
            continue;
        }
        if let Err(e) = push_one_tag_set(&state, &n).await {
            log!("sync_worker: push tags uuid={} failed: {}", n.uuid, e);
        }
    }
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
async fn extract_lessons(
    account_id: String,
    source_text: String,
    title_override: Option<String>,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    log!(
        "extract_lessons: account={} source_len={} request_id={}",
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
    let provider = crate::lessons::resolve::resolve_provider(&account).map_err(|e| {
        state.in_flight_extracts.lock().unwrap().remove(&request_id);
        e.to_string()
    })?;

    // Call LLM. The provider races its I/O against `cancel` and returns
    // ExtractError::Cancelled if the token fires.
    let envelope = match provider.extract(&source_text, cancel).await {
        Ok(env) => env,
        Err(crate::lessons::provider::ExtractError::Cancelled) => {
            // User-initiated abort: discard everything. Do NOT create a
            // fallback note — the user actively chose to cancel, not "lose"
            // their paste. The textarea still holds the source if they want
            // to retry.
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            log!("extract_lessons: cancelled by user (request_id={request_id})");
            return Err("cancelled".to_string());
        }
        Err(e) => {
            log!("extract_lessons: provider error {e:?} — creating fallback note");
            let uuid = create_fallback_source_note(&state, &account_id, &source_text)?;
            state.in_flight_extracts.lock().unwrap().remove(&request_id);
            return Err(format!(
                "LLM call failed; source preserved in note {uuid}. {e}"
            ));
        }
    };

    // Assemble note body.
    let body_html = crate::lessons::markdown::assemble_note_body(&envelope, &source_text);

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
        tags_meta_msg_id: None,
        tags_dirty: false,
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
        "extract_lessons: created note uuid={uuid} in {folder} with {} body-derived tag(s)",
        envelope.tags.len()
    );
    Ok(uuid)
}

/// Cancel an in-flight extract_lessons call. The frontend passes the same
/// request_id it gave to extract_lessons; we look up the CancellationToken
/// and fire it. The extract_lessons command's tokio::select! sees the token
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
async fn re_extract_lessons(
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

    let source = crate::lessons::markdown::extract_source(&note.body_html)
        .ok_or_else(|| "note has no Source section to re-extract from".to_string())?;

    extract_lessons(account_id, source, None, request_id, state).await
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

#[tauri::command]
fn save_oauth_config(client_id: String, client_secret: String) -> Result<(), String> {
    let id = client_id.trim().to_string();
    if id.is_empty() {
        oauth_config::clear()?;
        oauth_config::clear_secret()?;
    } else {
        oauth_config::save(&id)?;
        if !client_secret.trim().is_empty() {
            oauth_config::save_secret(client_secret.trim())?;
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
        crate::lessons::markdown::escape_html(source)
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
        tags_meta_msg_id: None,
        tags_dirty: false,
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

    let env_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".env");
    dotenv::from_path(&env_path).ok();
    dotenv::dotenv().ok();

    if std::env::var("GOOGLE_CLIENT_ID")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        log!(
            "WARNING: GOOGLE_CLIENT_ID not set. Tried .env at: {}",
            env_path.display()
        );
    } else {
        log!("OAuth credentials loaded from {}", env_path.display());
    }

    let accounts_list = accounts::load_accounts();
    log!("loaded {} account(s) from persistence", accounts_list.len());

    // Open the local SQLite replica. Lives in the platform's per-user app
    // data dir — never in the binary's working dir, so reinstalls don't
    // wipe the cache. Falls back to the temp dir as a last resort so we
    // never crash on startup (the cache being volatile is preferable to
    // the app refusing to launch).
    let data_dir = dirs::data_dir()
        .map(|d| d.join("jodd"))
        .unwrap_or_else(|| std::env::temp_dir().join("jodd"));
    let db = match db::Db::open(&data_dir) {
        Ok(d) => {
            log!("local cache opened at {}", data_dir.display());
            Arc::new(d)
        }
        Err(e) => {
            log!("FATAL: failed to open local cache: {} — using temp dir", e);
            let tmp = std::env::temp_dir().join("jodd");
            Arc::new(db::Db::open(&tmp).expect("temp-dir DB open"))
        }
    };

    let app_state = AppState {
        accounts: Mutex::new(accounts_list),
        account_states: Mutex::new(HashMap::new()),
        pending_pkce: Mutex::new(None),
        db,
        pushing: Mutex::new(std::collections::HashSet::new()),
        dup_stats: Mutex::new(HashMap::new()),
        in_flight_extracts: Mutex::new(HashMap::new()),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(app_state)
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                migrate_legacy_keychain(&state).await;
            });
            // Start the background sync worker. Runs for the lifetime of
            // the app, polling SQLite for dirty/deleted_pending rows and
            // pushing them to Gmail.
            spawn_sync_worker(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_auth_url,
            open_auth_url,
            is_authenticated,
            list_accounts,
            remove_account,
            get_account_settings,
            update_account_settings,
            add_local_account,
            rename_local_account,
            list_notes,
            list_notes_in_folder,
            list_cached_notes_in_folder,
            refetch_note,
            get_trashed_note_preview,
            list_cached_notes,
            index_account,
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
            create_folder,
            rename_folder,
            delete_folder,
            move_folder,
            cleanup_orphans,
            get_dup_stats,
            preview_orphans,
            trash_specific_messages,
            extract_lessons,
            re_extract_lessons,
            cancel_extraction,
            get_llm_settings,
            update_llm_settings,
            get_oauth_config,
            save_oauth_config,
            clear_oauth_config,
            get_log_settings,
            set_file_logging_enabled,
            clear_log_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
