pub mod accounts;
pub mod auth;
pub mod db;
pub mod gmail;

use accounts::{Account, AccountId, AccountState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};

// Timestamped log: prints `[jodd HH:MM:SS.mmm] ...` to stderr.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!(
            "[jodd {}] {}",
            chrono::Local::now().format("%H:%M:%S%.3f"),
            format_args!($($arg)*)
        )
    };
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
}

const LABEL_MAP_TTL: std::time::Duration = std::time::Duration::from_secs(300);

// ─── Account helpers ─────────────────────────────────────────────────────────

// Computes the deadline for a freshly-issued access token. We subtract a safety
// margin so refresh fires BEFORE the actual expiry — covers clock skew and the
// time it takes the refresh round-trip to complete.
fn token_deadline_from_expires_in(expires_in: Option<i64>) -> std::time::Instant {
    let secs = expires_in.unwrap_or(3600).max(60) as u64;
    let safety_margin = 60u64.min(secs / 2);
    std::time::Instant::now() + std::time::Duration::from_secs(secs - safety_margin)
}

// Ensures the AccountState for account_id has a valid access_token, refreshing
// from the keychain-stored refresh token if expired or missing.
async fn ensure_token(
    state: &State<'_, AppState>,
    account_id: &str,
) -> Result<String, String> {
    // Fast path: in-memory token, still fresh.
    {
        let states = state.account_states.lock().unwrap();
        if let Some(s) = states.get(account_id) {
            if let (Some(t), Some(exp)) = (s.access_token.as_ref(), s.token_expires_at) {
                if exp > std::time::Instant::now() {
                    return Ok(t.clone());
                }
                log!("ensure_token: {} access token expired, refreshing", account_id);
            } else if s.access_token.is_some() {
                // Have a token but no expiry tracked (e.g. from legacy migration).
                // Treat as unknown freshness — refresh defensively.
                log!("ensure_token: {} has token but no expiry — refreshing", account_id);
            }
        }
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
    let fresh = gmail::get_label_map(token).await?;
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
            Ok(code) => {
                log!("open_auth_url: received auth code (len={})", code.len());
                let state = app_clone.state::<AppState>();
                let pkce = state.pending_pkce.lock().unwrap().take();
                let Some(pkce) = pkce else {
                    log!("open_auth_url: PKCE verifier MISSING");
                    let _ = app_clone.emit("oauth-error", "PKCE verifier missing");
                    return;
                };
                let token_data = match auth::exchange_code(&code, &pkce.verifier).await {
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

#[tauri::command]
async fn is_authenticated(state: State<'_, AppState>) -> Result<bool, String> {
    // For multi-account: "authenticated" means at least one usable account exists.
    let ids: Vec<String> = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .map(|a| a.id.clone())
        .collect();
    if ids.is_empty() {
        log!("is_authenticated: no accounts in store → false");
        return Ok(false);
    }
    // Verify at least one account's refresh token is still good.
    for id in &ids {
        match ensure_token(&state, id).await {
            Ok(_) => {
                log!("is_authenticated: {} has valid token → true", id);
                return Ok(true);
            }
            Err(e) => {
                log!("is_authenticated: {} token failed: {}", id, e);
            }
        }
    }
    log!("is_authenticated: no accounts have valid tokens → false");
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
                let new_uuid = gmail::format_apple_uuid(uuid::Uuid::new_v4());
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
    let token = ensure_token(&state, &account_id).await?;

    let label_map = cached_label_map(&state, &account_id, &token).await?;
    let cache_map = cache_by_msg_id(&state, &account_id);
    let (mut result, mut dedup) = gmail::list_notes(&token, &label_map, &cache_map).await?;

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
                let (notes, fresh_dedup) = gmail::list_notes(&token, &fresh, &cache_map).await?;
                result = notes;
                dedup = fresh_dedup;
            }
        }
    }

    // Surface the dedup summary so the sidebar can show a passive "N dup"
    // indicator. Replace (not accumulate) — each list_notes call is a
    // complete observation; after cleanup_orphans runs the next call will
    // report fewer duplicates.
    state.dup_stats.lock().unwrap().insert(account_id.clone(), dedup);

    // Tag each note with its account so the frontend can scope folder views.
    for n in &mut result {
        n.account_id = Some(account_id.clone());
    }

    // Reconcile each fetched note against the cache. reconcile_one handles
    // the full state machine: insert fresh on unknown uuid, refresh clean,
    // detect conflicts when both sides changed, leave deletion-pending
    // alone, etc. See reconcile_one comments for the full decision table.
    //
    // After the per-row pass, prune clean cache rows whose uuid didn't
    // come back from Gmail — those notes are gone on remote. Only safe
    // here (full sweep), not in list_notes_in_folder (scoped fetch).
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
    // Resolve this account's meta_label, ensure it exists on Gmail
    // (silent no-op if it doesn't yet — first sidecar push lazily
    // creates it), list every sidecar, and apply each to the cache via
    // apply_remote_pin. Then clear pin on any locally-pinned row whose
    // uuid didn't appear in the listing (the sidecar was trashed
    // remotely → another Jodd instance unpinned it).
    //
    // Skipped silently on errors so a transient Gmail glitch on the
    // meta_label doesn't break the entire list_notes path — pin sync
    // is UX-only, not correctness-critical.
    {
        let meta_label = {
            let list = state.accounts.lock().unwrap();
            list.iter()
                .find(|a| a.id == account_id)
                .map(|a| a.effective_meta_label().to_string())
        };
        if let Some(meta_label) = meta_label {
            if let Some((meta_id, _)) = label_map.iter().find(|(_, n)| n.as_str() == meta_label) {
                match gmail::list_meta_sidecars(&token, meta_id).await {
                    Ok(sidecars) => {
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
                    Err(e) => log!("list_notes: list_meta_sidecars failed: {}", e),
                }
            }
            // If meta_label isn't in label_map yet, no sidecars can
            // possibly exist — skip silently. The first pin push will
            // ensure_label and the next list_notes will pick it up.
        }
    }

    // ── Folder pull reconciliation ─────────────────────────────────────
    // list_notes' label_map is authoritative for the remote folder set —
    // it lists every label visible to Gmail for this account, filtered
    // to the Notes/* tree. Upsert each into the local folders cache as
    // Clean (skipping rows in pending states), then prune any clean rows
    // whose path isn't in the remote list (folder deleted externally).
    reconcile_folders_from_labels(&state.db, &account_id, &label_map, true);

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
            gmail::canonicalize_uuid(u).unwrap_or_else(|| u.to_string())
        }
        _ => gmail::format_apple_uuid(uuid::Uuid::new_v4()),
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
        let token = ensure_token(&state, &account_id).await?;
        gmail::delete_note(&token, id).await?;
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
    account_id: String,
    tags: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let norm: Vec<String> = tags
        .iter()
        .filter_map(|t| normalize_tag(t))
        .collect();
    let cached = state.db
        .list_notes_with_tags(&account_id, &norm)
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
    // Capture the affected uuids BEFORE rename — set_all_tags_dirty joins
    // through note_tags WHERE tag = from, which only matches before the
    // rename mutates those rows. Marking dirty first means we may over-
    // mark if rename then fails, but the worker push of an unchanged set
    // is harmless (it writes the same sidecar body).
    let _ = state.db.set_all_tags_dirty(&account_id, &from);
    state.db.rename_tag(&account_id, &from, &to).map_err(|e| e.to_string())?;
    log!("rename_tag: account={} '{}' -> '{}'", account_id, from, to);
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
    // Same ordering rule as rename_tag: capture uuids BEFORE delete.
    let _ = state.db.set_all_tags_dirty(&account_id, &t);
    state.db.delete_tag(&account_id, &t).map_err(|e| e.to_string())?;
    log!("delete_tag: account={} tag={}", account_id, t);
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
fn validate_folder_segment(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Folder name cannot be empty".to_string());
    }
    if trimmed.contains('/') {
        return Err("Folder name cannot contain '/'".to_string());
    }
    if trimmed.len() > 200 {
        return Err("Folder name is too long".to_string());
    }
    Ok(trimmed.to_string())
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
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    let label_id = match label_map
        .iter()
        .find_map(|(id, name)| if name == &path { Some(id.clone()) } else { None })
    {
        Some(id) => id,
        None => {
            // Folder isn't on Gmail yet. If we have a local row, this is a
            // local-first folder whose Gmail label hasn't been pushed by the
            // sync worker — it's empty by definition (can't add notes to a
            // label that doesn't exist on Gmail), so return an empty list
            // instead of an error. Only error if the folder isn't local
            // either, which would be an actual "doesn't exist" condition.
            if let Ok(Some(_)) = state.db.get_folder(&account_id, &path) {
                log!(
                    "list_notes_in_folder: '{}' exists locally but not on Gmail yet — returning empty",
                    path
                );
                return Ok(Vec::new());
            }
            return Err(format!("Folder not found: {}", path));
        }
    };
    let cache_map = cache_by_msg_id(&state, &account_id);
    let mut result = gmail::list_notes_in_label(&token, &label_id, &label_map, &cache_map).await?;
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
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    let mut note = gmail::fetch_note(&token, &id, &label_map).await?;
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
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("Account not found: {}", account_id))?
    };
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    let Some((meta_id, _)) = label_map.iter().find(|(_, n)| n.as_str() == meta_label) else {
        // meta_label isn't on Gmail yet — no sidecars can exist. Not an
        // error; first pin push will ensure_label and the next sync_pin_state
        // call will find it.
        log!("sync_pin_state: meta_label '{}' not on Gmail yet for {}", meta_label, account_id);
        return Ok(0);
    };
    let sidecars = gmail::list_meta_sidecars(&token, meta_id).await?;
    let mut applied = 0usize;
    let mut keep: Vec<String> = Vec::with_capacity(sidecars.len());
    for s in &sidecars {
        let n = state.db.apply_remote_pin(&s.note_uuid, &account_id, true, &s.id)
            .unwrap_or(0);
        applied += n;
        keep.push(s.note_uuid.clone());
    }
    let cleared = state.db.clear_pins_not_in(&account_id, &keep).unwrap_or(0);
    log!(
        "sync_pin_state: account={} sidecars={} applied={} cleared={}",
        account_id, sidecars.len(), applied, cleared
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
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("Account not found: {}", account_id))?
    };
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    let Some((meta_id, _)) = label_map.iter().find(|(_, n)| n.as_str() == meta_label) else {
        // meta_label doesn't exist yet → no sidecars possible. Same
        // benign signal as sync_pin_state.
        log!("sync_tag_state: meta_label '{}' not on Gmail yet for {}", meta_label, account_id);
        return Ok(0);
    };
    let sidecars = gmail::list_tag_sidecars(&token, meta_id).await?;
    let mut applied = 0usize;
    let mut keep: Vec<String> = Vec::with_capacity(sidecars.len());
    for s in &sidecars {
        // apply_remote_tags is local-wins-guarded; a uuid that's tags_dirty
        // will be skipped (push will write the local value next worker tick).
        let n = state.db.apply_remote_tags(&account_id, &s.note_uuid, &s.tags, &s.id)
            .unwrap_or(0);
        applied += n;
        keep.push(s.note_uuid.clone());
    }
    let cleared = state.db.clear_tags_not_in(&account_id, &keep).unwrap_or(0);
    log!(
        "sync_tag_state: account={} sidecars={} applied={} cleared={}",
        account_id, sidecars.len(), applied, cleared
    );
    Ok(applied + cleared)
}

