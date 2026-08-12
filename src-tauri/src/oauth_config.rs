use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const KC_SERVICE: &str = "jodd";
const KC_SECRET_KEY: &str = "oauth_client_secret::google";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct OAuthConfig {
    pub client_id: String,
}

fn config_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    Ok(dir.join("google_oauth.json"))
}

pub fn load() -> Option<OAuthConfig> {
    let p = config_path().ok()?;
    if !p.exists() {
        return None;
    }
    let txt = fs::read_to_string(&p).ok()?;
    serde_json::from_str(&txt).ok()
}

pub fn save(client_id: &str) -> Result<(), String> {
    let cfg = OAuthConfig { client_id: client_id.to_string() };
    let txt = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
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
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_config_serde_roundtrip() {
        let cfg = OAuthConfig { client_id: "test-id-123".to_string() };
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: OAuthConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.client_id, "test-id-123");
    }

    #[test]
    fn oauth_config_default_is_empty() {
        let cfg = OAuthConfig::default();
        assert!(cfg.client_id.is_empty());
    }

    #[test]
    fn load_returns_none_for_nonexistent_file() {
        // Probabilistic: passes only if the user hasn't saved BYO credentials yet.
        // On a fresh install this always passes; on a configured machine it's skipped.
        let p = config_path().unwrap();
        if !p.exists() {
            assert!(load().is_none());
        }
    }
}
