use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use crate::log;

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

// ---------------------------------------------------------------------------
// Microsoft
//
// Deliberately a second file rather than a second field in `OAuthConfig`: the
// Google one is on disk as `google_oauth.json`, and putting a Microsoft client
// id inside a file named for Google is the kind of small lie that costs an hour
// later. A separate file also needs no migration for existing installs.
//
// **There is no secret half here, and that is not an omission.** The Microsoft
// registration is a public client (see `auth_ms`), so everything Google needs
// for secret/id pairing — the keychain entry, `plan_cred_write`, the four-state
// `CredWrite` — has no counterpart. Saving is "write the string or delete the
// file".
// ---------------------------------------------------------------------------

fn ms_config_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    Ok(dir.join("microsoft_oauth.json"))
}

/// The user-configured Microsoft client id, or `None` when unset or blank.
/// Blank is normalised to `None` here so callers never have to distinguish
/// "file absent" from "file present with an empty string" — both mean
/// "fall through to the next tier".
pub fn load_ms_client_id() -> Option<String> {
    let p = ms_config_path().ok()?;
    if !p.exists() {
        return None;
    }
    let txt = fs::read_to_string(&p).ok()?;
    let cfg: OAuthConfig = serde_json::from_str(&txt).ok()?;
    let id = cfg.client_id.trim().to_string();
    if id.is_empty() { None } else { Some(id) }
}

pub fn save_ms_client_id(client_id: &str) -> Result<(), String> {
    let cfg = OAuthConfig { client_id: client_id.trim().to_string() };
    let txt = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let p = ms_config_path()?;
    fs::write(&p, txt).map_err(|e| format!("write {}: {}", p.display(), e))
}

pub fn clear_ms_client_id() -> Result<(), String> {
    let p = ms_config_path()?;
    if p.exists() {
        fs::remove_file(&p).map_err(|e| format!("remove: {}", e))?;
    }
    Ok(())
}

/// The OAuth client secret, once read, for the life of the process.
///
/// **Why a VALUE cache here when `accounts::RT_PRESENT` deliberately caches
/// only a bool:** a refresh token is a live credential whose value must not
/// linger, and presence is all its caller needs. This is the opposite case on
/// both counts. Callers need the secret itself, and Google documents a Desktop
/// client's secret as *not actually secret* — it is embeddable in distributed
/// binaries by design, which `auth::client_secret` already says at length.
/// It is in this process's memory on every call regardless; holding it is not
/// a new exposure, and the alternative is a macOS password prompt per OAuth
/// operation.
///
/// `Option<Option<String>>`: the outer layer is "have we looked yet", the
/// inner is "was anything there". Both states must be cached — an absent
/// secret is the common case (most installs use the compile-time credential)
/// and re-reading to rediscover absence is exactly the amplification this
/// removes.
static SECRET: OnceLock<Mutex<Option<Option<String>>>> = OnceLock::new();

fn secret_cache() -> &'static Mutex<Option<Option<String>>> {
    SECRET.get_or_init(|| Mutex::new(None))
}

/// Record what the store now holds. Called from the only two functions that
/// can change it, so invalidation is structural rather than a discipline —
/// the same reason `RT_PRESENT` lives beside its writers.
fn note_secret(value: Option<String>) {
    if let Ok(mut g) = secret_cache().lock() {
        *g = Some(value);
    }
}

pub fn load_secret() -> Option<String> {
    if let Ok(g) = secret_cache().lock() {
        if let Some(cached) = g.as_ref() {
            return cached.clone();
        }
    }
    // Logged for the same reason `accounts::load_refresh_token` is: gotcha #15
    // was diagnosed by COUNTING keychain reads, and a read that does not log is
    // a read that count misses. Measured 2026-08-31 — a user asked why macOS
    // prompted four times at startup and the log could only account for two,
    // because three of the five production read sites (this one included) were
    // silent. A distinct keychain item is a distinct prompt.
    log!("keychain READ   {}/{}", KC_SERVICE, KC_SECRET_KEY);
    let found = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty());
    note_secret(found.clone());
    found
}

pub fn save_secret(secret: &str) -> Result<(), String> {
    keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?
        .set_password(secret)
        .map_err(|e| e.to_string())?;
    note_secret(Some(secret.to_string()));
    Ok(())
}

pub fn clear_secret() -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?;
    let outcome = match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    if outcome.is_ok() {
        note_secret(None);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, not three, because all three would share ONE process-wide
    /// static and cargo runs tests in parallel — split, they clobber each
    /// other's setup and fail by order rather than by behaviour. (Observed:
    /// the first split attempt failed exactly that way.) Independence you do
    /// not have is not worth pretending to.
    ///
    /// The cache must answer without touching the store — that is the whole
    /// point, and it is also what keeps this test from opening a real macOS
    /// password prompt in CI and on a developer's machine.
    ///
    /// Baseline this replaces, measured 2026-08-31 from Jodd's own keychain
    /// log across four app starts: `oauth_client_secret::google` was read
    /// 2-3 times per start, each read a separate password prompt, for a value
    /// that cannot change while the process lives.
    #[test]
    fn the_secret_cache_answers_without_the_store_and_follows_its_writers() {
        // A cached value is served, repeatedly.
        note_secret(Some("cached-value".into()));
        assert_eq!(load_secret(), Some("cached-value".to_string()));
        assert_eq!(load_secret(), Some("cached-value".to_string()));

        // A later write replaces it — invalidation is structural, driven by
        // the only two functions that can change what the store holds.
        note_secret(Some("new".into()));
        assert_eq!(load_secret(), Some("new".to_string()));

        // Absence is cached too. Most installs have no stored secret and use
        // the compile-time credential, so re-reading to rediscover "still
        // nothing" is precisely the amplification being removed.
        note_secret(None);
        assert_eq!(load_secret(), None);
        assert_eq!(load_secret(), None);
    }

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
