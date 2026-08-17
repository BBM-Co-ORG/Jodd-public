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

pub fn load_secret() -> Option<String> {
    keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty())
}

pub fn save_secret(secret: &str) -> Result<(), String> {
    keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?
        .set_password(secret)
        .map_err(|e| e.to_string())
}

pub fn clear_secret() -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::LlmProviderKind;

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
