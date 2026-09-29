//! App-level LLM provider configuration — the default every account inherits
//! unless it overrides or disables (see llm::resolve::effective_config).
//!
//! Deliberately mirrors oauth_config.rs: non-secret fields in a JSON file
//! under the OS config dir, the API key in the OS keychain. Ask Jodd always
//! uses this config; per-account workflows (Extract, auto-link) adopt it only
//! when `apply_to_accounts` is set.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::accounts::LlmConfig;
use crate::log;

const KC_SERVICE: &str = "jodd";
/// The `__app__` sentinel keeps this key disjoint from every per-account key
/// (`llm_api_key::{account_id}`), since an account id is an email address or
/// `localfs:<uuid>` and can never be `__app__`.
const KC_SECRET_KEY: &str = "llm_api_key::__app__";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AppLlmConfig {
    #[serde(default)]
    pub llm: LlmConfig,
    /// When true, accounts whose own provider is unset inherit `llm`.
    /// Does not affect Ask Jodd, which always uses `llm`.
    #[serde(default)]
    pub apply_to_accounts: bool,
}

fn config_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    Ok(dir.join("app_llm.json"))
}

pub fn load() -> Option<AppLlmConfig> {
    let p = config_path().ok()?;
    if !p.exists() {
        return None;
    }
    let txt = fs::read_to_string(&p).ok()?;
    serde_json::from_str(&txt).ok()
}

pub fn save(cfg: &AppLlmConfig) -> Result<(), String> {
    let txt = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let p = config_path()?;
    fs::write(&p, txt).map_err(|e| format!("write {}: {}", p.display(), e))
}

pub fn clear() -> Result<(), String> {
    let p = config_path()?;
    if p.exists() {
        fs::remove_file(&p).map_err(|e| format!("remove: {}", e))?;
    }
    Ok(())
}

// Non-secret last-known status, kept separately so saving provider fields cannot
// overwrite it. Missing metadata (including older installs) means unknown.
fn status_path() -> Result<PathBuf, String> {
    Ok(config_path()?.with_file_name("app_llm_key_status.json"))
}

pub fn secret_status() -> Option<bool> {
    serde_json::from_str(&fs::read_to_string(status_path().ok()?).ok()?).ok()
}

fn record_secret_status(present: bool) -> Result<(), String> {
    fs::write(status_path()?, if present { "true" } else { "false" })
        .map_err(|e| format!("API key operation succeeded, but its status could not be saved: {e}"))
}

fn decode_secret(result: Result<String, keyring_core::Error>) -> Result<Option<String>, String> {
    match result {
        Ok(s) => Ok((!s.is_empty()).then_some(s)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        // Cancellation/denial is not absence. Never return or log the key.
        Err(_) => Err("Could not access the API key. Access may have been cancelled or denied; its saved status is unchanged.".into()),
    }
}

pub fn load_secret() -> Result<Option<String>, String> {
    log!("keychain READ   {}/{}", KC_SERVICE, KC_SECRET_KEY);
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|_| "Could not access the API key store.".to_string())?;
    decode_secret(entry.get_password())
}

pub fn check_secret() -> Result<bool, String> {
    let present = load_secret()?.is_some();
    record_secret_status(present)?;
    Ok(present)
}

pub fn save_secret(secret: &str) -> Result<(), String> {
    if secret.trim().is_empty() {
        return Err("API key must not be blank. Use Delete API key to remove it.".into());
    }
    keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?
        .set_password(secret)
        .map_err(|e| e.to_string())?;
    record_secret_status(true)
}

pub fn clear_secret() -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => record_secret_status(false),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::LlmProviderKind;

    #[test]
    fn denied_or_failed_reads_are_not_absence() {
        assert_eq!(decode_secret(Err(keyring_core::Error::NoEntry)).unwrap(), None);
        assert_eq!(decode_secret(Ok(String::new())).unwrap(), None);
        assert_eq!(decode_secret(Ok("test-key".into())).unwrap().as_deref(), Some("test-key"));
        assert!(decode_secret(Err(keyring_core::Error::NoDefaultStore)).is_err());
        let error = decode_secret(Err(keyring_core::Error::BadEncoding(b"secret-marker".to_vec()))).unwrap_err();
        assert!(!error.contains("secret-marker"));
    }

    #[test]
    fn serde_roundtrip_preserves_apply_flag() {
        let cfg = AppLlmConfig {
            llm: crate::accounts::LlmConfig {
                provider: LlmProviderKind::AgentCli,
                agent_preset: Some("claude".into()),
                ..Default::default()
            },
            apply_to_accounts: true,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: AppLlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.llm.provider, LlmProviderKind::AgentCli);
        assert!(parsed.apply_to_accounts);
    }

    #[test]
    fn default_is_unconfigured_and_not_shared() {
        let cfg = AppLlmConfig::default();
        assert_eq!(cfg.llm.provider, LlmProviderKind::None);
        assert!(!cfg.apply_to_accounts);
    }

    #[test]
    fn missing_apply_flag_defaults_to_false() {
        // Forward-compat with any file written before the flag existed.
        let parsed: AppLlmConfig = serde_json::from_str(r#"{"llm":{}}"#).unwrap();
        assert!(!parsed.apply_to_accounts);
    }
}
