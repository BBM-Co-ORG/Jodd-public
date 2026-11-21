//! App-level speech-to-text endpoint (spec D3, D5, §5.4).
//!
//! One shape — OpenAI's `POST {base}/audio/transcriptions` — covers cloud
//! services and a server the user runs. Mirrors `app_llm_config.rs`: the
//! non-secret fields in `app_stt.json` beside `app_llm.json`, the key in the
//! OS keychain. App-level only: audio is not account data.
//!
//! **Where audio may go is a confirmed origin, not a URL** (D5). `confirmed_host`
//! holds `scheme://host[:port]` (default ports omitted, host lowercase). It is
//! written only by `apply_save`, and only when the user's Confirm names the
//! very origin the saved base URL has; `ready()` refuses unless it still equals
//! the base URL's origin — so editing the URL, port or scheme after confirming
//! sends nothing until the user confirms again.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::log;

const KC_SERVICE: &str = "jodd";
/// The `__app__` sentinel keeps this key disjoint from every other keychain
/// entry, as `app_llm_config::KC_SECRET_KEY` does.
const KC_SECRET_KEY: &str = "stt_api_key::__app__";

pub const DEFAULT_MAX_UPLOAD_MB: u32 = 25;

/// ≈2.5 s of Thai ("สวัสดีครับ") then English ("this is a test"), AAC in an
/// `.m4a`, for the Settings **Test** button (spec §5.4). Embedded rather than
/// a Tauri resource so it exists on every platform without a resource lookup.
/// Regenerate on macOS (`afconvert -f mp4f` writes an unfragmented file, which
/// is the shape `ingest/audio/remux.rs` produces and uploads):
///
/// ```text
/// say -v Kanya    -o th.aiff "สวัสดีครับ"
/// say -v Samantha -o en.aiff "this is a test"
/// ffmpeg -i th.aiff -i en.aiff -filter_complex \
///   "[0:a]aresample=16000,aformat=channel_layouts=mono[a];[1:a]aresample=16000,aformat=channel_layouts=mono[b];[a][b]concat=n=2:v=0:a=1" cat.wav
/// afconvert -f mp4f -d aac -b 32000 cat.wav stt_test_clip.m4a
/// ```
pub const TEST_CLIP: &[u8] = include_bytes!("../resources/stt_test_clip.m4a");

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SttPreset {
    /// `openai`
    OpenAi,
    Groq,
    #[default]
    Custom,
}

fn default_max_upload_mb() -> u32 {
    DEFAULT_MAX_UPLOAD_MB
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SttConfig {
    #[serde(default)]
    pub preset: SttPreset,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// Unset by default: Thai and English mix within a clip, and forcing one
    /// damages the other (§5.3).
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default = "default_max_upload_mb")]
    pub max_upload_mb: u32,
    /// The origin (`scheme://host[:port]`) the user confirmed audio may go to.
    /// Written by the backend only (`apply_save`); a value arriving from the
    /// frontend is ignored.
    #[serde(default)]
    pub confirmed_host: Option<String>,
}

impl Default for SttConfig {
    fn default() -> Self {
        SttConfig {
            preset: SttPreset::Custom,
            base_url: String::new(),
            model: String::new(),
            language: None,
            max_upload_mb: DEFAULT_MAX_UPLOAD_MB,
            confirmed_host: None,
        }
    }
}

/// What `transcribe` needs, available only once the user confirmed the host.
#[derive(Clone, Debug, PartialEq)]
pub struct SttReady {
    /// Which kind of endpoint, for receipts (D15): a code-owned label, never the host.
    pub preset: SttPreset,
    pub base_url: String,
    pub model: String,
    pub language: Option<String>,
    pub max_upload_bytes: u64,
    pub api_key: Option<String>,
    /// `host[:port]` for display (port only when not the scheme's default).
    pub host: String,
}

fn parsed(base_url: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(base_url.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(url)
}

fn host_port(url: &reqwest::Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    }
}

/// `host[:port]` of an http(s) URL, lowercase, the port only when it is not
/// the scheme's default; None for anything else. What the user is shown.
pub fn display_host_of(base_url: &str) -> Option<String> {
    parsed(base_url).map(|u| host_port(&u))
}

