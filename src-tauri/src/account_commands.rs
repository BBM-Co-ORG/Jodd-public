//! Account management: listing, adding, removing, signing in to iCloud,
//! per-account settings, lifecycle status and the sync-block retries.
//! Moved out of `lib.rs` 2026-11-16; the core account helpers (`vertical_for`,
//! `ensure_token`, `refuse_write`, the lifecycle predicates) stay there.

use super::*;
// ─── Account management ──────────────────────────────────────────────────────

#[tauri::command]
pub fn list_accounts(state: State<'_, AppState>) -> Vec<Account> {
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
pub(crate) async fn perform_account_removal(state: &State<'_, AppState>, account_id: &str, revoke_key: bool) -> Result<(), String> {
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
    // Looked up once, before the row is deleted below — a Managed SSH account
    // whose key directory the revoke step (and the cleanup after it) both
    // need. `None` for every other backend and for an Advanced SSH account
    // (no `ssh_key`), which makes the rest of this function a no-op for them.
    let managed = {
        let list = state.accounts.lock().unwrap();
        list.iter().find(|a| a.id == account_id)
            .filter(|a| a.backend_kind == accounts::BackendKind::Ssh && a.ssh_key.is_some())
            .cloned()
    };
    // The account's ssh process would otherwise live until the app exits.
    backend::ssh::forget_session(account_id);
    // Retire a Managed account's key: best-effort revoke of Jodd's line from
    // the server's authorized_keys (while the key on disk can still
    // authenticate), then delete the local key directory whatever happened.
    //
    // Spawned, not awaited — local-first: removing an account must never wait
    // on the server. An offline laptop or a dead VPS used to hold this command
    // (and the sidebar awaiting it) for ConnectTimeout and more. The account
    // row goes below regardless; the key directory is keyed by a fresh uuid
    // nothing else ever reuses, so running after the row is gone is safe.
    //
    // Who may be revoked is decided inside `retire_managed_key`, not here:
    // `revoke_key` (this call's choice), the keep-key marker (the choice of a
    // removal queued while Draining, which completes from the sync worker with
    // `true`), and C5's imported-key rule all apply on every path.
    if let Some(a) = managed {
        let dir = a.ssh_key.as_deref()
            .and_then(|k| crate::paths::data_base().map(|d| d.join("jodd").join("ssh").join(k)));
        if let (Some(target), Some(dir)) = (a.ssh_target.clone(), dir) {
            let opts = backend::ssh::managed_options(&a);
            let account_id = account_id.to_string();
            tauri::async_runtime::spawn_blocking(move || {
                match backend::ssh::setup::retire_managed_key(&target, &opts, &dir, revoke_key) {
                    backend::ssh::setup::RetireOutcome::Revoked => {
                        log!("remove_account: revoked jodd's key on {target} for {account_id}");
                    }
                    backend::ssh::setup::RetireOutcome::Skipped(why) => {
                        log!("remove_account: kept jodd's key on the server for {account_id} — {why}");
                    }
                    backend::ssh::setup::RetireOutcome::Failed(e) => {
                        log!("remove_account: could not revoke jodd's key on the server for {account_id} ({e}) — it remains under a 'jodd' comment in ~/.ssh/authorized_keys");
                    }
                }
            });
        }
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
pub async fn remove_account(
    account_id: String,
    revoke_key: Option<bool>,
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
            record_keep_key_choice(&list, &account_id, revoke_key)?;
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
        record_keep_key_choice(&list, &account_id, revoke_key)?;
        queue_removal(&mut list, &account_id);
        accounts::save_accounts(&list)?;
        return Ok(RemoveAccountOutcome::Queued);
    }
    perform_account_removal(&state, &account_id, revoke_key.unwrap_or(true)).await?;
    Ok(RemoveAccountOutcome::Removed)
}

/// Return the user-facing settings for one account. Resolves the
/// Option<String> fields in `Account` to concrete strings — the frontend
/// sees the effective label names, not the "unset = use default" rule.
#[tauri::command]
pub fn get_account_settings(
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
    /// What counts as a tag here — the editor's chip row reads it (gotcha #38).
    pub tag_model: backend::TagModel,
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
pub fn backend_capabilities(
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
    Ok(CapabilitiesDto {
        has_trash: caps.has_trash,
        writes: caps.writes,
        tag_model: backend::TagModel::for_backend(kind),
    })
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
pub(crate) fn refuse_unsafe_label_change(
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
pub async fn update_account_settings(
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
pub(crate) fn localfs_account_id() -> accounts::AccountId {
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
#[tauri::command]
pub async fn icloud_sign_in(state: State<'_, AppState>) -> Result<String, String> {
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
                    ssh_target: None,
                    ssh_port: None,
                    ssh_key: None,
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
pub async fn icloud_reauthenticate(account_id: String, state: State<'_, AppState>) -> Result<String, String> {
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
pub async fn add_local_account(
    path: String,
    name: Option<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<accounts::Account, String> {
    let p = std::path::Path::new(&path);
    // On Android the access check runs FIRST (`check_android_vault`): without
    // All files access the folder may not even look like a directory, and the
    // user must be told to turn access on, not that it is "not a directory".
    #[cfg(target_os = "android")]
    {
        let granted = crate::android_storage::has_all_files_access(&app).await?;
        crate::android_storage::check_android_vault(p, granted)?;
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = &app;
        if !p.is_dir() {
            return Err(format!("not a directory: {}", path));
        }
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
        ssh_target: None,
        ssh_port: None,
        ssh_key: None,
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
pub async fn rename_local_account(
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

/// Re-arm one note whose push was refused permanently, so the worker picks it
/// up on its next tick. Returns whether the note was blocked.
///
/// The user-driven half of the block's lifecycle. A block is not a verdict on
/// the note, it is a verdict on one attempt against one backend state — the
/// Notes folder gains an id, an account is re-authorised, the note is changed
/// in Apple Notes — and most of those produce no event Jodd can hook, so the
/// user pressing "Try again" is the signal. Cheap and safe to spam: if the
/// cause is unchanged the worker re-blocks the row on its very next tick with
/// the current reason.
///
/// Scoped to the note on screen (2026-09-30). It re-armed the whole account
/// before, on the theory that a refusal is rarely about one note — true of
/// Microsoft's missing folder id, false of iCloud's per-note refusals, where
/// it unlocked notes the user was not looking at until each was refused again.
#[tauri::command]
pub fn retry_blocked_push(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let cleared = state
        .db
        .clear_push_block(&uuid, &account_id)
        .map_err(|e| format!("retry_blocked_push: {e}"))?;
    log!("retry_blocked_push: uuid={} account={} re-armed={}", uuid, account_id, cleared);
    Ok(cleared)
}

/// Blocked pins and folders for one account (migration 23) — the read side of
/// the sidebar's folder marker, NoteList's pin marker and the editor's
/// "Pin not synced" line. Pure SQLite. `async` so it never waits on the main
/// thread behind a jodd-mcp write (R8's reasoning).
#[tauri::command]
pub async fn list_sync_blocks(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<db::SyncBlocks, String> {
    state.db.list_sync_blocks(&account_id).map_err(|e| format!("list_sync_blocks: {e}"))
}

/// `retry_blocked_push` for a pin: re-arm one note's refused pin push. Same
/// reasoning — the user pressing "Try again" is the only signal most
/// blockers produce, and an unchanged cause just re-blocks on the next tick.
#[tauri::command]
pub async fn retry_blocked_pin(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let cleared = state.db.clear_pin_push_block(&uuid, &account_id)
        .map_err(|e| format!("retry_blocked_pin: {e}"))?;
    log!("retry_blocked_pin: uuid={} account={} re-armed={}", uuid, account_id, cleared);
    Ok(cleared)
}

/// `retry_blocked_push` for a folder row: re-arm one refused folder push.
/// A refused delete is retried only if `delete_folder` would still accept
/// it — the folder is listed again while blocked, so a note may have been
/// filed into it since, and LocalFs deletes a directory with its contents.
#[tauri::command]
pub async fn retry_blocked_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let is_delete = state.db.folder_sync_state(&account_id, &path)
        .map_err(|e| format!("retry_blocked_folder: {e}"))?
        == Some(db::FolderSyncState::DeletedPending);
    if is_delete {
        if let Some(refusal) = state.db.folder_delete_refusal(&account_id, &path)
            .map_err(|e| format!("retry_blocked_folder: {e}"))?
        {
            return Err(refusal);
        }
    }
    let cleared = state.db.clear_folder_push_block(&account_id, &path)
        .map_err(|e| format!("retry_blocked_folder: {e}"))?;
    log!("retry_blocked_folder: path={:?} account={} re-armed={}", path, account_id, cleared);
    Ok(cleared)
}

/// Outstanding pushes for one account. Drives the Inactive group's "N left"
/// line and the Stop-waiting confirmation. No side effects. Polled every few
/// seconds per account, so it runs off the main thread (`db_blocking`).
#[tauri::command]
pub async fn count_pending_pushes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<db::PendingPushes, String> {
    crate::db_blocking(&state, move |db| {
        db.count_pending_pushes(&account_id).map_err(|e| format!("count_pending_pushes: {e}"))
    })
    .await
}

/// Move an account through its lifecycle. Rejects transitions the UI does not
/// offer rather than trusting the caller.
#[tauri::command]
pub fn set_account_status(
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
pub(crate) fn account_is_usable(has_local_cache: bool, has_refreshable_creds: bool) -> bool {
    has_local_cache || has_refreshable_creds
}

#[tauri::command]
pub async fn is_authenticated(state: State<'_, AppState>) -> Result<bool, String> {
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
