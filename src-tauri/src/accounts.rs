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
use std::sync::{Mutex, OnceLock};

use crate::log;

pub type AccountId = String;

/// The id namespace for a backend.
///
/// Deliberately an explicit match rather than serde's rendering: `BackendKind`
/// is `rename_all = "snake_case"`, so serde would spell `LocalFs` as
/// `local_fs` — but LocalFs accounts already exist on disk with `localfs:`
/// ids (`lib.rs`'s `add_local_account`). Deriving this from serde would
/// silently orphan every existing local vault.
pub fn backend_prefix(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Gmail => "gmail",
        BackendKind::LocalFs => "localfs",
        BackendKind::Microsoft => "microsoft",
        BackendKind::ICloud => "icloud",
    }
}

/// The only thing in the process that mints an account id.
///
/// Takes `BackendKind`, never a string, so a typo cannot invent a namespace.
/// Every producer calls this — sign-in, local-vault creation, migration #19,
/// tests — which is what keeps the migration and the live path from drifting
/// into disagreeing about what an id looks like.
pub fn account_id_for(kind: BackendKind, email: &str) -> AccountId {
    format!("{}:{}", backend_prefix(kind), email)
}

/// Whether `id` already carries a known backend prefix.
///
/// This is what makes migration #19 safe to re-run. It is not decoration:
/// `Db::migrate` records a migration's version in a statement *separate* from
/// the migration itself, so a crash in between re-runs a completed migration
/// on the next start.
pub fn is_qualified(id: &str) -> bool {
    matches!(
        id.split_once(':'),
        Some((prefix, _)) if ALL_BACKENDS.iter().any(|k| backend_prefix(*k) == prefix)
    )
}

