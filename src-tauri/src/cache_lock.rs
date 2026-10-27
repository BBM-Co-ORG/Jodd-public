//! What Jodd does when its local cache cannot be opened: it **locks**.
//!
//! Before 2026-10-26 a failed `Db::open` logged `FATAL … using temp dir` and
//! carried on with an empty throwaway database — beside the REAL
//! `accounts.json`, the real keychain and the real settings files. That
//! night a rebuilt binary's keychain prompt sat unanswered for ~30 minutes,
//! the read failed ("Unable to obtain authorization for this operation"),
//! and the UI showed the user's main Gmail account with 0 notes. Remove was
//! clicked on it, and `remove_account` deleted the account's refresh token
//! and its `accounts.json` entry for real. Only the throwaway cache was
//! "wiped".
//!
//! The rule that replaces the fallback: **a process whose cache did not open
//! must not touch persistent state.** Concretely, once [`engage`] has run:
//!
//! - `.setup()` does not read `accounts.json`, sweep ssh key directories,
//!   migrate legacy keychain entries or start the sync worker;
//! - every frontend command is refused at the invoke boundary ([`gate`])
//!   except [`ALLOWED_WHILE_LOCKED`];
//! - `accounts::save_accounts`, `save_refresh_token` and
//!   `delete_refresh_token` refuse at the leaf ([`refuse`]), so a background
//!   path nobody thought of still cannot write;
//! - the window shows "Couldn't unlock your notes" instead of the app, and
//!   its Try again relaunches the process, which re-reads the key with the
//!   user present to answer the prompt.
//!
//! The lock lasts for the life of the process. Lifting it in place would
//! mean swapping `AppState.db` under every command holding it; a relaunch
//! re-runs `.setup()` from the top, which is the path every successful
//! launch already takes.

use serde::Serialize;
use crate::log;
use std::sync::OnceLock;

use crate::db_crypto::DbOpenError;

/// Why the cache is locked. The frontend words the screen from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockKind {
    /// The keychain read for the DB key failed: a prompt that timed out or
    /// was denied, a locked login keychain, an unavailable secret service.
    /// Usually fixed by trying again and answering the prompt.
    Keychain,
    /// The key was obtained (or never needed) but the database still did not
    /// open: the plaintext→encrypted migration or SQLite itself failed.
    /// Trying again may not help, but nothing is lost by it.
    OpenFailed,
}

/// The state shown to the user, and the reason every refusal names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CacheLock {
    pub kind: LockKind,
    /// The underlying error, for the screen's "Details" and the log.
    pub detail: String,
}

impl CacheLock {
    /// The lock a `Db::open` failure calls for, or `None` when the error has
    /// its own handling and must never reach the lock.
    ///
    /// `KeyMismatchOrCorrupt` — the stored key does not decrypt the file —
    /// is quarantine-and-recover (`db_crypto::recover_from_key_mismatch`).
    /// `AccountIdMigration` fails the launch with its reason. Neither is a
    /// "try again" situation, and keeping them out of this function means
    /// a future caller cannot route them here by accident.
    pub fn for_open_error(e: &DbOpenError) -> Option<Self> {
        let kind = match e {
            DbOpenError::Keychain(_) => LockKind::Keychain,
            DbOpenError::Migration(_) | DbOpenError::Sqlite(_) => LockKind::OpenFailed,
            DbOpenError::KeyMismatchOrCorrupt | DbOpenError::AccountIdMigration(_) => {
                return None
            }
        };
        Some(CacheLock { kind, detail: e.to_string() })
    }

    /// The refusal every guarded operation returns while locked.
    pub fn refusal(&self, what: &str) -> String {
        format!(
            "refused to {what}: Jodd could not open its local notes cache ({}), so it is \
             not touching accounts, the keychain or settings until it can. Use Try again \
             on the unlock screen.",
            self.detail
        )
    }

    /// JavaScript the main window runs before the page loads. The frontend's
    /// entry point reads it synchronously and mounts the unlock screen instead
    /// of the app — no IPC round trip, so nothing can mount (and invoke)
    /// before the decision is made, and no Android startup IPC (gotcha #32)
    /// is added on the path where the lock never engages anyway.
    pub fn init_script(&self) -> String {
        // serde_json output is a valid JS expression; `detail` is an error
        // string and must never be spliced into the script by hand.
        let json = serde_json::to_string(self).expect("CacheLock serializes");
        format!("window.__JODD_CACHE_LOCK__ = {json};")
    }
}

