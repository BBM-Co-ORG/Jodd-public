//! App settings commands: the screens under Settings that configure Jodd
//! itself rather than one account.
//!
//! - **OAuth credentials** — a user's own Google or Microsoft client
//!   (`oauth_config`), resolved ahead of the embedded one.
//! - **App AI provider** — the app-level LLM config and its key.
//! - **Speech-to-text** endpoint, **link-following** rules, **YouTube** key.
//! - **Diagnostics** — the persistent log file toggle and "Clear log".
//!
//! Moved out of `lib.rs` 2026-11-16, unchanged.

use tauri::State;

use crate::{
    accounts, app_llm_config, applog, auth, auth_ms, data_since, ingest, invalidate_ai, link_settings, log,
    oauth_config, paths, stt_config, youtube_key, AppState,
};

// ─── App-level OAuth credential config (BYO credentials) ─────────────────────

#[derive(serde::Serialize)]
pub struct OAuthConfigStatus {
    client_id: String,
    has_secret: bool,
    /// True when ANY credentials are available via the 3-tier resolution chain
    /// (user-configured OR compile-time embedded OR runtime env). AuthScreen
    /// uses this to decide whether to enable the Gmail sign-in button.
    credentials_available: bool,
}

#[tauri::command]
pub fn get_oauth_config() -> OAuthConfigStatus {
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

/// Settings navigation needs only the configured ID, never the client secret.
#[tauri::command]
pub fn get_oauth_settings() -> oauth_config::OAuthConfig {
    oauth_config::load().unwrap_or_default()
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
pub fn save_oauth_config(client_id: String, client_secret: String) -> Result<(), String> {
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
pub fn clear_oauth_config() -> Result<(), String> {
    oauth_config::clear()?;
    oauth_config::clear_secret()?;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct MsOAuthConfigStatus {
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
pub fn get_ms_oauth_config() -> MsOAuthConfigStatus {
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
pub fn save_ms_oauth_config(client_id: String) -> Result<(), String> {
    let id = client_id.trim();
    if id.is_empty() {
        oauth_config::clear_ms_client_id()
    } else {
        oauth_config::save_ms_client_id(id)
    }
}

#[derive(serde::Serialize)]
pub struct AppLlmConfigStatus {
    cfg: app_llm_config::AppLlmConfig,
    /// Last-known non-secret status; None means not checked. Never reads the store.
    has_api_key: Option<bool>,
    /// Set when a saved file exists but cannot be read, so Settings › AI says
    /// so instead of showing "nothing set". `cfg` is then the default.
    unreadable: Option<String>,
}

#[tauri::command]
pub fn get_app_llm_config() -> AppLlmConfigStatus {
    let (cfg, unreadable) = match app_llm_config::load_status() {
        app_llm_config::Loaded::Config(cfg) => (cfg, None),
        app_llm_config::Loaded::Missing => (Default::default(), None),
        app_llm_config::Loaded::Unreadable(why) => (Default::default(), Some(why)),
    };
    AppLlmConfigStatus { cfg, unreadable, has_api_key: app_llm_config::secret_status() }
}

/// When the settings folder Jodd uses on this device began (`data_since`):
/// after a reinstall or Clear storage it moves forward, so a lost setting
/// can be told apart from a wiped install.
#[tauri::command]
pub fn get_data_since() -> Option<data_since::DataSince> {
    data_since::read()
}

/// Provider-only navigation probe. Neither this nor the Settings status command
/// reads the credential store; only explicit key operations may do that.
#[tauri::command]
pub fn get_app_llm_provider() -> accounts::LlmConfig {
    app_llm_config::load().unwrap_or_default().llm
}

/// `api_key`: Some("") clears the stored key, None leaves it untouched
/// (so the UI can save other fields without re-entering the secret).
#[tauri::command]
pub fn set_app_llm_config(
    cfg: app_llm_config::AppLlmConfig,
    api_key: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    match api_key.as_deref() {
        None => {}
        Some("") => app_llm_config::clear_secret()?,
        Some(k) => app_llm_config::save_secret(k.trim())?,
    }
    app_llm_config::save(&cfg)?;
    log!(
        "set_app_llm_config: provider={:?} apply_to_accounts={}",
        cfg.llm.provider,
        cfg.apply_to_accounts
    );
    Ok(())
}

/// Explicit user request; failures preserve the last-known status.
#[tauri::command]
pub fn check_app_llm_key() -> Result<bool, String> {
    app_llm_config::check_secret()
}

/// Key-only operation: never saves an unrelated provider form draft.
#[tauri::command]
pub fn delete_app_llm_key(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let _policy_gate = state.ai_policy.gate.lock().unwrap();
    invalidate_ai(&state, &app);
    app_llm_config::clear_secret()
}

// ─── Speech-to-text endpoint (spec 2026-10-18-social-video-ingest, D3/D5) ───

#[derive(serde::Serialize)]
pub struct SttConfigStatus {
    cfg: stt_config::SttConfig,
    /// Last-known non-secret status; None means not checked. Never reads the
    /// store — and never calls `stt_config::ready()`, which does (gotcha #15).
    has_api_key: Option<bool>,
}

#[tauri::command]
pub fn get_stt_config() -> SttConfigStatus {
    SttConfigStatus { cfg: stt_config::load().unwrap_or_default(), has_api_key: stt_config::secret_status() }
}

#[derive(serde::Serialize)]
pub struct LinkSettingsDto {
    /// Account ids that follow the best links automatically (spec 2026-10-28 D2).
    auto_follow: Vec<String>,
    /// The rule, so the frontend never mirrors it: at most this many per
    /// source, at or above this score.
    auto_follow_max: usize,
    auto_follow_min_score: i32,
}

/// Settings › Links and the Extract modal: which accounts follow links
/// automatically. A file read — never the keychain (gotcha #15).
#[tauri::command]
pub fn get_link_settings() -> LinkSettingsDto {
    LinkSettingsDto {
        auto_follow: link_settings::load().auto_follow,
        auto_follow_max: link_settings::AUTO_FOLLOW_MAX,
        auto_follow_min_score: link_settings::AUTO_FOLLOW_MIN_SCORE,
    }
}

/// Replace the auto-follow list. Ids that are not accounts are dropped.
#[tauri::command]
pub fn set_link_settings(auto_follow: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    let ids: Vec<String> = state.accounts.lock().unwrap().iter().map(|a| a.id.clone()).collect();
    let mut s = link_settings::LinkSettings::default();
    for id in &auto_follow {
        s.set_auto_follow(id, true);
    }
    s.keep_only(&ids);
    link_settings::save(&s)?;
    log!("set_link_settings: auto_follow on for {} account(s)", s.auto_follow.len());
    Ok(())
}

/// Settings › Links: whether a YouTube Data API key is saved. The flag file
/// only — never the keychain (gotcha #15).
#[tauri::command]
pub fn get_youtube_key_status() -> Option<bool> {
    youtube_key::status()
}

#[tauri::command]
pub fn set_youtube_key(key: String) -> Result<(), String> {
    youtube_key::save(&key)?;
    log!("set_youtube_key: saved");
    Ok(())
}

#[tauri::command]
pub fn clear_youtube_key() -> Result<(), String> {
    youtube_key::clear()?;
    log!("clear_youtube_key: removed");
    Ok(())
}

/// `api_key`: Some("") clears the stored key, None leaves it untouched (unless
/// the address moved to another origin, which clears it). `confirm` is the
/// exact origin the user was shown; it is honoured only if it still equals
/// the origin of `cfg.base_url`, else nothing is written. `cfg.confirmed_host`
/// is ignored. See `stt_config::apply_save`.
#[tauri::command]
pub fn set_stt_config(cfg: stt_config::SttConfig, api_key: Option<String>, confirm: Option<String>) -> Result<(), String> {
    let (cfg, key) = stt_config::apply_save(cfg, stt_config::load().as_ref(), api_key.as_deref(), confirm.as_deref())?;
    let (preset, origin, confirmed) = (cfg.preset, stt_config::origin_of(&cfg.base_url), cfg.confirmed_host.is_some());
    // In `save_steps`' order: a key never outlives a move of the address.
    for step in stt_config::save_steps(cfg, key) {
        match step {
            stt_config::SaveStep::ClearKey => stt_config::clear_secret()?,
            stt_config::SaveStep::WriteConfig(cfg) => stt_config::save(&cfg)?,
            stt_config::SaveStep::SetKey(k) => stt_config::save_secret(&k)?,
        }
    }
    log!("set_stt_config: preset={preset:?} origin={origin:?} confirmed={confirmed}");
    Ok(())
}

/// Transcribe the bundled Thai + English clip with the SAVED, confirmed
/// settings, so the user sees it work before relying on it.
#[tauri::command]
pub async fn test_stt() -> Result<String, String> {
    // `ready()` reads the keychain, which can block on an OS prompt.
    let ready = tokio::task::spawn_blocking(stt_config::ready)
        .await
        .map_err(|e| format!("keychain read: {e}"))?
        .ok_or("Speech-to-text is not set up: save the settings and confirm the host first.")?;
    let dir = paths::cache_base().ok_or("no cache dir on this OS")?.join("jodd");
    log!("test_stt: host={} model={}", ready.host, ready.model);
    ingest::audio::transcribe::test_clip(&ready, &dir).await
}

// ─── Diagnostics: persistent file logging toggle ──────────────────────────

#[derive(serde::Serialize)]
pub struct LogSettingsStatus {
    file_logging_enabled: bool,
    /// Resolved path so the UI can show "logs saved to: X" and offer to
    /// reveal it, without duplicating the path-resolution logic client-side.
    log_file_path: String,
    /// Current size of the log file in bytes, so the UI can show growth
    /// ("N KB") and the user knows when "Clear log" is worth reaching for.
    log_file_size_bytes: u64,
}

#[tauri::command]
pub fn get_log_settings() -> LogSettingsStatus {
    LogSettingsStatus {
        file_logging_enabled: applog::is_enabled(),
        log_file_path: applog::log_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        log_file_size_bytes: applog::log_file_size(),
    }
}

#[tauri::command]
pub fn set_file_logging_enabled(enabled: bool) -> Result<(), String> {
    applog::set_enabled(enabled)
}

/// User-triggered reset from the Diagnostics UI — reclaims space immediately
/// instead of waiting for the automatic 20MB rotation.
#[tauri::command]
pub fn clear_log_file() -> Result<(), String> {
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