/// Every `BackendKind`, once.
///
/// The single list both [`is_qualified`] and [`legacy_bare_id`] read. They
/// each carried their own copy, which is one list too many for two functions
/// that must agree on exactly the same question — "is this prefix a backend?"
/// — from opposite directions (does an id have one / strip the one it has).
/// A backend added to one copy and not the other is a silent half-migration.
///
/// `BackendKind` cannot enumerate itself, so `all_backends_lists_every_variant`
/// is what keeps this in step with the enum: it matches exhaustively, so
/// adding a variant fails to compile until this array is extended too.
pub const ALL_BACKENDS: [BackendKind; 4] = [
    BackendKind::Gmail,
    BackendKind::LocalFs,
    BackendKind::Microsoft,
    BackendKind::ICloud,
];

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Gmail,
    LocalFs,
    Microsoft,
    /// Apple Notes read straight out of iCloud, via CloudKit's private
    /// database web service — the one backend that reaches an account with no
    /// non-iCloud address attached to Notes at all.
    ///
    /// `rename` rather than serde's `snake_case` rendering, which would spell
    /// this `i_cloud`. `backend_prefix` above is an explicit match for the
    /// same class of reason, and this keeps `accounts.json` agreeing with the
    /// id namespace instead of carrying two spellings of one backend.
    #[serde(rename = "icloud")]
    ICloud,
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
    /// Independent data permission; absent preserves legacy behavior. Legacy
    /// provider=disabled always denies, including when this is true.
    #[serde(default)]
    pub data_allowed: Option<bool>,
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
    /// When true the HTTP provider adds `chat_template_kwargs:
    /// {enable_thinking: false}` to every request, turning OFF the reasoning
    /// step on local llama.cpp + Qwen3-style models. Those spend hundreds to
    /// thousands of tokens "thinking" before the answer; on a consumer GPU at
    /// single-digit tok/s that blows past the request timeout and reaches the
    /// user as a Transport/MalformedEnvelope error rather than as slowness.
    /// Measured against a real llama.cpp server: thinking ON = 66s, OFF = 27s
    /// with clean JSON.
    ///
    /// It is a llama.cpp-specific body param, so it defaults to OFF and is
    /// omitted from the wire entirely when unset — hosted providers
    /// (Anthropic/OpenAI/Kilo) are byte-for-byte unaffected. Only the `Http`
    /// provider reads it; `agent_cli` shells out to a CLI that has its own
    /// flags for this.
    #[serde(default)]
    pub disable_thinking: bool,
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

    /// iCloud only: whether a webview sign-in has ever completed for this
    /// account. A **marker, not a credential** — no secret goes in
    /// accounts.json. The session itself lives in the webview's own persistent
    /// cookie store and is harvested just-in-time; Jodd never holds a copy,
    /// because a copy is stale by construction (a captured session measured
    /// dead inside 3.5 h, the live browser having rotated its cookies out from
    /// under it — docs/PRIOR-ART.md).
    ///
    /// It exists because `is_ready_local` is a synchronous `-> bool` polled
    /// every 2 s during sign-in (gotcha #15) and a webview cookie read is
    /// async and main-thread-bound, so asking the session directly is not an
    /// option at any price. A stale `true` is accepted for the same reason
    /// `RT_PRESENT`'s stale `true` is: the first CloudKit call surfaces the
    /// loss, exactly as a revoked refresh token already does. Do not "fix" it
    /// by asking the webview per call — that is the bug, not the cure.
    #[serde(default)]
    pub icloud_session_established: bool,

    /// A whole-account hard block a read discovered, or `None`.
    ///
    /// Today the only producer is the Advanced Data Protection verdict
    /// (`backend/icloud`): with ADP on, note bodies are genuinely end-to-end
    /// encrypted and the account cannot work at all. **ADP can be switched on
    /// after the account exists**, which is why this is stored per account and
    /// re-stamped on every index pass rather than being decided once at
    /// sign-in — the sign-in gate refuses to create a blocked account, and this
    /// field covers the case where a working one becomes blocked later.
    ///
    /// The message, not a code: whoever sets it knows what the user can
    /// actually do about it, and a code would need a second table mapping it
    /// back to that sentence. Generalizes to any future backend-level block —
    /// `Vertical::blocked_reason` is the trait side.
    #[serde(default)]
    pub blocked_reason: Option<String>,

    /// The backend's own resume token for an incremental pull, or `None`.
    ///
    /// **Opaque bytes, never parsed here.** It is whatever
    /// `Transport::changes_since` last handed back — on iCloud a CloudKit
    /// `syncToken`, on a future backend whatever that backend calls one. The
    /// core has never inspected a `SyncCursor` and must not start.
    ///
    /// Persisted rather than kept in memory for one reason: across a restart.
    /// An in-memory token makes the first pull after every launch a
    /// from-scratch read of the whole account; a stored one makes it a delta.
    ///
    /// **A cursor is a hint, never a source of truth.** Losing it, or storing
    /// a stale one, costs at most one extra full read — the authoritative pull
    /// (`list_notes`) reads everything regardless. Nothing may be pruned,
    /// deleted or believed on the strength of what a cursor did or did not
    /// report.
    #[serde(default)]
    pub sync_cursor: Option<String>,

    /// Jodd's own CRDT replica identity for this account, once minted.
    ///
    /// Minted lazily on the first CRDT-writable edit — not at sign-in, since
    /// M1/M2 accounts never needed one — and persisted immediately once it
    /// is. Stored as a UUID string; converted to the 16 raw bytes Apple's
    /// `VectorTimestamp.Clock.replicaUUID` field wants only where that's
    /// needed (`backend/icloud/crdt.rs`). Not a secret — analogous to a
    /// device id, not a credential — so it lives here rather than in the
    /// keychain, the same reasoning `sync_cursor` above already applies.
    /// Once minted, never regenerated: a second UUID would make every prior
    /// edit this replica made look like an unordered, unrelated replica to
    /// Apple's merge (docs/superpowers/specs/2026-08-25-icloud-vertical-m2.5-design.md).
    #[serde(default)]
    pub icloud_replica_id: Option<String>,

    /// Lifecycle state. `#[serde(default)]` resolves to `Active`, so every
    /// accounts.json written before this feature parses unchanged.
    #[serde(default)]
    pub status: AccountStatus,

    /// Set when the user asked to remove this account while it was still
    /// `Draining` (a non-empty outbound queue). `remove_account` used to
    /// refuse that request outright — deleting the credential first would
    /// strand the drain — leaving "wait" or "Stop waiting" (which force-flips
    /// to `Inactive` and strands the queue) as the only options. This is the
    /// gentler third path (roadmap #0d): the request is queued here instead
    /// of refused, and `sync_worker_tick` performs the real removal once
    /// `status` reaches `Inactive` the normal way — i.e. once
    /// `has_pending_pushes` genuinely returns false, same as any other
    /// Draining -> Inactive flip (gotcha #2's guarantee is exactly what makes
    /// this safe to act on unconditionally).
    ///
    /// `#[serde(default)]` resolves to `false`, so existing accounts.json
    /// files parse unchanged. Meaningless once an account is actually
    /// removed — the row (and this flag with it) is deleted, never persisted
    /// with it cleared.
    #[serde(default)]
    pub pending_removal: bool,
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
    /// - Gmail/Microsoft: a refresh token in the OS keychain is sufficient — the
    ///   keychain read is local and never involves a network call. Goes through
    ///   `has_refresh_token`, which memoizes presence; this method sits under a
    ///   command the frontend polls every 2 s, so it must not perform a
    ///   credential-store read per call (see `RT_PRESENT`).
    /// - ICloud: a plain bool on the account record. There is no token, and
    ///   the session lives in the webview — neither is reachable from a
    ///   synchronous predicate on this path (see `icloud_session_established`).
    /// - LocalFs: the configured root_dir must exist as a directory on disk.
    pub fn is_ready_local(&self) -> bool {
        match self.backend_kind {
            BackendKind::Gmail => has_refresh_token(&self.id),
            BackendKind::Microsoft => has_refresh_token(&self.id),
            // No refresh token exists on this backend at all — see
            // `icloud_session_established`. Reading a plain bool off the
            // account record is what keeps this path free of both the
            // credential store and the webview.
            BackendKind::ICloud => self.icloud_session_established,
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

    /// Returns this account's CRDT replica id, minting and persisting a
    /// fresh one if it has never had one (or if the stored value doesn't
    /// parse as a UUID — treated the same as absent, so a corrupted field
    /// re-mints rather than panics).
    pub fn ensure_icloud_replica_id(&mut self) -> [u8; 16] {
        if let Some(existing) = self.icloud_replica_id.as_deref().and_then(parse_replica_id) {
            return existing;
        }
        let minted = uuid::Uuid::new_v4();
        self.icloud_replica_id = Some(minted.to_string());
        *minted.as_bytes()
    }
}

fn parse_replica_id(s: &str) -> Option<[u8; 16]> {
    uuid::Uuid::parse_str(s).ok().map(|u| *u.as_bytes())
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

/// Write the account list **atomically**: full contents into a sibling
/// `accounts.json.tmp`, then a single `rename` over the real file.
///
/// A plain `fs::write` truncates before it writes, so a crash, a full disk or
/// a killed process mid-write leaves a half-written `accounts.json`. That is
/// not a recoverable inconvenience: [`load_accounts`] swallows every error and
/// returns an empty `Vec`, so a truncated file reads as *"this user has no
/// accounts"* — every cached note becomes unreachable, and migration #19's
/// fail-closed abort (`db::migrate_account_ids_with`) then matches no account
/// for any id and refuses forever. `rename` within one directory is atomic on
/// every target platform, so a concurrent reader sees the old file or the new
/// one, never a prefix of either.
pub fn save_accounts(accounts: &[Account]) -> Result<(), String> {
    let p = accounts_path()?;
    let f = AccountsFile {
        accounts: accounts.to_vec(),
    };
    let txt = serde_json::to_string_pretty(&f).map_err(|e| format!("encode: {}", e))?;
    write_atomically(&p, &txt)
}

/// Write `contents` to `path` via a sibling temp file and a single `rename`.
///
/// Split out from [`save_accounts`] only so it is testable: `save_accounts`
/// resolves its path through `paths::config_base()`, a process-global
/// `OnceLock` a unit test must not point at a tempdir (see
/// `db::connection_is_the_live_user_cache`). This takes the path.
fn write_atomically(path: &std::path::Path, contents: &str) -> Result<(), String> {
    // Same directory as the target, so the rename cannot cross a filesystem.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    fs::rename(&tmp, path).map_err(|e| {
        // Leave no stray .tmp behind for a later run to trip over.
        let _ = fs::remove_file(&tmp);
        format!("rename {} -> {}: {}", tmp.display(), path.display(), e)
    })?;
    Ok(())
}

// ─── Keychain key per account ────────────────────────────────────────────────

const KC_SERVICE: &str = "jodd";
const LEGACY_KEY: &str = "refresh_token";

