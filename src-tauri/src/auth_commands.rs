//! Sign-in: the commands and helpers that add a Google or Microsoft account.
//!
//! `get_auth_url` builds the provider URL and remembers the pending sign-in;
//! `open_auth_url` opens the browser and, on desktop, waits for the loopback
//! callback; `complete_oauth` exchanges the code, stores the refresh token and
//! creates the account. Android reaches `complete_oauth` from a deep link
//! instead (gotcha #8), which is why it is a plain function and not a command.
//!
//! iCloud sign-in is not here: it has no OAuth and lives with the account
//! commands. Moved out of `lib.rs` 2026-11-16, unchanged.

use tauri::{AppHandle, Emitter, Manager, State};

use crate::accounts::Account;
use crate::backend::gmail::wire as gmail;
use crate::{
    accounts, auth, auth_ms, constant_time_eq, log, record_token_readability, secrets,
    token_deadline_from_expires_in, AppState,
};

// ─── Auth / Add Account ──────────────────────────────────────────────────────

/// Extract `(code, state)` from an OAuth redirect URL. Scheme-agnostic on
/// purpose — only the query string matters, so it has survived all three
/// redirect shapes this project has used (loopback, the retired custom scheme,
/// and the Android App Links https URL). Returns `None` if either parameter is
/// absent —
/// including the consent-denied case, where Google sends `error=` and no
/// code, and the missing-state case, where the CSRF check could not be done.
// Called through `capture::route` by the deep-link handler in `run()`'s
// `.setup()` on every platform; only Android acts on a match (desktop signs
// in via the loopback listener).
pub(crate) fn parse_oauth_callback(url: &str) -> Option<(String, String)> {
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
pub(crate) fn backend_kind_for_signin(backend: Option<&str>) -> Result<accounts::BackendKind, String> {
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
/// always fired. It kept firing in every release through v0.33.0, because the
/// `MS_CLIENT_ID` repo secret the embedded tier reads was never created;
/// `release.yml`'s build job now refuses to compile without it.
///
/// For a developer the advice is `.env` and a restart — the runtime tier is
/// read on every call, so nothing has to be rebuilt to supply a client id. A
/// downloaded build gets different advice; see [`refuse_empty_client_id`].
pub(crate) fn refuse_missing_client_id(kind: accounts::BackendKind) -> Result<(), String> {
    let (id, var, provider) = match kind {
        accounts::BackendKind::Microsoft => (auth_ms::client_id(), "MS_CLIENT_ID", "Microsoft"),
        accounts::BackendKind::Gmail => (auth::client_id(), "GOOGLE_CLIENT_ID", "Google"),
        // No OAuth flow — never reached via `backend_kind_for_signin`.
        // iCloud has no OAuth client of any kind: sign-in is a real webview
        // against Apple's own pages, so there is no id that could be missing.
        // SSH has no OAuth either — no client id, no PKCE pair, no redirect,
        // no token; it authenticates over ssh-agent, which Jodd never touches.
        accounts::BackendKind::LocalFs | accounts::BackendKind::ICloud | accounts::BackendKind::Ssh => return Ok(()),
    };
    refuse_empty_client_id(&id, var, provider)
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
///
/// The message speaks to two readers, because the code cannot tell them apart:
/// `cfg!(debug_assertions)` would not do it, since a developer's own
/// `tauri build` is a release build too. A downloaded release has no `.env` to
/// edit — v0.33.0 shipped without `MS_CLIENT_ID` and told its users to add one
/// (2026-10-11) — so that reader is told the build is at fault and where to
/// report it; a developer still gets the variable name and `.env`.
pub(crate) fn refuse_empty_client_id(id: &str, var: &str, provider: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err(format!(
            "This copy of Jodd was built without a {provider} client id, so it cannot \
             start a {provider} sign-in. If you downloaded Jodd, the release is missing \
             it: update to the latest version, or report it at \
             https://github.com/BBM-Co-ORG/Jodd-public/issues. If you built Jodd \
             yourself, set {var} in .env (see .env.example) and restart Jodd."
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
pub(crate) struct OauthError {
    pub(crate) message: String,
    pub(crate) admin_consent_url: Option<String>,
}

impl OauthError {
    /// A failure with nothing to offer but its message.
    pub(crate) fn plain(message: impl Into<String>) -> Self {
        Self { message: message.into(), admin_consent_url: None }
    }
}

pub(crate) fn emit_oauth_error(app: &AppHandle, message: impl Into<String>) {
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
pub(crate) fn signin_denial(kind: accounts::BackendKind, denial: &auth::CallbackDenial) -> OauthError {
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
        // SSH has no tenant, no consent screen and no app registration
        // either — it is never reached via `backend_kind_for_signin` at all.
        accounts::BackendKind::Gmail
        | accounts::BackendKind::LocalFs
        | accounts::BackendKind::ICloud
        | accounts::BackendKind::Ssh => None,
    };
    OauthError { message, admin_consent_url }
}

#[tauri::command]
pub async fn get_auth_url(
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
pub(crate) fn resolve_pending_signin(
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
pub(crate) async fn complete_oauth(app: AppHandle, code: String, cb_state: String) {
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
                ssh_target: None,
                ssh_port: None,
                ssh_key: None,
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
        // A fresh sign-in into an account that had lost its token: the same
        // account id, so its notes were kept (the push above did not replace
        // it), and this is the reconnect the signed-out row asked for.
        entry.refresh_token_unreadable = false;
    }
    record_token_readability(&state, &account_id, true);

    log!("complete_oauth: emitting oauth-success");
    let _ = app.emit("oauth-success", email);
}

#[tauri::command]
pub async fn open_auth_url(app: AppHandle, url: String) -> Result<(), String> {
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
pub fn platform_name() -> String {
    std::env::consts::OS.to_string()
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
