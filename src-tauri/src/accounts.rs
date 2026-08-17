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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Gmail,
    LocalFs,
    Microsoft,
}

/// Where an account sits in its lifecycle.
///
/// Jodd is a write-back cache: edits land in SQLite synchronously and reach
/// the backend when the worker gets to them. Deactivating is therefore a
/// quiesce, not a switch — stop taking new work, flush what is queued, then go
/// quiet. `Draining` is that middle phase, and it is why this is not a bool.
///
/// Only the user moves Active -> Draining and Inactive -> Active. Only the
/// worker moves Draining -> Inactive, when every outbound queue is empty —
/// which is what makes `Inactive` a guarantee that nothing is pending rather
/// than merely a label.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    #[default]
    Active,
    /// Hidden from the user, still pushing. Not refused by `vertical_for`.
    Draining,
    /// Hidden and silent. Refused by `vertical_for`; skipped by the worker.
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    /// Serde default. Since v0.21 this means **inherit the app-level
    /// provider** (see app_llm_config + llm::resolve), NOT "unconfigured".
    /// Existing accounts.json files parse unchanged and become inheritors,
    /// which is the intended upgrade behavior.
    #[default]
    None,
    /// Explicit opt-out: never run LLM workflows for this account, even when
    /// an app-level provider exists. Distinct from `None` on purpose — the
    /// old single "unset" state could not express this.
    Disabled,
    /// Legacy: pre-v0.19 accounts.json. Resolved to the `claude` agent-CLI
    /// preset at read time; the file is never rewritten.
    ClaudeCode,
    Http,
    AgentCli,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    #[serde(default)]
    pub provider: LlmProviderKind,
    #[serde(default)]
    pub http_base_url: Option<String>,
    #[serde(default)]
    pub http_model: Option<String>,
    /// Keychain key name (not the value!). Format: "llm_api_key::{account_id}".
    /// Stored in keychain under service=`jodd`, key=this value.
    #[serde(default)]
    pub http_api_key_keychain: Option<String>,
    /// Agent-CLI preset id, or the literal "custom".
    #[serde(default)]
    pub agent_preset: Option<String>,
    /// Only read when `agent_preset == Some("custom")`.
    #[serde(default)]
    pub agent_custom: Option<crate::llm::agent_cli::AgentCliSpec>,
}

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

    // ─── Per-account LLM provider configuration ─────────────────────────
    // Used by the lesson-extraction feature. API keys NEVER live here —
    // only the keychain key name is stored; the secret lives in the OS
    // keychain. #[serde(default)] keeps pre-LLM accounts.json files
    // parsing cleanly.
    #[serde(default)]
    pub llm: LlmConfig,

    // ─── Backend kind + LocalFS config ───────────────────────────────────
    // backend_kind: Gmail (default, backward-compatible) or LocalFs.
    // root_dir: absolute path to the notes root for LocalFs accounts.
    // Both are #[serde(default)] so existing accounts.json files (which
    // have neither field) continue to deserialize cleanly as Gmail accounts
    // with no root_dir — no migration needed.
    #[serde(default)]
    pub backend_kind: BackendKind,
    #[serde(default)]
    pub root_dir: Option<String>,

    /// Lifecycle state. `#[serde(default)]` resolves to `Active`, so every
    /// accounts.json written before this feature parses unchanged.
    #[serde(default)]
    pub status: AccountStatus,
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
    /// Local readiness — NEVER touches the network or keychain network I/O.
    /// True if the account is usable from local state alone.
    /// (Data doctrine: readiness ≠ network.)
    ///
    /// - Gmail: a refresh token in the OS keychain is sufficient — the keychain
    ///   read is local and never involves a network call.
    /// - LocalFs: the configured root_dir must exist as a directory on disk.
    pub fn is_ready_local(&self) -> bool {
        match self.backend_kind {
            BackendKind::Gmail => load_refresh_token(&self.id).is_some(),
            BackendKind::Microsoft => load_refresh_token(&self.id).is_some(),
            BackendKind::LocalFs => self
                .root_dir
                .as_ref()
                .map(|d| std::path::Path::new(d).is_dir())
                .unwrap_or(false),
        }
    }

    /// True only for `Active`. Draining and Inactive are both hidden from the
    /// user, and callers that ask "should this account appear?" mean this.
    pub fn is_active(&self) -> bool {
        self.status == AccountStatus::Active
    }

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
    //
    // Wall-clock (SystemTime), not monotonic (Instant): on macOS, Instant is
    // backed by CLOCK_UPTIME_RAW which pauses while the machine sleeps. If
    // the laptop sleeps past the token's lifetime, Instant thinks no time
    // passed and ensure_token's fast path returns a token Google has already
    // expired — surfacing as a 401 UNAUTHENTICATED on the next API call.
    pub token_expires_at: Option<std::time::SystemTime>,
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