/// `scheme://host[:port]` of an http(s) URL — what a confirmation binds to, so
/// a changed port or `https` -> `http` asks again.
pub fn origin_of(base_url: &str) -> Option<String> {
    parsed(base_url).map(|u| format!("{}://{}", u.scheme(), host_port(&u)))
}

/// The part of `ready()` that needs no keychain: the config as it would be
/// used if the origin is confirmed, else None. Kept apart so a changed or
/// unconfirmed origin never reaches the credential store (gotcha #15's spirit).
fn confirmed(cfg: &SttConfig) -> Option<SttReady> {
    let origin = origin_of(&cfg.base_url)?;
    if cfg.confirmed_host.as_deref().map(str::to_ascii_lowercase).as_deref() != Some(origin.as_str()) {
        return None;
    }
    let mb = if cfg.max_upload_mb == 0 { DEFAULT_MAX_UPLOAD_MB } else { cfg.max_upload_mb };
    Some(SttReady {
        preset: cfg.preset,
        base_url: cfg.base_url.trim().trim_end_matches('/').to_string(),
        model: cfg.model.trim().to_string(),
        language: cfg.language.as_deref().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string),
        max_upload_bytes: u64::from(mb) * 1024 * 1024,
        api_key: None,
        host: display_host_of(&cfg.base_url)?,
    })
}

/// `host[:port]` audio would go to, when the saved config is confirmed for its
/// own address — what the Extract modal names (D5). Reads no keychain, so the
/// analysis the modal re-runs as the user types may call it (gotcha #15).
pub fn confirmed_display_host(cfg: &SttConfig) -> Option<String> {
    confirmed(cfg).map(|r| r.host)
}

/// Whether audio would go to the confirmed address over plain `http`, so the
/// Extract modal can say "(not encrypted)". No keychain, like
/// `confirmed_display_host`; false when nothing would be sent at all.
pub fn confirmed_insecure(cfg: &SttConfig) -> bool {
    confirmed(cfg).is_some() && origin_of(&cfg.base_url).is_some_and(|o| o.starts_with("http://"))
}

pub const ADDRESS_CHANGED: &str = "the address changed — save again to confirm it";

/// What `set_stt_config` does to the stored API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOp {
    Keep,
    Clear,
    Set(String),
    /// The address moved and a new key came with the save: the old key must
    /// be gone before the config names the new origin (see `save_steps`).
    Replace(String),
}

/// One write `set_stt_config` performs, in the order `save_steps` gives.
#[derive(Debug, Clone, PartialEq)]
pub enum SaveStep {
    ClearKey,
    WriteConfig(SttConfig),
    SetKey(String),
}

/// `apply_save`'s decision as ordered writes. A key is never left paired with
/// an origin it was not given for: a clear comes before the config, a new key
/// after it — so if a later step fails, the stored key is either gone or
/// belongs to the origin the stored config names.
pub fn save_steps(cfg: SttConfig, key: KeyOp) -> Vec<SaveStep> {
    match key {
        KeyOp::Keep => vec![SaveStep::WriteConfig(cfg)],
        KeyOp::Clear => vec![SaveStep::ClearKey, SaveStep::WriteConfig(cfg)],
        KeyOp::Set(k) => vec![SaveStep::WriteConfig(cfg), SaveStep::SetKey(k)],
        KeyOp::Replace(k) => vec![SaveStep::ClearKey, SaveStep::WriteConfig(cfg), SaveStep::SetKey(k)],
    }
}