// ─── Keychain access serialization ─────────────────────────────────────────
//
// 2026-09-09: two Microsoft accounts signed in back to back on the same
// Android device ended up with each other's refresh tokens under their own
// keychain keys — `load_refresh_token("microsoft:a@x.com")` returned a token
// that, once refreshed, authenticated Graph calls as `microsoft:b@y.com` and
// vice versa. Confirmed on a completely cold app restart (no in-memory state
// survives a process kill), so the values themselves were already swapped in
// storage, not merely raced on read. Every OTHER layer on the path —
// `keychain_key`'s per-account string, `MicrosoftVertical`'s per-instance
// token, `vertical_for`'s fresh construction per call — was read end to end
// and is correctly scoped; `android-native-keyring-store`'s own `build()`
// derives a literal, unhashed, per-(service,user) key with no collision for
// these two accounts either. What the two accounts DO share is concurrent
// access to one Android SharedPreferences-backed vault, and this session's
// evidence (both accounts' token refreshes landing within a second of app
// launch, both their `keychain WRITE` lines a fraction of a second apart)
// points at a race in that shared native resource rather than a single line
// of Rust logic to fix.
//
// Not proven to the level the rest of this codebase requires before calling
// something root-caused — an Android JNI race needs an Android target to
// observe directly, the way `db_crypto`'s own Android proof needs a real
// emulator (CLAUDE.md's dev-commands section) — so this is defense-in-depth,
// not a confirmed fix: it removes the concurrent-access window the evidence
// points at, on every platform, at negligible cost (auth operations, not a
// hot path). If a live Android re-test with this lock in place still shows
// the swap, the mechanism is elsewhere and this comment's theory is wrong —
// that would need investigating with a patched local copy of
// `android-native-keyring-store` rather than more log capture from this repo.
static KEYCHAIN_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with every other keychain-touching call in this process excluded.
/// See the module comment above `KEYCHAIN_LOCK` for why this exists.
fn with_keychain_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = KEYCHAIN_LOCK.lock().unwrap();
    f()
}

fn keychain_key(account_id: &str) -> String {
    format!("rt::{}", account_id)
}

/// The pre-`{backend}:` form of a qualified id — what a credential saved
/// before this change is filed under. `None` for an id that is already bare,
/// since a bare id has no legacy form to fall back to: it IS the legacy form.
///
/// `pub` because `jodd-mcp` needs the same answer for `mcp_write_scope.json`,
/// whose keys are hand-written account ids that nothing migrates
/// (`scope::WriteScope::allowed_folders`). One definition of "what did this id
/// used to be called" for every store keyed by account id.
pub fn legacy_bare_id(account_id: &str) -> Option<String> {
    account_id
        .split_once(':')
        .filter(|(prefix, _)| ALL_BACKENDS.iter().any(|k| backend_prefix(*k) == *prefix))
        .map(|(_, rest)| rest.to_string())
}

/// The legacy refresh-token keychain key for a bare email — `rt::{bare}`.
/// Equal to `keychain_key(bare)` by construction; kept as its own name
/// because "this is the legacy key" is the fact call sites care about, not
/// the format string.
fn legacy_key_for(bare: &str) -> String {
    keychain_key(bare)
}

/// Read a secret from the credential store, trying the id as given first and
/// falling back to the pre-migration entry filed under the account's bare
/// email — only reached when the first read misses AND the id is qualified
/// (gotcha #15: this sits *inside* `load_refresh_token`/`read_llm_api_key`,
/// downstream of `RT_PRESENT`'s cache check in `has_refresh_token`, so a
/// cache hit never reaches this function at all — see that static's doc
/// comment).
///
/// `build_key` turns the id as given into the full keychain key
/// (`keychain_key` for refresh tokens, `llm_keychain_key` for LLM API keys);
/// `build_legacy_key` turns the bare email into the pre-migration key
/// (`legacy_key_for` for refresh tokens — `llm_keychain_key` again for LLM
/// keys, since that prefix never changed shape, only the id it's applied
/// to). Two params rather than one so each credential kind names its own
/// legacy format instead of the fallback control flow assuming they match.
fn read_secret_with_legacy_fallback(
    account_id: &str,
    build_key: impl Fn(&str) -> String,
    build_legacy_key: impl Fn(&str) -> String,
) -> Option<String> {
    let key = build_key(account_id);
    log!("keychain READ   {}/{}", KC_SERVICE, key);
    if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &key) {
        if let Ok(pw) = entry.get_password() {
            return Some(pw);
        }
    }
    // Pre-migration entry, filed under the bare email.
    let bare = legacy_bare_id(account_id)?;
    let legacy_key = build_legacy_key(&bare);
    log!("keychain READ   {}/{} (legacy fallback)", KC_SERVICE, legacy_key);
    let entry = keyring_core::Entry::new(KC_SERVICE, &legacy_key).ok()?;
    let secret = entry.get_password().ok()?;

    // Copy it onto the qualified key, so this account stops paying for two
    // entries from here on.
    //
    // The original design said "the next successful save completes the move"
    // and left it at that. Measured on a real profile (2026-08-21): that save
    // can never come. `save_refresh_token` runs only when a token rotates, and
    // Google does not return a fresh refresh token on every exchange — so
    // across four launches the Microsoft account healed itself via rotation
    // while the Gmail account read through the fallback every single time. On
    // macOS each surviving legacy entry is one extra authorization prompt
    // whenever the keychain ACL changes (gotcha #15 — every `cargo` rebuild),
    // permanently.
    //
    // This does NOT reintroduce the migration moment the design rejected. The
    // write happens only AFTER a successful read, so a write that fails leaves
    // the legacy entry serving exactly as it did a moment ago: there is no
    // state in which this can sign a user out, which was the whole objection.
    //
    // The legacy entry is deliberately not deleted. Deleting is the direction
    // that can lose a credential, and `delete_secret_and_its_legacy_entry`
    // already removes both when the account is genuinely removed.
    match keyring_core::Entry::new(KC_SERVICE, &key).and_then(|e| e.set_password(&secret)) {
        Ok(()) => log!("keychain WRITE  {}/{} (healed from legacy)", KC_SERVICE, key),
        Err(e) => log!(
            "keychain WRITE  {}/{} failed: {} — legacy entry still serves",
            KC_SERVICE, key, e
        ),
    }
    Some(secret)
}

/// Delete a secret under the id as given **and** under the pre-migration bare
/// email, mirroring [`read_secret_with_legacy_fallback`].
///
/// The read side falls back to the legacy entry, so a delete that removes only
/// the qualified one does not delete anything the user can observe: the next
/// read finds the legacy entry and returns it. Deletion has to cover exactly
/// the set the read covers, or "delete" means "hide until the next read".
///
/// Both deletes are best-effort. `delete_credential` fails when there was no
/// entry to remove, which is the common case for the legacy key on any profile
/// created after migration #19 — absent either way is the outcome wanted.
fn delete_secret_and_its_legacy_entry(
    account_id: &str,
    build_key: impl Fn(&str) -> String,
    build_legacy_key: impl Fn(&str) -> String,
) {
    let key = build_key(account_id);
    log!("keychain DELETE {}/{}", KC_SERVICE, key);
    if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &key) {
        let _ = entry.delete_credential();
    }
    if let Some(bare) = legacy_bare_id(account_id) {
        let legacy_key = build_legacy_key(&bare);
        log!("keychain DELETE {}/{} (legacy)", KC_SERVICE, legacy_key);
        if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &legacy_key) {
            let _ = entry.delete_credential();
        }
    }
}

