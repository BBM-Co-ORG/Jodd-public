// Multi-account model. Each Account represents one signed-in Gmail user.
// AccountId = email address — stable, human-readable, unique per Google account.
//
// Storage layout:
//   accounts.json (filesystem)      → list of Account metadata (email, added_at, ...)
//   keychain "jodd" / "rt::<email>" → that account's refresh token
//   AppState.account_states         → live access tokens + caches (in-memory only)
//
// The legacy single-account install (where the keychain entry was just "refresh_token"
// with no email suffix) auto-migrates to a first multi-account on launch — see
// migrate_legacy_keychain() below.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

pub type AccountId = String;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    pub id: AccountId,                // = email
    pub email: String,
    pub added_at: String,             // ISO 8601

    // ─── Per-account label configuration ─────────────────────────────────
    //
    // notes_label: the Gmail label (or label path) Apple Notes uses for
    // this account's notes. Default "Notes" — what Apple itself creates.
    // Configurable so a user with an existing custom Apple setup (or a
    // separate Jodd-only workflow) can point at something else. Strongly
    // recommend keeping "Notes" for cross-device interop with Apple Notes.
    //
    // meta_label: the Gmail label used for Jodd-managed sidecar messages
    // (per-note metadata like pin state). Default "Notes-Meta". Lives at
    // the top level (not under Notes/) so Apple Notes doesn't enumerate
    // it and doesn't trash sidecars during its sync. Sidecar messages in
    // this label have a Subject prefixed with the sentinel "___<uuid>" so
    // a user who manually drops a real note here won't be mistaken for
    // metadata by the pull-side reader.
    //
    // Both fields are #[serde(default)] so accounts.json files written
    // before this migration continue to parse — load_settings_for resolves
    // None to the default constants.
    #[serde(default)]
    pub notes_label: Option<String>,
    #[serde(default)]
    pub meta_label: Option<String>,
}

/// Default value for `notes_label` when an Account leaves it unset.
/// Apple Notes creates this label itself on the user's first sync, so
/// using it gets cross-device interop "for free."
pub const DEFAULT_NOTES_LABEL: &str = "Notes";

/// Default value for `meta_label` when an Account leaves it unset.
/// Top-level (no "Notes/" prefix) so Apple Notes' label enumeration
/// — which scopes to `Notes` and its descendants — doesn't see it.
pub const DEFAULT_META_LABEL: &str = "Notes-Meta";

/// User-visible projection of an Account's settings. The Tauri command
/// layer maps Option<String> → String here so the frontend doesn't have
/// to know about the "unset = use default" rule.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountSettings {
    pub notes_label: String,
    pub meta_label: String,
}

impl Account {
    pub fn effective_notes_label(&self) -> &str {
        self.notes_label.as_deref().unwrap_or(DEFAULT_NOTES_LABEL)
    }
    pub fn effective_meta_label(&self) -> &str {
        self.meta_label.as_deref().unwrap_or(DEFAULT_META_LABEL)
    }
    pub fn settings(&self) -> AccountSettings {
        AccountSettings {
            notes_label: self.effective_notes_label().to_string(),
            meta_label: self.effective_meta_label().to_string(),
        }
    }
}

#[derive(Default)]
pub struct AccountState {
    pub access_token: Option<String>,
    // When the current access_token stops being valid. Google access tokens
    // last ~3600s; we proactively refresh ~60s before expiry to avoid
    // 401 UNAUTHENTICATED errors mid-session.
    pub token_expires_at: Option<std::time::Instant>,
    pub label_map_cache: Option<(HashMap<String, String>, std::time::Instant)>,
    // Per-account async lock that coalesces concurrent label_map refreshes.
    // Without it, two callers finding the cache stale at the same time would
    // both fire gmail::get_label_map; their writes race and the later one
    // clobbers the earlier — corruption window if Apple Notes added/removed
    // a label between the two fetches. Held only across the network call,
    // not the in-memory read path (cache hits never touch this lock).
    pub label_map_refresh: std::sync::Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default, Serialize, Deserialize)]
struct AccountsFile {
    accounts: Vec<Account>,
}

// ─── Filesystem paths ────────────────────────────────────────────────────────

// Returns the app's config directory, creating it if needed.
// macOS: ~/Library/Application Support/jodd
// Linux: ~/.config/jodd
// Windows: %APPDATA%/jodd
fn config_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or("no config dir on this OS")?;
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    Ok(dir)
}

fn accounts_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("accounts.json"))
}

// ─── Load / Save ─────────────────────────────────────────────────────────────

pub fn load_accounts() -> Vec<Account> {
    match accounts_path().and_then(|p| {
        if !p.exists() {
            return Ok(Vec::new());
        }
        let txt = fs::read_to_string(&p).map_err(|e| format!("read {}: {}", p.display(), e))?;
        let f: AccountsFile =
            serde_json::from_str(&txt).map_err(|e| format!("parse: {}", e))?;
        Ok(f.accounts)
    }) {
        Ok(list) => list,
        Err(e) => {
            eprintln!("[jodd] load_accounts failed: {}", e);
            Vec::new()
        }
    }
}

pub fn save_accounts(accounts: &[Account]) -> Result<(), String> {
    let p = accounts_path()?;
    let f = AccountsFile {
        accounts: accounts.to_vec(),
    };
    let txt = serde_json::to_string_pretty(&f).map_err(|e| format!("encode: {}", e))?;
    fs::write(&p, txt).map_err(|e| format!("write {}: {}", p.display(), e))?;
    Ok(())
}

// ─── Keychain key per account ────────────────────────────────────────────────

const KC_SERVICE: &str = "jodd";
const LEGACY_KEY: &str = "refresh_token";

fn keychain_key(account_id: &str) -> String {
    format!("rt::{}", account_id)
}

pub fn load_refresh_token(account_id: &str) -> Option<String> {
    let entry = keyring::Entry::new(KC_SERVICE, &keychain_key(account_id)).ok()?;
    entry.get_password().ok()
}

pub fn save_refresh_token(account_id: &str, token: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(KC_SERVICE, &keychain_key(account_id))
        .map_err(|e| format!("keychain open: {}", e))?;
    entry
        .set_password(token)
        .map_err(|e| format!("keychain write: {}", e))
}

pub fn delete_refresh_token(account_id: &str) {
    if let Ok(entry) = keyring::Entry::new(KC_SERVICE, &keychain_key(account_id)) {
        let _ = entry.delete_password();
    }
}

// ─── Legacy single-account migration ─────────────────────────────────────────
// Old install path: keychain at ("jodd", "refresh_token") with no account id.
// On startup, if we find a legacy token AND no accounts.json exists yet,
// preserve the token under a temporary id and let the caller finish migration
// after a getProfile call resolves the actual email.

pub fn take_legacy_refresh_token() -> Option<String> {
    let entry = keyring::Entry::new(KC_SERVICE, LEGACY_KEY).ok()?;
    let token = entry.get_password().ok()?;
    // Remove the legacy entry — we'll re-save under the email-keyed path.
    let _ = entry.delete_password();
    Some(token)
}