/// Everything `set_stt_config` decides, as a pure function so it is testable
/// without the filesystem or keychain. Returns the config to write (with the
/// backend-computed `confirmed_host`) and what to do with the key; an `Err`
/// means nothing is written.
///
/// - `confirm` is the exact origin the user was shown. It is honoured only if
///   it equals the origin of `cfg.base_url` — an edit between showing and
///   confirming must not confirm an address the user never saw.
/// - Without `confirm`, the previous confirmation survives only while the
///   origin is unchanged. Never reads `confirmed_host` from `cfg`.
/// - If the saved address moved to another origin and no new key came with the
///   save, the stored key is cleared: a Groq key must not follow the user to
///   OpenAI.
pub fn apply_save(
    mut cfg: SttConfig,
    previous: Option<&SttConfig>,
    api_key: Option<&str>,
    confirm: Option<&str>,
) -> Result<(SttConfig, KeyOp), String> {
    cfg.base_url = cfg.base_url.trim().to_string();
    cfg.model = cfg.model.trim().to_string();
    cfg.language = cfg.language.take().map(|l| l.trim().to_string()).filter(|l| !l.is_empty());
    if cfg.max_upload_mb == 0 {
        cfg.max_upload_mb = DEFAULT_MAX_UPLOAD_MB;
    }
    let origin = origin_of(&cfg.base_url);
    cfg.confirmed_host = match confirm {
        Some(c) => {
            let c = c.trim().to_ascii_lowercase();
            if origin.as_deref() != Some(c.as_str()) {
                return Err(ADDRESS_CHANGED.to_string());
            }
            origin.clone()
        }
        None => previous
            .and_then(|p| p.confirmed_host.as_deref())
            .map(str::to_ascii_lowercase)
            .filter(|h| Some(h) == origin.as_ref()),
    };
    let moved = previous.is_some_and(|p| {
        let old_base = origin_of(&p.base_url);
        (p.confirmed_host.is_some() && p.confirmed_host != cfg.confirmed_host) || (old_base.is_some() && old_base != origin)
    });
    let key = match api_key {
        Some("") => KeyOp::Clear,
        Some(k) if moved => KeyOp::Replace(k.trim().to_string()),
        Some(k) => KeyOp::Set(k.trim().to_string()),
        None if moved => KeyOp::Clear,
        None => KeyOp::Keep,
    };
    Ok((cfg, key))
}

fn config_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
    Ok(dir.join("app_stt.json"))
}

pub fn load() -> Option<SttConfig> {
    let p = config_path().ok()?;
    if !p.exists() {
        return None;
    }
    let txt = fs::read_to_string(&p).ok()?;
    serde_json::from_str(&txt).ok()
}

pub fn save(cfg: &SttConfig) -> Result<(), String> {
    let txt = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    let p = config_path()?;
    fs::write(&p, txt).map_err(|e| format!("write {}: {}", p.display(), e))
}

/// Configured **and** confirmed for the current base URL. Reads the keychain
/// for the API key, so it must never be called from anything the frontend
/// polls (gotcha #15); `get_stt_config` reports `secret_status()` instead.
pub fn ready() -> Option<SttReady> {
    let mut ready = confirmed(&load()?)?;
    // A denied or failed keychain read is not "no key": sending without the
    // key the user set would only come back as a confusing 401.
    ready.api_key = match load_secret() {
        Ok(key) => key,
        Err(e) => {
            log!("stt: {}", e);
            return None;
        }
    };
    Some(ready)
}

// Non-secret last-known status, kept separately so saving fields cannot
// overwrite it. Missing metadata means unknown.
fn status_path() -> Result<PathBuf, String> {
    Ok(config_path()?.with_file_name("app_stt_key_status.json"))
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
        Err(_) => Err("Could not access the speech-to-text API key. Access may have been cancelled or denied.".into()),
    }
}

pub fn load_secret() -> Result<Option<String>, String> {
    log!("keychain READ   {}/{}", KC_SERVICE, KC_SECRET_KEY);
    let entry = keyring_core::Entry::new(KC_SERVICE, KC_SECRET_KEY)
        .map_err(|_| "Could not access the API key store.".to_string())?;
    decode_secret(entry.get_password())
}