/// Whether each account has a refresh token, memoized for the process lifetime.
///
/// This exists because presence is asked far more often than it can change.
/// `is_authenticated` is polled by the frontend every 2 s for up to two minutes
/// during a sign-in ([`AuthScreen.svelte`]), and it reaches the credential store
/// through `Account::is_ready_local()`. Answering each of those ticks with a
/// fresh `Entry::new` + `get_password()` is one OS credential-store read per
/// tick: invisible on macOS once "Always Allow" is granted, a **dialog per
/// tick** under plain "Allow", and a cost the user waits on for Android, whose
/// store is slower and is the reason `secrets.rs` exists at all.
///
/// **Presence only — never the token.** The value is a `bool`, so no secret is
/// copied anywhere new; `load_refresh_token` stays uncached.
///
/// **Why it lives here rather than in `AppState`.** These three functions are
/// the only things in the process that can change what the map describes, so
/// keeping it beside them makes the map correct by construction. Hanging it off
/// `AppState` instead would put invalidation at the mercy of remembering it at
/// every call site that writes a token (`complete_oauth`, `remove_account`, the
/// re-auth path, *and* the rotation save in `ensure_token` — four, where the
/// obvious three are the ones you'd think of), and `is_ready_local(&self)` has
/// no `AppState` handle to consult anyway.
///
/// **Staleness is accepted, deliberately.** A cached `true` outlives a token
/// removed outside this process (Keychain Access by hand, a server-side
/// revocation). The app then treats the account as usable and the first sync
/// attempt surfaces `handleAuthLoss` — which is already exactly what a
/// present-but-revoked token does today, so this adds no new failure mode. A
/// stale `false` would need a token to appear without passing through
/// `save_refresh_token`, which nothing does.
static RT_PRESENT: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

fn rt_present() -> &'static Mutex<HashMap<String, bool>> {
    RT_PRESENT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record what we now know about presence, so the next `has_refresh_token` can
/// answer without the store. Called on every write and delete.
fn note_rt_presence(account_id: &str, present: bool) {
    rt_present()
        .lock()
        .unwrap()
        .insert(account_id.to_string(), present);
}

pub fn load_refresh_token(account_id: &str) -> Option<String> {
    // Every credential-store access below is real. Item names only — never
    // the secret. Under macOS's one-shot "Allow", each is a password dialog,
    // so the count here IS the dialog count. Only reached at all when the
    // caller is not answering from `RT_PRESENT` (see that static's doc
    // comment) — a qualified id whose token was already saved under the new
    // key never falls through to the legacy read below.
    //
    // Serialized — see `KEYCHAIN_LOCK`'s doc comment: this is one of the two
    // account-scoped operations implicated by the cross-account swap it
    // guards against.
    with_keychain_lock(|| read_secret_with_legacy_fallback(account_id, keychain_key, legacy_key_for))
}

/// Presence check only — does the keychain hold a refresh token for this
/// account? Never refreshes (no network). Backs the offline-safe readiness gate
/// so a cold start can't block on Gmail.
///
/// Reads the credential store at most **once per account per process**; every
/// later call is answered from `RT_PRESENT`. See that static for why.
pub fn has_refresh_token(account_id: &str) -> bool {
    if let Some(&present) = rt_present().lock().unwrap().get(account_id) {
        // No OS access: this is the read-amplification fix doing its job.
        log!("keychain CACHED {}/{} present={}", KC_SERVICE, keychain_key(account_id), present);
        return present;
    }
    // First ask for this account: pay for the real read, then remember it.
    // Deliberately not holding the map lock across the store read — the OS call
    // can be slow (notably on Android), and a duplicate concurrent read is
    // harmless where a blocked caller would not be.
    let present = load_refresh_token(account_id).is_some();
    note_rt_presence(account_id, present);
    present
}

pub fn save_refresh_token(account_id: &str, token: &str) -> Result<(), String> {
    // Serialized — see `KEYCHAIN_LOCK`'s doc comment: this is the write side
    // of the cross-account swap it guards against, and the one this
    // investigation's diagnostic logging caught landing a fraction of a
    // second apart for two different accounts.
    with_keychain_lock(|| {
        let entry = keyring_core::Entry::new(KC_SERVICE, &keychain_key(account_id))
            .map_err(|e| format!("keychain open: {}", e))?;
        log!("keychain WRITE  {}/{}", KC_SERVICE, keychain_key(account_id));
        entry
            .set_password(token)
            .map_err(|e| format!("keychain write: {}", e))?;
        // Only after the write succeeded: on failure presence is unchanged, and a
        // cached negative from the pre-sign-in poll window must not be flipped to
        // `true` by a write that did not land.
        note_rt_presence(account_id, true);
        Ok(())
    })
}

pub fn delete_refresh_token(account_id: &str) {
    with_keychain_lock(|| delete_refresh_token_locked(account_id));
}

fn delete_refresh_token_locked(account_id: &str) {
    // Both entries, always. A refresh token left behind under the
    // pre-migration bare key survives `remove_account` indefinitely — the one
    // thing that command promises not to do — and would be handed straight
    // back by `read_secret_with_legacy_fallback` if the same account is added
    // again. See `delete_secret_and_its_legacy_entry`.
    delete_secret_and_its_legacy_entry(account_id, keychain_key, legacy_key_for);
    // Recorded as absent even when the delete errored: `delete_credential`
    // fails when there was no entry to remove, which means absent either way.
    // Store `false` rather than removing the key, because absence is a fact we
    // just established — dropping it would make the next caller re-ask the OS.
    note_rt_presence(account_id, false);
}

// ─── Legacy single-account migration ─────────────────────────────────────────
// Old install path: keychain at ("jodd", "refresh_token") with no account id.
// On startup, if we find a legacy token AND no accounts.json exists yet,
// preserve the token under a temporary id and let the caller finish migration
// after a getProfile call resolves the actual email.

/// A one-line census of a loaded account list, for the startup log.
///
/// The bare count alone is genuinely misleading and was misread in practice
/// (2026-08-31): "loaded 4 account(s)" on an install with two Active and two
/// Inactive accounts reads as "four accounts are in play", and the number of
/// accounts that DO anything is what a reader is actually after — it is what
/// the worker polls and what `vertical_for` will accept. Draining is named
/// separately rather than folded into either side, because it is the one
/// state that is hidden from the user yet still pushing (gotcha #2).
pub fn account_census(accounts: &[Account]) -> String {
    let mut active = 0usize;
    let mut draining = 0usize;
    let mut inactive = 0usize;
    for a in accounts {
        match a.status {
            AccountStatus::Active => active += 1,
            AccountStatus::Draining => draining += 1,
            AccountStatus::Inactive => inactive += 1,
        }
    }
    let mut out = format!("{} account(s): {active} active", accounts.len());
    if draining > 0 {
        out.push_str(&format!(", {draining} draining"));
    }
    if inactive > 0 {
        out.push_str(&format!(", {inactive} inactive"));
    }
    out
}

