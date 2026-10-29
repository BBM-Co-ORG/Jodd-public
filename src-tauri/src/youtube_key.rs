//! The app-level YouTube Data API key that lets ingest read a video's public
//! comments (spec 2026-10-28 §6.2, §11). Optional: without it, YouTube links
//! are read exactly as before.
//!
//! Mirrors `stt_config`'s key handling: the key in the OS keychain, a
//! non-secret "is a key saved" flag in a file beside the app config — so the
//! Settings page and the ingest's "should I read the keychain?" check never
//! touch the credential store (gotcha #15). The keychain is read only when an
//! ingest or a "Find links" actually holds a YouTube link.

use std::fs;
use std::path::PathBuf;

use crate::log;

const KC_SERVICE: &str = "jodd";
/// The `__app__` sentinel keeps this disjoint from every account's entries.
const KC_SECRET_KEY: &str = "youtube_api_key::__app__";

fn status_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    Ok(dir.join("app_youtube_key_status.json"))
}

/// Whether a key is saved, from the flag file only. `None` = never set.
pub fn status() -> Option<bool> {
    serde_json::from_str(&fs::read_to_string(status_path().ok()?).ok()?).ok()
}

fn record_status(present: bool) -> Result<(), String> {
    fs::write(status_path()?, if present { "true" } else { "false" })
        .map_err(|e| format!("The key was saved, but its status could not be recorded: {e}"))
}

/// The key, from the keychain. Never logged.
pub fn load() -> Result<Option<String>, String> {
    log!("keychain READ   {}/{}", KC_SERVICE, KC_SECRET_KEY);
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY).map_err(|_| "Could not access the API key store.".to_string())?;
    match entry.get_password() {
        Ok(s) => Ok((!s.trim().is_empty()).then_some(s)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(_) => Err("Could not access the YouTube API key. Access may have been cancelled or denied.".into()),
    }
}

/// A Data API key is 39 characters starting `AIza`; anything far from that
/// shape is a paste of the wrong thing (an OAuth client id, a URL).
pub fn check_shape(key: &str) -> Result<(), String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("The key must not be blank. Use Remove key to delete it.".into());
    }
    if k.len() < 30 || k.len() > 60 || k.contains(char::is_whitespace) || k.contains("://") || k.contains(".apps.googleusercontent.com") {
        return Err("That does not look like a YouTube Data API key (it usually starts with \"AIza\").".into());
    }
    Ok(())
}

pub fn save(key: &str) -> Result<(), String> {
    check_shape(key)?;
    keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|e| e.to_string())?
        .set_password(key.trim())
        .map_err(|e| e.to_string())?;
    record_status(true)
}

pub fn clear() -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY).map_err(|e| e.to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => record_status(false),
        Err(e) => Err(e.to_string()),
    }
}

/// The comments reader, when a key is saved and readable. Reads the keychain
/// only when the flag says a key exists — call it off the async runtime.
pub fn ready() -> Option<crate::ingest::discuss::youtube::CommentsApi> {
    if status() != Some(true) {
        return None;
    }
    match load() {
        Ok(Some(key)) => Some(crate::ingest::discuss::youtube::CommentsApi::with_key(key)),
        Ok(None) => None,
        Err(e) => {
            log!("youtube_key: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_check_catches_the_usual_wrong_pastes() {
        assert!(check_shape("AIzaSyA1234567890abcdefghijklmnopqrstu").is_ok());
        assert!(check_shape("  ").unwrap_err().contains("blank"));
        for wrong in ["1234-abc.apps.googleusercontent.com-padding-padding", "https://console.cloud.google.com/apis", "short", "AIza with spaces in the middle of it"] {
            assert!(check_shape(wrong).is_err(), "{wrong}");
        }
    }
}