// `_under(base)` variants take the base dir as a parameter so tests can point
// them at a tempdir — same pattern as applog.rs.
fn config_dir_under(base: &std::path::Path) -> Result<PathBuf, String> {
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    Ok(dir)
}

// Returns the app's config directory, creating it if needed.
// macOS: ~/Library/Application Support/jodd
// Linux: ~/.config/jodd
// Windows: %APPDATA%/jodd
// Android: <app-private config dir>/jodd (set via paths::init in setup())
fn config_dir() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    config_dir_under(&base)
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
    let entry = keyring_core::Entry::new(KC_SERVICE, &keychain_key(account_id)).ok()?;
    entry.get_password().ok()
}

/// Presence check only — does the keychain hold a refresh token for this
/// account? Reads the keychain (local) but NEVER refreshes (no network). Used
/// by the offline-safe readiness gate so a cold start can't block on Gmail.
pub fn has_refresh_token(account_id: &str) -> bool {
    load_refresh_token(account_id).is_some()
}

pub fn save_refresh_token(account_id: &str, token: &str) -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, &keychain_key(account_id))
        .map_err(|e| format!("keychain open: {}", e))?;
    entry
        .set_password(token)
        .map_err(|e| format!("keychain write: {}", e))
}

pub fn delete_refresh_token(account_id: &str) {
    if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &keychain_key(account_id)) {
        let _ = entry.delete_credential();
    }
}

// ─── Legacy single-account migration ─────────────────────────────────────────
// Old install path: keychain at ("jodd", "refresh_token") with no account id.
// On startup, if we find a legacy token AND no accounts.json exists yet,
// preserve the token under a temporary id and let the caller finish migration
// after a getProfile call resolves the actual email.

pub fn take_legacy_refresh_token() -> Option<String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, LEGACY_KEY).ok()?;
    let token = entry.get_password().ok()?;
    // Remove the legacy entry — we'll re-save under the email-keyed path.
    let _ = entry.delete_credential();
    Some(token)
}

// ─── LLM API key keychain helpers ────────────────────────────────────────────
// Same KC_SERVICE ("jodd") as refresh tokens, but a distinct key prefix so the
// two never collide. The secret is the raw API key string; the keychain key
// name itself is what's stored in accounts.json (see LlmConfig).

/// Build the keychain key for an account's LLM API key.
pub fn llm_keychain_key(account_id: &str) -> String {
    format!("llm_api_key::{}", account_id)
}

/// Read the LLM API key from keychain. Returns None if not set.
pub fn read_llm_api_key(account_id: &str) -> Option<String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, &llm_keychain_key(account_id)).ok()?;
    entry.get_password().ok()
}

/// Write the LLM API key to keychain.
pub fn write_llm_api_key(account_id: &str, key: &str) -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, &llm_keychain_key(account_id))
        .map_err(|e| format!("keychain open: {e}"))?;
    entry
        .set_password(key)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Remove the LLM API key from keychain (e.g. on provider change to None).