pub fn take_legacy_refresh_token() -> Option<String> {
    // Logged like every other keychain read: this one runs at startup on a
    // key most installs no longer have, so it is easy to forget it is a read
    // at all — and on macOS a distinct item is a distinct password prompt.
    log!("keychain READ   {}/{} (legacy single-account)", KC_SERVICE, LEGACY_KEY);
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
///
/// Same pre-migration fallback as `load_refresh_token` — an LLM key saved
/// before account ids gained a `{backend}:` prefix is filed under the bare
/// email, and a qualified id must still find it or the user's configured
/// key silently vanishes on read.
pub fn read_llm_api_key(account_id: &str) -> Option<String> {
    read_secret_with_legacy_fallback(account_id, llm_keychain_key, llm_keychain_key)
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
///
/// Deletes the legacy bare-email entry too. `update_llm_settings` treats an
/// empty string as "clear my key" and calls this — and with only the qualified
/// entry removed, the very next `read_llm_api_key` falls back to the bare one
/// and resurrects the key the user just deleted.
pub fn delete_llm_api_key(account_id: &str) {
    delete_secret_and_its_legacy_entry(account_id, llm_keychain_key, llm_keychain_key);
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
mod keychain_lock_tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// `with_keychain_lock` must give one caller at a time exclusive access —
    /// this is the whole of the mitigation for the cross-account refresh-token
    /// swap this lock exists to close off (see the lock's own doc comment).
    ///
    /// A plain `Mutex` proves mutual exclusion directly: many threads run a
    /// non-atomic read-increment-write on a shared counter through the guard;
    /// with real exclusion every increment lands, so the final count is
    /// exactly `threads * increments_per_thread`. Without exclusion this
    /// specific pattern reliably loses increments to interleaving — it is a
    /// real behavior assertion, not a check on the lock's internals.
    #[test]
    fn excludes_concurrent_callers() {
        static COUNTER: AtomicI32 = AtomicI32::new(0);
        COUNTER.store(0, Ordering::SeqCst);

        const THREADS: usize = 8;
        const INCREMENTS: usize = 500;

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..INCREMENTS {
                        with_keychain_lock(|| {
                            let current = COUNTER.load(Ordering::SeqCst);
                            // A deliberately non-atomic read-modify-write: this is
                            // what loses updates under real concurrent access and
                            // what a correct mutual-exclusion guard prevents.
                            COUNTER.store(current + 1, Ordering::SeqCst);
                        });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(COUNTER.load(Ordering::SeqCst), (THREADS * INCREMENTS) as i32);
    }
}

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
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: AccountStatus::Active,
            pending_removal: false,
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

    /// The one variant whose serde spelling is pinned by hand. Left to
    /// `rename_all = "snake_case"` it would write `i_cloud` into
    /// accounts.json while `backend_prefix` minted `icloud:` ids — two
    /// spellings of one backend, in the two files that have to agree about
    /// which account is which.
    #[test]
    fn icloud_serializes_as_icloud_not_i_cloud() {
        let json = serde_json::to_string(&BackendKind::ICloud).unwrap();
        assert_eq!(json, "\"icloud\"");
        let back: BackendKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, BackendKind::ICloud);
        assert_eq!(backend_prefix(BackendKind::ICloud), "icloud");
    }

    /// The whole point of `icloud_session_established` is that this path
    /// reads a bool off the record and touches nothing else — no credential
    /// store, no webview (gotcha #15).
    #[test]
    fn icloud_readiness_reads_the_marker_and_nothing_else() {
        let mut account = make_account(BackendKind::ICloud, None);
        assert!(!account.is_ready_local(), "no sign-in yet → not ready");
        account.icloud_session_established = true;
        assert!(account.is_ready_local(), "marker set → ready, with no token anywhere");
    }

    /// An accounts.json written before this field existed must still parse,
    /// and must not claim a session it has never had.
    #[test]
    fn account_json_without_the_icloud_marker_defaults_to_no_session() {
        let json = r#"{
            "id": "gmail:old@example.com",
            "email": "old@example.com",
            "added_at": "2025-01-01T00:00:00Z"
        }"#;
        let acc: Account = serde_json::from_str(json).expect("should parse");
        assert!(!acc.icloud_session_established);
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

    /// End-to-end proof that `load_refresh_token` reaches the pre-migration
    /// entry, not just that `legacy_key_for` computes the right string. A
    /// credential "saved before this change" is simulated by writing directly
    /// under the bare-email key (never through `save_refresh_token`, which
    /// always writes the qualified key now) — then a QUALIFIED id must still
    /// find it.
    #[test]
    fn load_refresh_token_falls_back_to_the_legacy_entry_when_the_qualified_key_misses() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-legacy-fallback@example.invalid";
        let qualified = format!("gmail:{}", bare);

        // Known-clean slate on both keys.
        delete_refresh_token(&qualified);
        let legacy_entry =
            keyring_core::Entry::new(KC_SERVICE, &keychain_key(bare)).expect("entry should open");
        let _ = legacy_entry.delete_credential();

        legacy_entry
            .set_password("pre-migration-token")
            .expect("write should succeed");

        assert_eq!(
            load_refresh_token(&qualified),
            Some("pre-migration-token".to_string()),
            "a qualified id must find the token filed under its pre-migration bare id"
        );

        // The next successful save writes only the new key — nothing deletes
        // the legacy entry, and nothing here should be read again from it.
        save_refresh_token(&qualified, "post-migration-token").expect("write should succeed");
        assert_eq!(
            load_refresh_token(&qualified),
            Some("post-migration-token".to_string()),
            "once the qualified key holds a token, it must win over the legacy one"
        );
        assert_eq!(
            legacy_entry.get_password().expect("legacy entry should still be readable"),
            "pre-migration-token",
            "the legacy entry must be left untouched, not deleted, by a save under the new key"
        );

        // Cleanup.
        delete_refresh_token(&qualified);
        let _ = legacy_entry.delete_credential();
    }

    #[test]
    fn a_bare_id_with_no_token_anywhere_finds_nothing_and_does_not_panic() {
        // A bare id is its own legacy form (gotcha in the brief): there is no
        // second key to try, so a miss on the only key must return None, not
        // fall back to itself in a loop.
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-bare-no-fallback@example.invalid";
        delete_refresh_token(bare);
        assert_eq!(load_refresh_token(bare), None);
    }

    /// Same fallback treatment applied to the second credential keyed by
    /// account id — the LLM API key. Missing this would silently orphan every
    /// user's configured LLM key the same way a missed refresh token would
    /// silently log them out.
    #[test]
    fn a_fallback_read_copies_the_secret_onto_the_qualified_key() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-fallback-selfheal@example.invalid";
        let qualified = format!("gmail:{}", bare);

        // Known-clean slate on both keys.
        delete_refresh_token(&qualified);
        let legacy_entry =
            keyring_core::Entry::new(KC_SERVICE, &keychain_key(bare)).expect("entry should open");
        let _ = legacy_entry.delete_credential();
        legacy_entry
            .set_password("pre-migration-token")
            .expect("write should succeed");

        // First read has to go through the fallback — the qualified key is empty.
        assert_eq!(
            load_refresh_token(&qualified),
            Some("pre-migration-token".to_string()),
            "a qualified id must find the token filed under its pre-migration bare id"
        );

        // ...and must have copied it onto the qualified key on the way. Deleting
        // the legacy entry outright is the assertion: if the copy did not happen,
        // the next read has nowhere left to look.
        //
        // This is what a Gmail account needs. `save_refresh_token` runs only on
        // token rotation, and Google does not return a fresh refresh token on
        // every exchange — so "the next successful save completes the move" can
        // be a save that never comes, leaving the account reading through two
        // keychain entries forever.
        legacy_entry
            .delete_credential()
            .expect("legacy entry should be removable");
        assert_eq!(
            load_refresh_token(&qualified),
            Some("pre-migration-token".to_string()),
            "the fallback read must copy the secret onto the qualified key"
        );

        delete_refresh_token(&qualified);
    }
    #[test]
    fn read_llm_api_key_falls_back_to_the_legacy_entry_when_the_qualified_key_misses() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-llm-legacy-fallback@example.invalid";
        let qualified = format!("gmail:{}", bare);

        delete_llm_api_key(&qualified);
        let legacy_entry = keyring_core::Entry::new(KC_SERVICE, &llm_keychain_key(bare))
            .expect("entry should open");
        let _ = legacy_entry.delete_credential();

        legacy_entry
            .set_password("pre-migration-llm-key")
            .expect("write should succeed");

        assert_eq!(
            read_llm_api_key(&qualified),
            Some("pre-migration-llm-key".to_string()),
            "a qualified id must find the LLM key filed under its pre-migration bare id"
        );

        write_llm_api_key(&qualified, "post-migration-llm-key").expect("write should succeed");
        assert_eq!(
            read_llm_api_key(&qualified),
            Some("post-migration-llm-key".to_string()),
            "once the qualified key holds a value, it must win over the legacy one"
        );
        assert_eq!(
            legacy_entry.get_password().expect("legacy entry should still be readable"),
            "pre-migration-llm-key",
            "the legacy entry must be left untouched, not deleted, by a save under the new key"
        );

        delete_llm_api_key(&qualified);
        let _ = legacy_entry.delete_credential();
    }

    /// I4: `remove_account` calls `delete_refresh_token`, and its whole point
    /// is that nothing is left behind for an account the user believes they
    /// signed out of. Deleting only the qualified entry leaves a **live
    /// refresh token** under the pre-migration bare key — and the read side
    /// falls back to exactly that key, so the account is not even signed out.
    #[test]
    fn delete_refresh_token_also_removes_the_pre_migration_entry() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-rt-legacy-delete@example.invalid";
        let qualified = format!("gmail:{}", bare);

        // A profile that upgraded: the only token on disk is the legacy one.
        let legacy_entry =
            keyring_core::Entry::new(KC_SERVICE, &legacy_key_for(bare)).expect("entry should open");
        legacy_entry
            .set_password("pre-migration-token")
            .expect("write should succeed");
        assert_eq!(
            load_refresh_token(&qualified),
            Some("pre-migration-token".to_string()),
            "fixture sanity: the qualified id must reach the legacy token"
        );

        delete_refresh_token(&qualified);

        assert_eq!(
            load_refresh_token(&qualified),
            None,
            "the legacy refresh token survived account removal — it is still usable"
        );
        assert!(
            legacy_entry.get_password().is_err(),
            "the pre-migration keychain entry must be gone, not merely bypassed"
        );
    }

    /// I3: `update_llm_settings` treats `Some("")` as "clear my key" and calls
    /// `delete_llm_api_key`. With only the qualified entry deleted, the very
    /// next read falls back to the legacy one and hands back the key the user
    /// just cleared.
    #[test]
    fn clearing_an_llm_api_key_is_not_undone_by_the_legacy_fallback() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-llm-legacy-delete@example.invalid";
        let qualified = format!("gmail:{}", bare);

        let legacy_entry = keyring_core::Entry::new(KC_SERVICE, &llm_keychain_key(bare))
            .expect("entry should open");
        legacy_entry
            .set_password("pre-migration-llm-key")
            .expect("write should succeed");
        // The user then sets a key in the app: qualified entry written too.
        write_llm_api_key(&qualified, "post-migration-llm-key").expect("write should succeed");

        delete_llm_api_key(&qualified);

        assert_eq!(
            read_llm_api_key(&qualified),
            None,
            "an explicit clear must not resurrect the pre-migration key"
        );
    }
}