/// Frontend commands that still run while locked. Everything else is refused
/// at the invoke boundary. Keep this list minimal: each entry is a command
/// that runs beside a throwaway database.
pub const ALLOWED_WHILE_LOCKED: &[&str] = &["retry_cache_unlock"];

static ENGAGED: OnceLock<CacheLock> = OnceLock::new();

#[cfg(test)]
thread_local! {
    /// Per-test override, so a test can engage the lock without leaking it
    /// into every other test running in this process.
    static TEST_LOCK: std::cell::RefCell<Option<CacheLock>> = const { std::cell::RefCell::new(None) };
}

/// Lock this process. Called once, from `.setup()`, before anything reads
/// persistent state. A second call keeps the first reason.
pub fn engage(lock: CacheLock) {
    log!(
        "CACHE LOCKED ({:?}): {} — accounts, keychain writes, settings and sync are off \
         until a relaunch opens the cache",
        lock.kind,
        lock.detail
    );
    let _ = ENGAGED.set(lock);
}

/// The active lock, if any.
pub fn engaged() -> Option<CacheLock> {
    #[cfg(test)]
    if let Some(l) = TEST_LOCK.with(|c| c.borrow().clone()) {
        return Some(l);
    }
    ENGAGED.get().cloned()
}

/// `Err` with the reason when locked. The leaf guard for writes to
/// `accounts.json` and the keychain: call it FIRST, before resolving a path
/// or opening a keychain entry.
pub fn refuse(what: &str) -> Result<(), String> {
    match engaged() {
        Some(lock) => {
            let why = lock.refusal(what);
            log!("{why}");
            Err(why)
        }
        None => Ok(()),
    }
}

/// The refusal for frontend command `cmd`, or `None` if it may run.
pub fn refusal_for_command(lock: Option<&CacheLock>, cmd: &str) -> Option<String> {
    let lock = lock?;
    if ALLOWED_WHILE_LOCKED.contains(&cmd) {
        return None;
    }
    Some(lock.refusal(&format!("run `{cmd}`")))
}

/// Wrap the app's `generate_handler!` so every command passes the lock first.
/// One chokepoint instead of a check in each of ~300 commands, and a command
/// added later is covered without anyone remembering to add it.
pub fn gate<R, F>(inner: F) -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
where
    R: tauri::Runtime,
    F: Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke| {
        let lock = engaged();
        if let Some(why) = refusal_for_command(lock.as_ref(), invoke.message.command()) {
            log!("{why}");
            invoke.resolver.reject(why);
            return true;
        }
        inner(invoke)
    }
}

/// Try again: relaunch, so `.setup()` re-reads the DB key while the user is
/// at the keyboard to answer the prompt. `request_restart` (not `restart`)
/// so the exit goes through `RunEvent::Exit`, where the single-instance
/// plugin removes its socket — otherwise the new process can hand its launch
/// to this dying one and exit, leaving nothing running.
#[tauri::command]
pub fn retry_cache_unlock(app: tauri::AppHandle) {
    log!("cache lock: Try again — relaunching to re-read the DB key");
    app.request_restart();
}