/// Cheap account-wide index — every Notes message's id + label, no body
/// fetch. Returns in seconds even for a 6k mailbox. The frontend uses this
/// to render folder counts and a "loaded X of Y" indicator before bodies
/// arrive. Bodies are hydrated on-demand by `list_notes_in_folder` / full
/// `list_notes` calls — both already cache-aware (Phase B), so this index
/// pass costs nothing the next time around.
#[tauri::command]
async fn index_account(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::MessageIndex>, String> {
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    // Populate the folders cache from the full remote label set so EMPTY
    // folders (no notes) show in the sidebar on cold start. list_notes — the
    // only other folder-sync path — is not called on cold start, so without
    // this an empty label like `Notes/play2` stayed invisible until the user
    // navigated. Upsert-only: cold start adds folders but defers removal to
    // the authoritative list_notes pull.
    reconcile_folders_from_labels(&state.db, &account_id, &label_map, false);
    gmail::list_account_index(&token, &label_map).await
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
    let token = ensure_token(state, account_id).await?;
    let label_map = cached_label_map(state, account_id, &token).await?;

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

        let found = match gmail::find_gmail_ids_for_uuid(&token, &note.uuid, &label_map).await {
            Ok(v) => v,
            Err(e) => {
                log!("cleanup_orphans: find failed for uuid={}: {}", note.uuid, e);
                continue;
            }
        };
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
            match gmail::delete_note(&token, &gmail_id).await {
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
    log!("preview_orphans: starting for account {}", account_id);
    let token = ensure_token(&state, &account_id).await?;
    let label_map = cached_label_map(&state, &account_id, &token).await?;
    log!("preview_orphans: token + label_map ready ({} labels)", label_map.len());
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
        let ids = match gmail::find_gmail_ids_for_uuid(&token, &note.uuid, &label_map).await {
            Ok(v) => v,
            Err(_) => continue,
        };
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
            match gmail::fetch_note(&token, &id, &label_map).await {
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
    let token = ensure_token(&state, &account_id).await?;

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
        match gmail::delete_note(&token, &id).await {
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
    let token = ensure_token(state, &n.account_id).await?;
    let label_map = cached_label_map(state, &n.account_id, &token).await?;
    let existing_gmail_id = if n.id.is_empty() { None } else { Some(n.id.as_str()) };
    let existing_uuid = Some(n.uuid.as_str());
    let existing_x_mail = n.x_mail_created_date.as_deref();
    let saved = gmail::save_note(
        &token,
        &n.title,
        &n.body_html,
        existing_gmail_id,
        existing_uuid,
        existing_x_mail,
        &n.label,
        &n.account_id,
        &label_map,
    ).await?;
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
    let token = ensure_token(state, &n.account_id).await?;
    gmail::delete_note(&token, &n.id).await?;
    // Best-effort trash of any sidecars in Notes-Meta. Without these,
    // deleted notes leave orphan metadata messages that accumulate over
    // time. Either failure is logged but doesn't fail the deletion — the
    // user's intent ("remove this note") is more important than sidecar
    // hygiene, and the next sync_pin_state / sync_tag_state pass will
    // notice the orphans (they have no matching note locally) and the
    // user can clean them up via the dup-cleanup flow.
    if let Some(pin_sidecar) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = gmail::trash_meta_sidecar(&token, pin_sidecar).await {
            log!("push_one_deletion: trash pin sidecar {} failed: {}", pin_sidecar, e);
        }
    }
    if let Some(tag_sidecar) = n.tags_meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
        if let Err(e) = gmail::trash_meta_sidecar(&token, tag_sidecar).await {
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
    // Resolve meta_label for this account. Snapshot to avoid holding the
    // accounts Mutex across the network call.
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == n.account_id)
            .map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("account {} vanished mid-push", n.account_id))?
    };
    let token = ensure_token(state, &n.account_id).await?;
    let label_map = cached_label_map(state, &n.account_id, &token).await?;
    let meta_label_id = gmail::ensure_label(&token, &meta_label, &label_map).await?;
    // ensure_label may have created the label; invalidate the cache so
    // subsequent push_one_pin / list_notes see the fresh id mapping.
    invalidate_label_cache(state, &n.account_id);

    let new_meta_id: Option<String> = if n.pinned {
        let payload = serde_json::json!({ "pinned": true }).to_string();
        let id = gmail::save_meta_sidecar(
            &token,
            &n.uuid,
            &payload,
            &meta_label_id,
            n.meta_msg_id.as_deref(),
            &n.account_id,
        ).await?;
        Some(id)
    } else {
        if let Some(old) = n.meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Best-effort trash. If the sidecar was already trashed by
            // another Jodd instance we still want to clear meta_msg_id
            // locally — mark_pin_pushed runs regardless.
            if let Err(e) = gmail::trash_meta_sidecar(&token, old).await {
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
    let meta_label = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == n.account_id)
            .map(|a| a.effective_meta_label().to_string())
            .ok_or_else(|| format!("account {} vanished mid-push", n.account_id))?
    };
    let token = ensure_token(state, &n.account_id).await?;
    let label_map = cached_label_map(state, &n.account_id, &token).await?;
    let meta_label_id = gmail::ensure_label(&token, &meta_label, &label_map).await?;
    invalidate_label_cache(state, &n.account_id);

    // Snapshot the current tag set from SQLite. list_tags_for returns
    // them sorted alphabetically so the JSON payload is deterministic.
    let tags = state.db.list_tags_for(&n.account_id, &n.uuid)
        .map_err(|e| e.to_string())?;

    let new_meta_id: Option<String> = if !tags.is_empty() {
        let payload = serde_json::json!({ "tags": tags }).to_string();
        let id = gmail::save_tag_sidecar(
            &token,
            &n.uuid,
            &payload,
            &meta_label_id,
            n.tags_meta_msg_id.as_deref(),
            &n.account_id,
        ).await?;
        Some(id)
    } else {
        if let Some(old) = n.tags_meta_msg_id.as_deref().filter(|s| !s.is_empty()) {
            if let Err(e) = gmail::trash_meta_sidecar(&token, old).await {
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
    let token = ensure_token(state, &f.account_id).await?;
    match f.sync_state {
        DirtyNew => {
            // Create on Gmail. Returns the new label_id.
            let info = gmail::create_label(&token, &f.path).await?;
            state.db.mark_folder_created(&f.account_id, &f.path, &info.id)
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
            gmail::rename_label(&token, label_id, &f.path).await?;
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
            gmail::delete_label(&token, label_id).await?;
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
    let dirty_tags = match state.db.list_tags_dirty() {
        Ok(v) => v,
        Err(e) => { log!("sync_worker: list_tags_dirty failed: {}", e); vec![] }
    };
    for n in dirty_tags {
        if !live_accts.contains(&n.account_id) {
            log!(
                "sync_worker: skipping tags-dirty uuid={} — account {} no longer exists",
                n.uuid, n.account_id
            );
            continue;
        }
        if let Err(e) = push_one_tag_set(&state, &n).await {
            log!("sync_worker: push tags uuid={} failed: {}", n.uuid, e);
        } else {
            log!("sync_worker: pushed tag sidecar uuid={}", n.uuid);
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
    });
    let _ = accounts::save_accounts(&list);

    // Cache the live access token so the user doesn't see a sign-in screen.
    let mut states = state.account_states.lock().unwrap();
    let entry = states.entry(email).or_default();
    entry.access_token = Some(token_data.access_token);
    entry.token_expires_at = Some(token_deadline_from_expires_in(token_data.expires_in));

    log!("migrate: legacy account migrated successfully");
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
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
            list_notes,
            list_notes_in_folder,
            list_cached_notes_in_folder,
            refetch_note,
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
            add_tag,
            remove_tag,
            list_tags,
            list_note_tags,
            list_cached_notes_with_tags,
            rename_tag,
            delete_tag,
            list_folders,
            create_folder,
            rename_folder,
            delete_folder,
            move_folder,
            cleanup_orphans,
            get_dup_stats,
            preview_orphans,
            trash_specific_messages,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