#[cfg(test)]
mod presence_cache_tests {
    use super::*;

    // These tests prove `has_refresh_token` answers from the in-process
    // presence cache rather than the OS credential store, WITHOUT counting
    // reads or mocking anything: they mutate the store behind the module's
    // back (a raw `keyring_core::Entry`, never `delete_refresh_token`) and
    // then assert the cached answer is the one that survives. If the cache
    // were absent, `has_refresh_token` would see the emptied store and
    // disagree.
    //
    // Every test uses a UNIQUE throwaway `.invalid` id. The cache is a
    // process-global that cannot be reset between tests (the same constraint
    // paths.rs:19 documents for its OnceLocks), so shared ids would let one
    // test's writes decide another's result depending on ordering.

    /// Delete straight from the platform store, bypassing
    /// `delete_refresh_token` so the presence cache is NOT updated. Models an
    /// external removal: Keychain Access by hand, or a server-side revocation.
    fn delete_behind_the_cache(account_id: &str) {
        let entry = keyring_core::Entry::new(KC_SERVICE, &keychain_key(account_id))
            .expect("entry should open");
        let _ = entry.delete_credential();
    }

    #[test]
    fn presence_is_cached_so_an_external_deletion_does_not_change_the_answer() {
        crate::secrets::init().expect("credential store must initialize");
        let acct = "jodd-test-presence-cached@example.invalid";
        delete_behind_the_cache(acct); // start from a known-absent store

        save_refresh_token(acct, "throwaway").expect("write should succeed");
        assert!(has_refresh_token(acct), "a just-saved token must read present");

        delete_behind_the_cache(acct);

        // The store is genuinely empty now...
        assert_eq!(
            load_refresh_token(acct),
            None,
            "the raw store must really be empty — otherwise this test proves nothing"
        );
        // ...yet presence still answers true, which is only possible from cache.
        assert!(
            has_refresh_token(acct),
            "presence must come from the cache, not a fresh keychain read"
        );

        // Accepted staleness, not an oversight: a stale `true` sends the app
        // down the same path a present-but-revoked token already takes — the
        // first sync attempt surfaces handleAuthLoss (see is_authenticated's
        // doc comment in lib.rs).
    }