pub fn save_secret(secret: &str) -> Result<(), String> {
    if secret.trim().is_empty() {
        return Err("API key must not be blank. Use Remove key to delete it.".into());
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

    fn cfg(base_url: &str, confirmed_host: Option<&str>) -> SttConfig {
        SttConfig {
            preset: SttPreset::Custom,
            base_url: base_url.into(),
            model: "whisper-1".into(),
            confirmed_host: confirmed_host.map(String::from),
            ..SttConfig::default()
        }
    }

    #[test]
    fn a_confirmed_origin_is_ready() {
        let r = confirmed(&cfg("https://API.Groq.com/openai/v1/", Some("https://api.groq.com"))).unwrap();
        assert_eq!(r.host, "api.groq.com");
        assert_eq!(r.base_url, "https://API.Groq.com/openai/v1");
        assert_eq!(r.max_upload_bytes, 25 * 1024 * 1024);
        // A default port is the same origin; a non-default one is shown.
        assert!(confirmed(&cfg("https://api.groq.com:443/v1", Some("https://api.groq.com"))).is_some());
        let lan = confirmed(&cfg("http://192.168.1.5:8000/v1", Some("http://192.168.1.5:8000"))).unwrap();
        assert_eq!(lan.host, "192.168.1.5:8000");
    }

    #[test]
    fn a_changed_host_is_not_ready() {
        // Confirmed for groq, then the URL was edited to another host.
        assert_eq!(confirmed(&cfg("https://evil.example/v1", Some("https://api.groq.com"))), None);
        assert_eq!(confirmed(&cfg("https://api.groq.com.evil.example/v1", Some("https://api.groq.com"))), None);
        // Same host, other port.
        assert_eq!(confirmed(&cfg("http://192.168.1.5:9000/v1", Some("http://192.168.1.5:8000"))), None);
        // Same host and port, other scheme.
        assert_eq!(confirmed(&cfg("http://api.groq.com/v1", Some("https://api.groq.com"))), None);
        assert_eq!(confirmed(&cfg("https://api.groq.com:8443/v1", Some("https://api.groq.com"))), None);
        // A pre-origin value (bare host) no longer confirms anything.
        assert_eq!(confirmed(&cfg("https://api.groq.com/v1", Some("api.groq.com"))), None);
    }

    #[test]
    fn the_modal_names_only_a_confirmed_host() {
        assert_eq!(confirmed_display_host(&cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"))).as_deref(), Some("api.groq.com"));
        assert_eq!(confirmed_display_host(&cfg("http://192.168.1.5:9000/v1", Some("http://192.168.1.5:9000"))).as_deref(), Some("192.168.1.5:9000"));
        assert_eq!(confirmed_display_host(&cfg("https://evil.example/v1", Some("https://api.groq.com"))), None);
        assert_eq!(confirmed_display_host(&cfg("https://api.groq.com/openai/v1", None)), None);
    }

    #[test]
    fn an_unconfirmed_config_is_not_ready() {
        assert_eq!(confirmed(&cfg("https://api.groq.com/openai/v1", None)), None);
    }

    #[test]
    fn a_url_that_is_not_http_is_not_ready() {
        assert_eq!(confirmed(&cfg("not a url", Some("not a url"))), None);
        assert_eq!(confirmed(&cfg("ftp://files.example/v1", Some("ftp://files.example"))), None);
        assert_eq!(confirmed(&cfg("", Some(""))), None);
        assert_eq!(origin_of("file:///etc/passwd"), None);
        assert_eq!(display_host_of("mailto:a@b.c"), None);
    }

    #[test]
    fn the_language_and_size_are_normalised() {
        let mut c = cfg("http://192.168.1.5:8000/v1", Some("http://192.168.1.5:8000"));
        c.language = Some("  ".into());
        c.max_upload_mb = 0;
        let r = confirmed(&c).unwrap();
        assert_eq!(r.language, None);
        assert_eq!(r.max_upload_bytes, 25 * 1024 * 1024);
        c.language = Some(" th ".into());
        assert_eq!(confirmed(&c).unwrap().language.as_deref(), Some("th"));
    }

    /// The Extract modal adds "(not encrypted)" to its per-run line from this —
    /// read from the config file alone, never the keychain (gotcha #15).
    #[test]
    fn only_a_confirmed_http_origin_is_insecure() {
        assert!(confirmed_insecure(&cfg("http://192.168.1.5:8000/v1", Some("http://192.168.1.5:8000"))));
        assert!(confirmed_insecure(&cfg("HTTP://192.168.1.5:8000/v1", Some("http://192.168.1.5:8000"))));
        assert!(!confirmed_insecure(&cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"))));
        // Not confirmed for its own address: nothing is sent, so nothing is insecure.
        assert!(!confirmed_insecure(&cfg("http://192.168.1.5:8000/v1", None)));
        assert!(!confirmed_insecure(&cfg("http://192.168.1.5:9000/v1", Some("http://192.168.1.5:8000"))));
    }

    #[test]
    fn origins_omit_default_ports_and_lowercase() {
        assert_eq!(origin_of("HTTPS://Api.Groq.com:443/x").as_deref(), Some("https://api.groq.com"));
        assert_eq!(origin_of("http://h:80").as_deref(), Some("http://h"));
        assert_eq!(origin_of("http://h:8000/v1").as_deref(), Some("http://h:8000"));
        assert_eq!(origin_of("http://[::1]:8000/v1").as_deref(), Some("http://[::1]:8000"));
    }

    #[test]
    fn confirm_writes_the_shown_origin_and_nothing_else_does() {
        let prev = cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"));
        let save = |url: &str, prev: Option<&SttConfig>, confirm: Option<&str>| {
            apply_save(cfg(url, Some("https://frontend.example")), prev, None, confirm).map(|(c, _)| c.confirmed_host)
        };
        // Confirm of the origin the new URL has: written (the frontend's own confirmed_host is ignored).
        assert_eq!(save("https://api.openai.com/v1", Some(&prev), Some("https://api.openai.com")), Ok(Some("https://api.openai.com".into())));
        // No confirm, same origin: kept.
        assert_eq!(save("https://api.groq.com/other", Some(&prev), None), Ok(Some("https://api.groq.com".into())));
        // No confirm, origin changed (host, port or scheme): dropped.
        assert_eq!(save("https://api.openai.com/v1", Some(&prev), None), Ok(None));
        assert_eq!(save("https://api.groq.com:8443/v1", Some(&prev), None), Ok(None));
        assert_eq!(save("http://api.groq.com/v1", Some(&prev), None), Ok(None));
        // Nothing previously stored: stays unconfirmed, whatever the frontend said.
        assert_eq!(save("https://api.openai.com/v1", None, None), Ok(None));
    }

    #[test]
    fn a_confirm_for_another_address_is_refused_and_writes_nothing() {
        // The user was shown api.groq.com, then edited the URL before pressing Confirm.
        let prev = cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"));
        let edited = cfg("https://other.example/v1", None);
        assert_eq!(
            apply_save(edited.clone(), Some(&prev), Some("k"), Some("https://api.groq.com")).unwrap_err(),
            "the address changed — save again to confirm it"
        );
        // Port and scheme count as the address.
        let lan = cfg("http://192.168.1.5:9000/v1", None);
        assert!(apply_save(lan.clone(), None, None, Some("http://192.168.1.5:8000")).is_err());
        assert!(apply_save(cfg("http://api.groq.com/v1", None), None, None, Some("https://api.groq.com")).is_err());
        // A confirm for a URL that is not http(s) confirms nothing.
        assert!(apply_save(cfg("nonsense", None), None, None, Some("nonsense")).is_err());
        // Case in the shown origin does not matter.
        assert!(apply_save(lan, None, None, Some("HTTP://192.168.1.5:9000")).is_ok());
    }

    #[test]
    fn the_stored_key_does_not_follow_the_user_to_another_origin() {
        let prev = cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"));
        let to = |url: &str, key: Option<&str>, confirm: Option<&str>| {
            apply_save(cfg(url, None), Some(&prev), key, confirm).map(|(_, k)| k)
        };
        // Moved to OpenAI, confirmed, no new key: cleared.
        assert_eq!(to("https://api.openai.com/v1", None, Some("https://api.openai.com")), Ok(KeyOp::Clear));
        // Moved but not confirmed: cleared too, so a later confirm cannot carry it.
        assert_eq!(to("https://api.openai.com/v1", None, None), Ok(KeyOp::Clear));
        // A new key in the same call wins — but the old one is cleared before
        // the address moves (see `save_steps`).
        assert_eq!(to("https://api.openai.com/v1", Some(" new "), Some("https://api.openai.com")), Ok(KeyOp::Replace("new".into())));
        // Same origin, a new key: a plain set.
        assert_eq!(to("https://api.groq.com/other", Some("k2"), None), Ok(KeyOp::Set("k2".into())));
        // Same origin: kept; explicit empty clears.
        assert_eq!(to("https://api.groq.com/other", None, None), Ok(KeyOp::Keep));
        assert_eq!(to("https://api.groq.com/other", Some(""), None), Ok(KeyOp::Clear));
        // First setup (nothing saved before): nothing to clear.
        assert_eq!(apply_save(cfg("https://api.groq.com/v1", None), None, None, Some("https://api.groq.com")).map(|(_, k)| k), Ok(KeyOp::Keep));
        // A saved-but-never-confirmed address that stays the same keeps its key.
        let unconfirmed = cfg("https://api.groq.com/v1", None);
        assert_eq!(
            apply_save(cfg("https://api.groq.com/v1", None), Some(&unconfirmed), None, Some("https://api.groq.com")).map(|(_, k)| k),
            Ok(KeyOp::Keep)
        );
    }

    /// A new key is never paired with the OLD confirmed origin: when the
    /// address moves, the old key goes first, the config second, and the new
    /// key last — a failure at any step leaves no key reachable by the wrong host.
    #[test]
    fn a_moved_address_clears_the_old_key_before_the_config_and_sets_the_new_one_last() {
        let prev = cfg("https://api.groq.com/openai/v1", Some("https://api.groq.com"));
        let steps = |url: &str, key: Option<&str>, confirm: Option<&str>| {
            let (c, k) = apply_save(cfg(url, None), Some(&prev), key, confirm).unwrap();
            save_steps(c, k)
                .into_iter()
                .map(|s| match s {
                    SaveStep::ClearKey => "clear".to_string(),
                    SaveStep::WriteConfig(c) => format!("config {}", c.confirmed_host.unwrap_or_default()),
                    SaveStep::SetKey(k) => format!("set {k}"),
                })
                .collect::<Vec<_>>()
        };
        let openai = Some("https://api.openai.com");
        assert_eq!(steps("https://api.openai.com/v1", Some("new"), openai), ["clear", "config https://api.openai.com", "set new"]);
        assert_eq!(steps("https://api.openai.com/v1", None, openai), ["clear", "config https://api.openai.com"]);
        assert_eq!(steps("https://api.groq.com/v1", Some(""), None), ["clear", "config https://api.groq.com"]);
        assert_eq!(steps("https://api.groq.com/v1", Some("k2"), None), ["config https://api.groq.com", "set k2"]);
        assert_eq!(steps("https://api.groq.com/v1", None, None), ["config https://api.groq.com"]);
    }

    #[test]
    fn the_wire_names_and_defaults_are_stable() {
        let parsed: SttConfig = serde_json::from_str(r#"{"preset":"openai"}"#).unwrap();
        assert_eq!(parsed.preset, SttPreset::OpenAi);
        assert_eq!(parsed.max_upload_mb, 25);
        assert_eq!(serde_json::to_string(&SttPreset::Groq).unwrap(), "\"groq\"");
        assert_eq!(serde_json::to_string(&SttPreset::Custom).unwrap(), "\"custom\"");
    }

    #[test]
    fn denied_or_failed_reads_are_not_absence() {
        assert_eq!(decode_secret(Err(keyring_core::Error::NoEntry)).unwrap(), None);
        assert_eq!(decode_secret(Ok(String::new())).unwrap(), None);
        assert_eq!(decode_secret(Ok("k".into())).unwrap().as_deref(), Some("k"));
        let e = decode_secret(Err(keyring_core::Error::BadEncoding(b"secret-marker".to_vec()))).unwrap_err();
        assert!(!e.contains("secret-marker"));
    }

    #[test]
    fn the_bundled_clip_is_a_small_mp4() {
        assert_eq!(&TEST_CLIP[4..8], b"ftyp");
        assert!(TEST_CLIP.len() < 60 * 1024);
    }
}