#[cfg(test)]
pub(crate) fn with_test_lock<T>(lock: CacheLock, f: impl FnOnce() -> T) -> T {
    TEST_LOCK.with(|c| *c.borrow_mut() = Some(lock));
    let out = f();
    TEST_LOCK.with(|c| *c.borrow_mut() = None);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keychain_lock() -> CacheLock {
        CacheLock::for_open_error(&DbOpenError::Keychain(
            "keychain read: Unable to obtain authorization for this operation".into(),
        ))
        .unwrap()
    }

    #[test]
    fn a_failed_keychain_read_locks_as_retryable_keychain() {
        let lock = keychain_lock();
        assert_eq!(lock.kind, LockKind::Keychain);
        assert!(lock.detail.contains("Unable to obtain authorization"));
    }

    #[test]
    fn migration_and_sqlite_failures_lock_as_open_failed() {
        let m = CacheLock::for_open_error(&DbOpenError::Migration("disk full".into())).unwrap();
        assert_eq!(m.kind, LockKind::OpenFailed);
        let s = CacheLock::for_open_error(&DbOpenError::Sqlite(
            rusqlite::Error::InvalidQuery,
        ))
        .unwrap();
        assert_eq!(s.kind, LockKind::OpenFailed);
    }

    #[test]
    fn a_key_mismatch_and_a_refused_id_migration_never_lock() {
        // Both have their own handling in `.setup()`: quarantine-and-recover,
        // and failing the launch with the reason.
        assert_eq!(CacheLock::for_open_error(&DbOpenError::KeyMismatchOrCorrupt), None);
        assert_eq!(
            CacheLock::for_open_error(&DbOpenError::AccountIdMigration("x".into())),
            None
        );
    }

    #[test]
    fn unlocked_every_command_runs() {
        assert_eq!(refusal_for_command(None, "remove_account"), None);
        assert_eq!(refusal_for_command(None, "list_accounts"), None);
    }

    #[test]
    fn locked_the_commands_of_the_incident_are_refused() {
        let lock = keychain_lock();
        // The incident's command, and the ones around it that read or write
        // accounts, the keychain or settings.
        for cmd in [
            "remove_account",
            "list_accounts",
            "is_authenticated",
            "save_oauth_config",
            "add_local_account",
            "save_note",
            "set_app_llm_config",
            "flush_sync",
        ] {
            let why = refusal_for_command(Some(&lock), cmd)
                .unwrap_or_else(|| panic!("`{cmd}` must be refused while locked"));
            assert!(why.contains(cmd), "refusal names the command: {why}");
            assert!(why.contains("Unable to obtain authorization"), "and the cause: {why}");
        }
    }

    #[test]
    fn locked_only_try_again_runs() {
        let lock = keychain_lock();
        assert_eq!(refusal_for_command(Some(&lock), "retry_cache_unlock"), None);
        assert_eq!(ALLOWED_WHILE_LOCKED, &["retry_cache_unlock"]);
    }

    #[test]
    fn refuse_is_ok_unlocked_and_err_locked() {
        assert_eq!(refuse("save accounts.json"), Ok(()));
        let err = with_test_lock(keychain_lock(), || refuse("save accounts.json")).unwrap_err();
        assert!(err.contains("save accounts.json"));
        // The override is per-test: gone once the closure returns.
        assert_eq!(refuse("save accounts.json"), Ok(()));
    }

    #[test]
    fn the_init_script_is_json_not_spliced_text() {
        let lock = CacheLock::for_open_error(&DbOpenError::Keychain(
            "a \"quoted\" </script> detail".into(),
        ))
        .unwrap();
        let js = lock.init_script();
        let json = js
            .strip_prefix("window.__JODD_CACHE_LOCK__ = ")
            .and_then(|s| s.strip_suffix(';'))
            .expect("assignment shape");
        let v: serde_json::Value = serde_json::from_str(json).expect("valid JSON");
        assert_eq!(v["kind"], "keychain");
        assert!(v["detail"].as_str().unwrap().contains("\"quoted\""));
    }

    /// The leaf guards in `accounts.rs` must run before the write they guard.
    /// Source-scanned rather than called: calling `save_accounts` or
    /// `delete_refresh_token` from a test reaches the user's REAL config dir
    /// and keychain if the guard ever regresses — the incident itself.
    #[test]
    fn accounts_writes_check_the_lock_first() {
        let src = include_str!("accounts.rs");
        for sig in [
            "pub fn save_accounts(",
            "pub fn save_refresh_token(",
            "pub fn delete_refresh_token(",
        ] {
            let body = crate::test_support::extract_fn_body(src, sig);
            let guard = body
                .find("cache_lock::refuse(")
                .unwrap_or_else(|| panic!("{sig} must call cache_lock::refuse"));
            for effect in ["accounts_path()", "keyring_core::Entry::new", "with_keychain_lock("] {
                if let Some(e) = body.find(effect) {
                    assert!(
                        guard < e,
                        "{sig} reaches `{effect}` before checking the cache lock"
                    );
                }
            }
        }
    }
}