    #[test]
    fn deleting_through_the_wrapper_updates_the_cache() {
        crate::secrets::init().expect("credential store must initialize");
        let acct = "jodd-test-presence-delete@example.invalid";
        delete_behind_the_cache(acct);

        save_refresh_token(acct, "throwaway").expect("write should succeed");
        assert!(has_refresh_token(acct));

        delete_refresh_token(acct);

        // No stale `true`: the wrapper owns the cache, so an in-process delete
        // is reflected immediately.
        assert!(
            !has_refresh_token(acct),
            "delete_refresh_token must flip the cached answer to false"
        );
    }

    #[test]
    fn an_absent_token_reads_absent_and_stays_that_way() {
        crate::secrets::init().expect("credential store must initialize");
        let acct = "jodd-test-presence-absent@example.invalid";
        delete_behind_the_cache(acct);

        // Caching a negative is what makes the sign-in poll cheap: during the
        // window before the first token is saved there is nothing to find, and
        // re-asking the store every 2s is the amplification being fixed.
        assert!(!has_refresh_token(acct));
        assert!(!has_refresh_token(acct), "repeated asks stay false");

        // A save through the wrapper must beat the cached negative.
        save_refresh_token(acct, "throwaway").expect("write should succeed");
        assert!(
            has_refresh_token(acct),
            "save_refresh_token must overwrite a cached negative — this is the \
             sign-in transition"
        );
        delete_refresh_token(acct);
    }

    /// Gotcha #15, extended to a QUALIFIED id specifically — the case this
    /// task's legacy fallback touches. Proves the fallback branch added to
    /// `load_refresh_token` is unreachable once `RT_PRESENT` has cached an
    /// answer, by emptying BOTH the qualified-key entry and the legacy-key
    /// entry behind the cache's back and showing the cached `true` survives.
    /// If a cache hit fell through to `read_secret_with_legacy_fallback` at
    /// all, this would observe `false` — both real entries are gone.
    #[test]
    fn presence_is_cached_for_a_qualified_id_even_with_no_legacy_entry_to_fall_back_to() {
        crate::secrets::init().expect("credential store must initialize");
        let bare = "jodd-test-presence-qualified@example.invalid";
        let qualified = format!("gmail:{}", bare);
        delete_behind_the_cache(&qualified);
        let legacy_entry = keyring_core::Entry::new(KC_SERVICE, &keychain_key(bare))
            .expect("entry should open");
        let _ = legacy_entry.delete_credential();

        save_refresh_token(&qualified, "throwaway").expect("write should succeed");
        assert!(has_refresh_token(&qualified), "a just-saved token must read present");

        // Empty every real entry this id could ever resolve to — new key AND
        // legacy key — while the cache still holds `true` from the line above.
        delete_behind_the_cache(&qualified);
        let _ = legacy_entry.delete_credential();
        assert_eq!(
            load_refresh_token(&qualified),
            None,
            "both real entries must be genuinely empty — otherwise this test proves nothing"
        );

        // The cache alone must still answer `true`: no path from a cache hit
        // reaches load_refresh_token, so it cannot see the emptied legacy
        // entry either.
        assert!(
            has_refresh_token(&qualified),
            "a cache hit must never fall through to the legacy-fallback read"
        );
    }
}

#[cfg(test)]
mod tests {

    fn acct_with(status: AccountStatus) -> Account {
        Account {
            id: format!("gmail:{status:?}@example.com").to_lowercase(),
            email: "a@example.com".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: BackendKind::Gmail,
            root_dir: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status,
            pending_removal: false,
        }
    }

    /// The exact shape that was misread on 2026-08-31: a user with two Active
    /// and two Inactive accounts saw "loaded 4 account(s)" and reasonably took
    /// it to mean four accounts were in play. The count a reader wants is how
    /// many DO anything.
    #[test]
    fn the_census_says_how_many_accounts_are_actually_in_play() {
        let list = vec![
            acct_with(AccountStatus::Active),
            acct_with(AccountStatus::Active),
            acct_with(AccountStatus::Inactive),
            acct_with(AccountStatus::Inactive),
        ];
        assert_eq!(account_census(&list), "4 account(s): 2 active, 2 inactive");
    }

    /// Draining is named on its own, never folded into either side: it is the
    /// one state that is hidden from the user and still pushing (gotcha #2),
    /// so a reader chasing "why is this account still syncing" needs to see it.
    #[test]
    fn the_census_names_draining_separately() {
        let list = vec![
            acct_with(AccountStatus::Active),
            acct_with(AccountStatus::Draining),
        ];
        assert_eq!(account_census(&list), "2 account(s): 1 active, 1 draining");
    }

    /// The common case stays short — no empty clauses for states nobody is in.
    #[test]
    fn the_census_omits_states_with_no_accounts() {
        let list = vec![acct_with(AccountStatus::Active)];
        assert_eq!(account_census(&list), "1 account(s): 1 active");
        assert_eq!(account_census(&[]), "0 account(s): 0 active");
    }
    use super::*;

    #[test]
    fn account_id_for_prefixes_by_backend() {
        assert_eq!(account_id_for(BackendKind::Gmail, "a@b.com"), "gmail:a@b.com");
        assert_eq!(account_id_for(BackendKind::Microsoft, "a@b.com"), "microsoft:a@b.com");
        // LocalFs is "localfs", NOT serde's "local_fs" — existing ids are localfs:{uuid}.
        assert_eq!(account_id_for(BackendKind::LocalFs, "vault"), "localfs:vault");
    }

    #[test]
    fn is_qualified_recognises_every_backend_and_rejects_a_bare_email() {
        assert!(is_qualified("gmail:a@b.com"));
        assert!(is_qualified("microsoft:a@b.com"));
        assert!(is_qualified("localfs:1E2D3C4B-0000-0000-0000-000000000000"));
        assert!(!is_qualified("a@b.com"));
        assert!(!is_qualified(""));
    }

    #[test]
    fn a_localfs_id_minted_today_is_already_qualified() {
        // lib.rs:1516 already mints localfs:{uuid}. Migration #19 must skip it
        // rather than produce localfs:localfs:{uuid}.
        let existing = format!("localfs:{}", "1E2D3C4B-0000-0000-0000-000000000000");
        assert!(is_qualified(&existing));
    }