pub fn delete_llm_api_key(account_id: &str) {
    if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &llm_keychain_key(account_id)) {
        let _ = entry.delete_credential();
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// Every test in this file that can reach the real OS credential store
// (`keyring_core::Entry::new`), directly or via `Account::is_ready_local()`,
// must call `crate::secrets::init()` first. keyring-core has no lazy
// fallback: `Entry::new` fails with `NoDefaultStore` until a store is
// registered, and `cargo test` never runs `run()` (which is where
// `secrets::init()` normally runs, once, before anything is spawned). Unlike
// the keyring-4 v1 shim this superseded, `secrets::init()` is `Once`-backed
// and blocks concurrent callers until registration completes, so no shared
// test-only warm-up is needed — every call site simply calls
// `crate::secrets::init()` itself, and repeated calls are cheap (idempotent,
// returns the first call's outcome).

#[cfg(test)]
mod is_ready_local_tests {
    use super::*;

    fn make_account(backend_kind: BackendKind, root_dir: Option<String>) -> Account {
        Account {
            id: "test@example.com".to_string(),
            email: "test@example.com".to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind,
            root_dir,
            status: AccountStatus::Active,
        }
    }

    #[test]
    fn localfs_existing_dir_is_ready() {
        // std::env::temp_dir() always exists on every supported OS.
        let dir = std::env::temp_dir().to_string_lossy().to_string();
        let account = make_account(BackendKind::LocalFs, Some(dir));
        assert!(account.is_ready_local(), "existing temp dir should be ready");
    }

    #[test]
    fn localfs_nonexistent_path_is_not_ready() {
        let bogus = "/nonexistent/jodd/test/path/that/cannot/exist".to_string();
        let account = make_account(BackendKind::LocalFs, Some(bogus));
        assert!(!account.is_ready_local(), "nonexistent path should not be ready");
    }

    #[test]
    fn localfs_no_root_dir_is_not_ready() {
        let account = make_account(BackendKind::LocalFs, None);
        assert!(!account.is_ready_local(), "LocalFs with no root_dir should not be ready");
    }

    #[test]
    fn gmail_with_no_keychain_token_is_not_ready() {
        // Touches the real credential store via is_ready_local() ->
        // load_refresh_token() -> keyring_core::Entry::new, which fails with
        // NoDefaultStore until a store is registered. This assertion cannot
        // fail either way (store error or genuine absence both yield
        // `false`), so a missing init() call here would not announce itself
        // as a failure — it would just make the suite flaky depending on
        // test ordering. Call it anyway so this test's result reflects real
        // "no entry" semantics, not an uninitialized store.
        crate::secrets::init().expect("credential store must initialize");
        // An email that can't have a keychain entry in CI — absence = not ready.
        let account = make_account(BackendKind::Gmail, None);
        // We can't assert true (that would require a real keychain entry), but
        // we can confirm the Gmail branch runs without panicking and returns a bool.
        let result = account.is_ready_local();
        // No keychain entry for a throwaway id → false in any real environment.
        assert!(
            !result,
            "Gmail account with no keychain entry should report not ready"
        );
    }

    #[test]
    fn backend_kind_default_is_gmail() {
        let kind = BackendKind::default();
        assert_eq!(kind, BackendKind::Gmail);
    }

    #[test]
    fn old_account_json_deserializes_as_gmail() {
        // Simulate an accounts.json written before BackendKind existed —
        // no backend_kind or root_dir fields present.
        let json = r#"{
            "id": "old@example.com",
            "email": "old@example.com",
            "added_at": "2025-01-01T00:00:00Z"
        }"#;
        let acc: Account = serde_json::from_str(json).expect("should parse old format");
        assert_eq!(acc.backend_kind, BackendKind::Gmail, "old accounts default to Gmail");
        assert!(acc.root_dir.is_none(), "old accounts have no root_dir");
    }

    #[test]
    fn microsoft_backend_kind_round_trips_as_snake_case() {
        let json = serde_json::to_string(&BackendKind::Microsoft).unwrap();
        assert_eq!(json, "\"microsoft\"", "wire form must be snake_case");
        let back: BackendKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, BackendKind::Microsoft);
    }
}

#[cfg(test)]
mod account_status_tests {
    use super::*;

    /// The upgrade path. Every accounts.json in the wild predates `status`;
    /// if it does not default to Active, every account on every install
    /// switches itself off on upgrade.
    #[test]
    fn an_accounts_json_without_status_parses_as_active() {
        let json = r#"{
            "id": "a@example.com",
            "email": "a@example.com",
            "added_at": "2026-01-01T00:00:00Z"
        }"#;
        let a: Account = serde_json::from_str(json).expect("legacy account should parse");
        assert_eq!(a.status, AccountStatus::Active);
        assert!(a.is_active());
    }

    #[test]
    fn status_round_trips_as_snake_case() {
        let json = serde_json::to_string(&AccountStatus::Draining).unwrap();
        assert_eq!(json, "\"draining\"");
        let back: AccountStatus = serde_json::from_str("\"inactive\"").unwrap();
        assert_eq!(back, AccountStatus::Inactive);
    }

    #[test]
    fn only_active_is_active() {
        let mut a: Account = serde_json::from_str(
            r#"{"id":"a","email":"a","added_at":"x"}"#,
        )
        .unwrap();
        a.status = AccountStatus::Draining;
        assert!(!a.is_active());
        a.status = AccountStatus::Inactive;
        assert!(!a.is_active());
    }
}

#[cfg(test)]
mod keychain_roundtrip_tests {
    use super::*;

    // Characterization tests for the credential-store migration: a token
    // written through our wrappers must read back byte-identical from the real
    // platform store. Deliberately NOT mocked — a mock would prove nothing
    // about whether the migration preserved real platform behavior, which is
    // the entire question. Throwaway `.invalid` ids, cleaned up after.
    //
    // Every test here calls secrets::init() first: keyring-core has no lazy
    // fallback, so Entry::new fails with NoDefaultStore until a store is
    // registered, and `cargo test` never runs `run()`.

    #[test]
    fn refresh_token_roundtrips_through_the_platform_store() {
        crate::secrets::init().expect("credential store must initialize");
        let acct = "jodd-test-roundtrip@example.invalid";
        let token = "test-refresh-token-value-12345";

        save_refresh_token(acct, token).expect("write should succeed");
        let read_back = load_refresh_token(acct);
        delete_refresh_token(acct);

        assert_eq!(read_back.as_deref(), Some(token));
    }

    #[test]
    fn deleted_refresh_token_is_gone() {
        crate::secrets::init().expect("credential store must initialize");
        let acct = "jodd-test-delete@example.invalid";
        save_refresh_token(acct, "throwaway").expect("write should succeed");
        delete_refresh_token(acct);
        assert_eq!(load_refresh_token(acct), None);
    }
}