    #[test]
    fn an_email_containing_a_colon_is_not_mistaken_for_a_qualified_id() {
        // Only a known backend prefix counts, so a stray colon cannot fake one.
        assert!(!is_qualified("weird:address@b.com"));
    }

    #[test]
    fn a_qualified_id_falls_back_to_the_legacy_bare_email_entry() {
        // A token saved before this change lives under rt::a@b.com. Reading with
        // the new id must still find it, or the user is silently logged out.
        let legacy = legacy_key_for("a@b.com");
        assert_eq!(legacy, "rt::a@b.com");
        assert_eq!(keychain_key("gmail:a@b.com"), "rt::gmail:a@b.com");
    }

    #[test]
    fn fallback_only_applies_to_a_qualified_id() {
        // A bare id has no legacy form to fall back to — that IS the legacy form.
        assert!(legacy_bare_id("gmail:a@b.com") == Some("a@b.com".to_string()));
        assert!(legacy_bare_id("a@b.com").is_none());
        assert!(legacy_bare_id("localfs:1E2D-3C4B").is_some());
    }

    #[test]
    fn write_atomically_replaces_the_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("accounts.json");
        fs::write(&p, "old").unwrap();
        write_atomically(&p, "new").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "new");
        assert!(
            !p.with_extension("json.tmp").exists(),
            "the temp file must not survive a successful write"
        );
    }

    /// The property a plain `fs::write` does not have: a write that fails
    /// leaves the PREVIOUS file intact rather than a truncated one. That
    /// matters because `load_accounts` swallows a parse error and returns an
    /// empty `Vec`, so a truncated `accounts.json` reads as "no accounts" —
    /// after which every cached note is unreachable and migration #19 declines
    /// to run at all (`db::migrate_account_ids_with` case 3, which is what
    /// keeps that state from ALSO destroying or locking out the cache).
    ///
    /// The failure is induced by making the target a directory, which `rename`
    /// refuses; the temp file is written first either way, so this exercises
    /// exactly the window a truncating write would have destroyed.
    #[test]
    fn a_failed_write_leaves_the_previous_contents_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("accounts.json");
        fs::create_dir(&p).unwrap();
        fs::write(p.join("sentinel"), "still here").unwrap();

        let err = write_atomically(&p, "new").unwrap_err();
        assert!(err.contains("rename"), "unexpected error: {err}");
        assert_eq!(
            fs::read_to_string(p.join("sentinel")).unwrap(),
            "still here",
            "a failed write must not disturb what was already there"
        );
        assert!(
            !p.with_extension("json.tmp").exists(),
            "a failed write must clean up its temp file"
        );
    }

    /// `is_qualified` and `legacy_bare_id` ask the same question from opposite
    /// directions and used to carry a private copy of the backend list each.
    /// Two lists that must agree is the defect; this is what would fail if
    /// they drifted apart again, for every backend at once.
    #[test]
    fn is_qualified_and_legacy_bare_id_read_the_same_backend_list() {
        for kind in ALL_BACKENDS {
            let id = account_id_for(kind, "someone@example.com");
            assert!(
                is_qualified(&id),
                "{id} was minted by account_id_for and must read as qualified"
            );
            assert_eq!(
                legacy_bare_id(&id),
                Some("someone@example.com".to_string()),
                "{id} must strip back to the pre-migration bare id"
            );
        }
        // An unknown prefix is not a backend to either of them.
        assert!(!is_qualified("imap:someone@example.com"));
        assert_eq!(legacy_bare_id("imap:someone@example.com"), None);
    }

    /// `ALL_BACKENDS` is a hand-written array, so nothing but this exhaustive
    /// match stops a fifth `BackendKind` from being added while the array
    /// still lists four — which would leave its ids unrecognised by
    /// `is_qualified` (migration #19 double-prefixes them) and by
    /// `legacy_bare_id` (its credentials never find the pre-migration entry).
    #[test]
    fn all_backends_lists_every_variant() {
        fn exhaustive(kind: BackendKind) -> &'static str {
            // Adding a variant breaks THIS match first. Fix it by extending
            // ALL_BACKENDS and the assertion below, not by adding a wildcard.
            match kind {
                BackendKind::Gmail => "gmail",
                BackendKind::LocalFs => "localfs",
                BackendKind::Microsoft => "microsoft",
                BackendKind::ICloud => "icloud",
            }
        }
        assert_eq!(ALL_BACKENDS.len(), 4, "extend ALL_BACKENDS when BackendKind grows");
        for kind in ALL_BACKENDS {
            assert_eq!(backend_prefix(kind), exhaustive(kind));
        }
    }

    #[test]
    fn ensure_icloud_replica_id_mints_once_and_is_stable() {
        let mut a = Account {
            id: "icloud:test@me.com".into(),
            email: "test@me.com".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: BackendKind::ICloud,
            root_dir: None,
            icloud_session_established: true,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: AccountStatus::Active,
            pending_removal: false,
        };
        assert!(a.icloud_replica_id.is_none());

        let first = a.ensure_icloud_replica_id();
        let stored = a.icloud_replica_id.clone().expect("field must be set after minting");
        assert_eq!(uuid::Uuid::parse_str(&stored).unwrap().as_bytes(), &first);

        // Calling again must NOT mint a second id.
        let second = a.ensure_icloud_replica_id();
        assert_eq!(first, second);
        assert_eq!(a.icloud_replica_id.as_deref(), Some(stored.as_str()));
    }

    #[test]
    fn a_corrupted_replica_id_field_is_treated_as_absent_not_a_panic() {
        let mut a = Account {
            id: "icloud:test@me.com".into(),
            email: "test@me.com".into(),
            added_at: "2026-01-01T00:00:00Z".into(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: BackendKind::ICloud,
            root_dir: None,
            icloud_session_established: true,
            blocked_reason: None,
            sync_cursor: None,
            status: AccountStatus::Active,
            pending_removal: false,
            icloud_replica_id: Some("not-a-uuid".into()),
        };
        let minted = a.ensure_icloud_replica_id();
        assert_ne!(a.icloud_replica_id.as_deref(), Some("not-a-uuid"));
        assert_eq!(uuid::Uuid::parse_str(&a.icloud_replica_id.clone().unwrap()).unwrap().as_bytes(), &minted);
    }

    #[test]
    fn accounts_json_written_before_this_field_existed_still_parses() {
        let json = r#"{
            "id": "icloud:old@me.com",
            "email": "old@me.com",
            "added_at": "2026-01-01T00:00:00Z",
            "backend_kind": "icloud",
            "icloud_session_established": true,
            "status": "active"
        }"#;
        let a: Account = serde_json::from_str(json).unwrap();
        assert_eq!(a.icloud_replica_id, None);
    }
}
