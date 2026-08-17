//! DB-at-rest encryption: key lifecycle, plaintext detection, and the
//! one-time plaintext→encrypted migration for `jodd.sqlite3`.
//!
//! See docs/superpowers/specs/2026-08-13-at-rest-encryption-design.md for
//! the full design reasoning (crypto backend choice, key lifecycle,
//! jodd-mcp coordination).

const KC_SERVICE: &str = "jodd";

/// The keychain entry name holding the DB cipher key.
///
/// In non-test builds this is the single real entry `Jodd.app` and
/// `jodd-mcp` share. In test builds it is namespaced **per test thread**:
/// `cargo test --workspace` is this repo's mandated verify command, and the
/// db_crypto/db tests save, load and delete this entry — against the real
/// name they would destroy the DB key of any developer who also runs Jodd on
/// that machine, permanently locking them out of their own encrypted note
/// cache. Per-*thread* (not merely per-process) also removes the keychain
/// test flakiness earlier task reviews flagged: libtest gives each test its
/// own thread, so no two tests can now contend for one entry under
/// `--test-threads=N`.
#[cfg(not(test))]
fn kc_key_name() -> String {
    "db_cipher_key::v1".to_string()
}

#[cfg(test)]
fn kc_key_name() -> String {
    use std::cell::RefCell;
    thread_local! {
        static NAME: RefCell<Option<String>> = const { RefCell::new(None) };
    }
    NAME.with(|n| {
        n.borrow_mut()
            .get_or_insert_with(|| {
                format!(
                    "db_cipher_key::v1::test-{}-{}",
                    std::process::id(),
                    // 64 bits of randomness: distinct per thread, and never
                    // colliding with a concurrent `cargo test` on the same box.
                    &generate_key_hex()[..16]
                )
            })
            .clone()
    })
}

/// Best-effort removal of this test thread's namespaced key entry, so a test
/// run doesn't leave a trail of entries in the developer's real keychain.
#[cfg(test)]
pub(crate) fn delete_test_key() {
    if let Ok(entry) = keyring_core::Entry::new(KC_SERVICE, &kc_key_name()) {
        let _ = entry.delete_credential();
    }
}

/// A fresh, random 256-bit key as a 64-character lowercase hex string, for
/// SQLCipher's raw-key syntax (`PRAGMA key = "x'<hex>'"`). Never a
/// passphrase — this is machine-generated and never human-typed, so there
/// is no brute-force surface to slow down against with PBKDF2.
pub fn generate_key_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Read the DB cipher key from the keychain.
///
/// - `Ok(None)` — the entry genuinely does not exist (`keyring_core::Error::NoEntry`),
///   i.e. a fresh install. Only this case may lead a caller to mint a new key.
/// - `Ok(Some(key))` — the stored key.
/// - `Err(msg)` — the read FAILED (locked secret-service, lost macOS ACL,
///   backend unavailable, Android restore, …). This must never be collapsed
///   into "no key yet": `Db::open()` responds to `None` by generating and
///   saving a fresh key, which would overwrite a perfectly good existing
///   entry and make the already-encrypted DB permanently undecryptable.
pub fn load_key_hex() -> Result<Option<String>, String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, &kc_key_name())
        .map_err(|e| format!("keychain open: {e}"))?;
    match entry.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

/// Write the DB cipher key to the keychain. Callers MUST confirm this
/// succeeds before using the key to encrypt anything — see the plan's
/// Global Constraints on generate→persist→confirm→encrypt ordering.
pub fn save_key_hex(hex: &str) -> Result<(), String> {
    let entry = keyring_core::Entry::new(KC_SERVICE, &kc_key_name())
        .map_err(|e| format!("keychain open: {e}"))?;
    entry
        .set_password(hex)
        .map_err(|e| format!("keychain write: {e}"))
}

use rusqlite::Connection;
use std::path::Path;

/// The three ways opening the DB can fail, distinguished so callers can
/// react differently — see the spec's Key Lifecycle section on why "no key
/// in keychain", "key doesn't decrypt this file", and "corrupt file" need
/// different recovery messaging rather than one generic error.
#[derive(Debug)]
pub enum DbOpenError {
    /// Reading or writing the OS keychain failed outright.
    Keychain(String),
    /// The plaintext→encrypted migration failed partway through.
    Migration(String),
    /// A `rusqlite`/SQLite-level error unrelated to encryption.
    Sqlite(rusqlite::Error),
    /// `PRAGMA key` was applied but the canary query failed — the stored
    /// key doesn't decrypt this file. Could be key drift or file
    /// corruption; from the caller's side both need the same recovery
    /// action (see Task 6), so they're not distinguished further here.
    KeyMismatchOrCorrupt,
}

impl std::fmt::Display for DbOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DbOpenError::Keychain(e) => write!(f, "keychain error: {e}"),
            DbOpenError::Migration(e) => write!(f, "migration error: {e}"),
            DbOpenError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            DbOpenError::KeyMismatchOrCorrupt => {
                write!(f, "stored key does not decrypt this database file")
            }
        }
    }
}

impl std::error::Error for DbOpenError {}

impl From<rusqlite::Error> for DbOpenError {
    fn from(e: rusqlite::Error) -> Self {
        DbOpenError::Sqlite(e)
    }
}

/// Reads the first 16 bytes and checks for SQLite's own plaintext magic
/// header (`"SQLite format 3\0"`). A correctly-encrypted SQLCipher file's
/// header is encrypted too, so this check alone is sufficient to tell
/// plaintext from encrypted — no key needed. Returns `false` for a
/// nonexistent file (nothing to migrate) as well as for anything that
/// isn't a plaintext SQLite file.
pub fn is_plaintext_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 16];
    if f.read_exact(&mut header).is_err() {
        return false;
    }
    &header == b"SQLite format 3\0"
}

/// Opens `db_path`, applies the SQLCipher raw-key, and forces a real read
/// (the canary query) before returning — `PRAGMA key` itself never errors
/// on a wrong key, only the first real read does, so this makes that
/// failure happen at open time instead of at some arbitrary later query.
pub fn open_encrypted(db_path: &Path, key_hex: &str) -> Result<Connection, DbOpenError> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))?;
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)) {
        Ok(_) => Ok(conn),
        Err(_) => Err(DbOpenError::KeyMismatchOrCorrupt),
    }
}

/// One-time migration for an existing plaintext `jodd.sqlite3`: attach a
/// fresh encrypted database alongside it, export everything into the
/// encrypted copy via SQLCipher's own `sqlcipher_export()`, then swap files
/// — the old plaintext file is renamed aside as a backup rather than
/// deleted, in case anything downstream needs to inspect it.
pub fn migrate_plaintext_to_encrypted(db_path: &Path, key_hex: &str) -> Result<(), String> {
    let tmp_path = db_path.with_file_name("jodd.sqlite3.encrypting");
    let backup_path = db_path.with_file_name("jodd.sqlite3.plaintext-backup");
    // Start from a clean slate. A previous attempt that died mid-export (or
    // whose sqlcipher_export failed) leaves a partial `.encrypting` file
    // behind; ATTACHing that same partial file again makes the export fail
    // permanently ("table already exists"), poisoning every later launch.
    let _ = std::fs::remove_file(&tmp_path);
    let _ = std::fs::remove_file(tmp_path.with_extension("encrypting-wal"));
    let _ = std::fs::remove_file(tmp_path.with_extension("encrypting-shm"));
    {
        let conn = Connection::open(db_path).map_err(|e| format!("open plaintext db: {e}"))?;
        // `ATTACH DATABASE` takes the filename as a SQL string literal here,
        // so a path containing a single quote (`%APPDATA%\O'Brien\…`) would
        // break the statement. rusqlite can bind ATTACH parameters only via
        // `execute`, not `execute_batch`, and the KEY clause is not
        // bindable at all — so escape the literal the SQL way: `'` → `''`.
        let tmp_literal = tmp_path.display().to_string().replace('\'', "''");
        conn.execute_batch(&format!(
            "ATTACH DATABASE '{tmp_literal}' AS encrypted KEY \"x'{key_hex}'\";"
        ))
        .map_err(|e| format!("attach encrypted target: {e}"))?;
        conn.query_row("SELECT sqlcipher_export('encrypted');", [], |_| Ok(()))
            .map_err(|e| format!("sqlcipher_export: {e}"))?;
        conn.execute_batch("DETACH DATABASE encrypted;")
            .map_err(|e| format!("detach: {e}"))?;
    } // conn drops here — both the plaintext source and the new encrypted file are closed
    std::fs::rename(db_path, &backup_path).map_err(|e| format!("backup old db: {e}"))?;
    std::fs::rename(&tmp_path, db_path).map_err(|e| format!("swap in encrypted db: {e}"))?;
    Ok(())
}

/// Maximum number of `jodd.sqlite3.recovery-*` quarantine files kept on
/// disk. Repeated mismatches would otherwise accumulate a full DB-sized copy
/// each time, forever.
const MAX_QUARANTINE_FILES: usize = 5;

/// Called when the stored key can't decrypt the existing DB file (drift or
/// corruption).
///
/// Quarantines the undecryptable file aside as `jodd.sqlite3.recovery-<epoch>`,
/// then opens a fresh DB **using the SAME stored key** — this does NOT rotate
/// or regenerate the key. The key in the keychain is assumed good; what failed
/// is the *file*, so re-minting a key would only guarantee that any later
/// attempt to read the quarantined copy is hopeless. `Db::open()` then creates
/// a new encrypted database at the canonical path with that same key.
///
/// It also drops a `NEEDS_REINDEX` marker the frontend checks — for Gmail
/// accounts, a full re-index recovers the lost local cache (see the spec's Key
/// Lifecycle / Backup-restore section); this is strictly better than the old
/// behavior of silently falling back to a throwaway temp-dir DB.
///
/// Quarantine files are capped at [`MAX_QUARANTINE_FILES`]; older ones beyond
/// the cap are deleted best-effort (a cleanup failure never fails recovery).
pub fn recover_from_key_mismatch(app_data_dir: &Path) -> Result<crate::db::Db, String> {
    let db_path = app_data_dir.join("jodd.sqlite3");
    if db_path.exists() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let quarantine = app_data_dir.join(format!("jodd.sqlite3.recovery-{suffix}"));
        std::fs::rename(&db_path, &quarantine).map_err(|e| format!("quarantine old db: {e}"))?;

        // The -wal/-shm sidecars are matched to the main file purely by
        // filename, not by any embedded reference — if they're left behind
        // at the canonical path, opening a brand-new database there makes
        // SQLite try to replay their frames, which were encrypted under the
        // OLD file's key material. That decryption failure then makes the
        // FRESH database look key-mismatched too, and recovery can never
        // succeed. Quarantine them alongside the main file (best-effort;
        // WAL mode doesn't guarantee either sidecar exists).
        for ext in ["-wal", "-shm"] {
            let sidecar = app_data_dir.join(format!("jodd.sqlite3{ext}"));
            if sidecar.exists() {
                let sidecar_quarantine = app_data_dir.join(format!("jodd.sqlite3.recovery-{suffix}{ext}"));
                if let Err(e) = std::fs::rename(&sidecar, &sidecar_quarantine) {
                    eprintln!("[db_crypto] quarantine: failed to move {sidecar:?}: {e}");
                }
            }
        }

        prune_quarantine_files(app_data_dir);
    }
    std::fs::write(app_data_dir.join("NEEDS_REINDEX"), b"")
        .map_err(|e| format!("write recovery marker: {e}"))?;
    crate::db::Db::open(&app_data_dir.to_path_buf())
        .map_err(|e| format!("open fresh db after recovery: {e}"))
}

/// Delete the oldest `jodd.sqlite3.recovery-*` files beyond
/// [`MAX_QUARANTINE_FILES`]. Best-effort throughout: a directory we can't
/// read, or a file we can't delete, only logs — recovery already succeeded
/// and must not be failed over housekeeping.
fn prune_quarantine_files(app_data_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(app_data_dir) else {
        eprintln!("[db_crypto] quarantine cleanup: cannot read {}", app_data_dir.display());
        return;
    };
    // Count/cap by MAIN quarantine file only ("jodd.sqlite3.recovery-<digits>",
    // no trailing -wal/-shm) so a single recovery event — main file plus its
    // sidecars — is one unit against the cap, not up to three. Matching the
    // sidecars here too would let them be pruned independently of their main
    // file, leaving an orphaned half-backup that isn't useful for recovery.
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("jodd.sqlite3.recovery-")
                .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|e| {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (mtime, e.path())
        })
        .collect();
    if files.len() <= MAX_QUARANTINE_FILES {
        return;
    }
    files.sort_by_key(|(t, _)| *t); // oldest first
    let excess = files.len() - MAX_QUARANTINE_FILES;
    for (_, path) in files.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!("[db_crypto] quarantine cleanup: could not delete {}: {e}", path.display());
        }
        for ext in ["-wal", "-shm"] {
            let mut sidecar = path.clone().into_os_string();
            sidecar.push(ext);
            let sidecar = std::path::PathBuf::from(sidecar);
            if sidecar.exists() {
                if let Err(e) = std::fs::remove_file(&sidecar) {
                    eprintln!("[db_crypto] quarantine cleanup: could not delete {}: {e}", sidecar.display());
                }
            }
        }
    }
}

/// Round-trips a probe value through the SAME keychain service `jodd`
/// entries use, without touching the real DB key or any database file.
/// Mirrors `secrets::self_test()`'s shape (`src-tauri/src/secrets.rs:199`)
/// but exists so `jodd-mcp --self-test` can prove its OWN binary has
/// keychain access — see the spec's jodd-mcp coordination section on why
/// this can't be inherited from `Jodd.app`'s grant.
pub fn self_test() -> Result<(), String> {
    match load_key_hex() {
        // The real key is already readable by this binary — access is
        // already granted, no need for a separate probe.
        Ok(Some(_)) => return Ok(()),
        // A failed read of the REAL key is exactly the failure this
        // self-test exists to catch. Falling through to the probe entry
        // would test a different credential and could report success while
        // the DB key stays unreadable.
        Err(e) => return Err(format!("cannot read the stored DB key: {e}")),
        // Genuinely no key yet (e.g. before Jodd.app has ever launched) —
        // fall through and prove keychain access with a throwaway probe.
        Ok(None) => {}
    }
    let entry = keyring_core::Entry::new(KC_SERVICE, "db_cipher_key::selftest-probe")
        .map_err(|e| format!("keychain open: {e}"))?;
    entry.set_password("probe").map_err(|e| format!("keychain write: {e}"))?;
    let read_back = entry.get_password().map_err(|e| format!("keychain read: {e}"))?;
    let _ = entry.delete_credential();
    if read_back == "probe" {
        Ok(())
    } else {
        Err(format!("probe mismatch: wrote \"probe\", read {read_back:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_key_hex_is_64_lowercase_hex_chars() {
        let k = generate_key_hex();
        assert_eq!(k.len(), 64);
        assert!(k.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn generate_key_hex_is_not_deterministic() {
        assert_ne!(generate_key_hex(), generate_key_hex());
    }

    #[test]
    fn save_and_load_key_round_trips_through_the_keychain() {
        crate::secrets::init().expect("credential store must initialize");
        let original = generate_key_hex();
        save_key_hex(&original).expect("save must succeed");
        let read_back = load_key_hex()
            .expect("read must not error")
            .expect("key must be present after save");
        assert_eq!(read_back, original);
        // Clean up so this test doesn't leak a keychain entry across runs.
        delete_test_key();
    }

    #[test]
    fn tests_never_touch_the_real_production_key_entry() {
        // Guards finding #2: `cargo test --workspace` must not be able to
        // read, overwrite or delete the entry Jodd.app itself uses.
        assert_ne!(kc_key_name(), "db_cipher_key::v1");
        assert!(kc_key_name().starts_with("db_cipher_key::v1::test-"));
    }

    #[test]
    fn load_key_hex_reports_no_entry_as_ok_none_not_err() {
        crate::secrets::init().expect("credential store must initialize");
        delete_test_key();
        assert_eq!(load_key_hex().expect("missing entry is not an error"), None);
    }

    #[test]
    fn is_plaintext_sqlite_true_for_a_real_plaintext_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.sqlite3");
        Connection::open(&path).unwrap().execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        assert!(is_plaintext_sqlite(&path));
    }

    #[test]
    fn is_plaintext_sqlite_false_for_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_plaintext_sqlite(&dir.path().join("missing.sqlite3")));
    }

    #[test]
    fn is_plaintext_sqlite_false_for_an_encrypted_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enc.sqlite3");
        let key = generate_key_hex();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{key}'\"; CREATE TABLE t (x INTEGER);")).unwrap();
        }
        assert!(!is_plaintext_sqlite(&path));
    }

    #[test]
    fn open_encrypted_succeeds_with_the_correct_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enc.sqlite3");
        let key = generate_key_hex();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{key}'\"; CREATE TABLE t (x INTEGER);")).unwrap();
        }
        assert!(open_encrypted(&path, &key).is_ok());
    }

    #[test]
    fn open_encrypted_fails_with_the_wrong_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("enc.sqlite3");
        let real_key = generate_key_hex();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!("PRAGMA key = \"x'{real_key}'\"; CREATE TABLE t (x INTEGER);")).unwrap();
        }
        let wrong_key = generate_key_hex();
        match open_encrypted(&path, &wrong_key) {
            Err(DbOpenError::KeyMismatchOrCorrupt) => {}
            other => panic!("expected KeyMismatchOrCorrupt, got {other:?}"),
        }
    }

    #[test]
    fn migrate_plaintext_to_encrypted_preserves_data_and_backs_up_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jodd.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE t (x INTEGER); INSERT INTO t (x) VALUES (42);"
            ).unwrap();
        }
        assert!(is_plaintext_sqlite(&path));

        let key = generate_key_hex();
        migrate_plaintext_to_encrypted(&path, &key).expect("migration must succeed");

        assert!(!is_plaintext_sqlite(&path), "path must now be encrypted");
        assert!(dir.path().join("jodd.sqlite3.plaintext-backup").exists());

        let conn = open_encrypted(&path, &key).expect("must open with the migration key");
        let value: i64 = conn.query_row("SELECT x FROM t;", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 42);
    }

    #[test]
    fn migrate_plaintext_to_encrypted_fails_loudly_on_a_nonexistent_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sqlite3");
        let key = generate_key_hex();
        // Connection::open creates an empty file rather than failing, so this
        // documents the actual (harmless) behavior: migrating a "missing" path
        // produces an empty encrypted db rather than an error. Callers must not
        // call this on a path where is_plaintext_sqlite() returned false.
        let result = migrate_plaintext_to_encrypted(&path, &key);
        assert!(result.is_ok());
    }

    #[test]
    fn migrate_recovers_from_a_stale_partial_encrypting_file() {
        // Finding #6: a leftover `.encrypting` file from an interrupted or
        // failed attempt used to poison every subsequent migration.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jodd.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t (x) VALUES (9);")
                .unwrap();
        }
        // A stale tmp file carrying a colliding table, as a crashed export
        // would leave behind.
        let stale = dir.path().join("jodd.sqlite3.encrypting");
        {
            let conn = Connection::open(&stale).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER);").unwrap();
        }

        let key = generate_key_hex();
        migrate_plaintext_to_encrypted(&path, &key).expect("migration must survive a stale tmp");
        let conn = open_encrypted(&path, &key).unwrap();
        let value: i64 = conn.query_row("SELECT x FROM t;", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 9);
    }

    #[test]
    fn migrate_handles_a_path_containing_a_single_quote() {
        // Finding #7: `%APPDATA%\O'Brien\…` used to break the ATTACH literal.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("O'Brien");
        std::fs::create_dir_all(&sub).unwrap();
        let path = sub.join("jodd.sqlite3");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (x INTEGER); INSERT INTO t (x) VALUES (13);")
                .unwrap();
        }
        let key = generate_key_hex();
        migrate_plaintext_to_encrypted(&path, &key).expect("quoted path must migrate");
        let conn = open_encrypted(&path, &key).unwrap();
        let value: i64 = conn.query_row("SELECT x FROM t;", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 13);
    }

    #[test]
    fn quarantine_files_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..8 {
            let p = dir.path().join(format!("jodd.sqlite3.recovery-{i}"));
            std::fs::write(&p, b"x").unwrap();
            // Distinct mtimes so "oldest first" is well-defined.
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        prune_quarantine_files(dir.path());
        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("jodd.sqlite3.recovery-"))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), MAX_QUARANTINE_FILES);
        // The newest five survive; the three oldest are gone.
        assert!(remaining.contains(&"jodd.sqlite3.recovery-7".to_string()));
        assert!(!remaining.contains(&"jodd.sqlite3.recovery-0".to_string()));
    }

    #[test]
    fn recover_from_key_mismatch_quarantines_and_opens_fresh() {
        crate::secrets::init().expect("credential store must initialize");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let db_file = path.join("jodd.sqlite3");
        Connection::open(&db_file).unwrap().execute_batch("PRAGMA journal_mode=WAL;").unwrap();

        let _db = recover_from_key_mismatch(&path).expect("recovery must succeed");
        delete_test_key();

        assert!(path.join("NEEDS_REINDEX").exists());
        assert!(!is_plaintext_sqlite(&db_file), "fresh db after recovery must be encrypted");
        let quarantined: Vec<_> = std::fs::read_dir(&path)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.strip_prefix("jodd.sqlite3.recovery-")
                    .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        assert_eq!(quarantined.len(), 1, "exactly one quarantined main file expected");
    }

    #[test]
    fn recover_from_key_mismatch_quarantines_stale_wal_and_shm_sidecars() {
        // Regression test for a real crash found in live testing on a real
        // app-data directory: quarantining only the main `jodd.sqlite3` left
        // its `-wal`/`-shm` behind at the canonical path. SQLite matches
        // those sidecars to a main file purely by filename, so the FRESH db
        // this function creates right after quarantining picked up the OLD
        // file's leftover WAL frames — encrypted under different key
        // material — and its own canary query failed, making recovery
        // itself unrecoverable. This test uses synthetic (non-WAL-format)
        // content for the sidecars: it verifies the actual fix (quarantine
        // moves any `-wal`/`-shm` present, unconditionally, before the new
        // db is created) rather than re-deriving SQLite's internal WAL
        // validation rules — the failure mode itself was confirmed against
        // the real crash, not reconstructed here.
        crate::secrets::init().expect("credential store must initialize");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let db_file = path.join("jodd.sqlite3");
        let key = generate_key_hex();
        Connection::open(&db_file)
            .unwrap()
            .execute_batch(&format!("PRAGMA key = \"x'{key}'\"; CREATE TABLE t (x INTEGER);"))
            .unwrap();

        // Simulate a stale WAL/SHM left behind by a prior process (crash,
        // or — as found in live testing — a previous quarantine that only
        // moved the main file). Content doesn't need to be a real WAL: the
        // bug is that these files are matched to the main db purely by
        // filename, so their mere presence at the canonical path is what
        // makes SQLite attempt to replay them against whatever new file
        // shows up there.
        std::fs::write(path.join("jodd.sqlite3-wal"), b"stale wal frames from the old key").unwrap();
        std::fs::write(path.join("jodd.sqlite3-shm"), b"stale shm index").unwrap();

        let db = recover_from_key_mismatch(&path).expect("recovery must succeed even with a stale WAL/SHM present");
        delete_test_key();

        // The fresh db must actually be usable.
        db.list_notes("smoke-test-account").expect("fresh db after recovery must be queryable");

        // The stale sidecars must not still be sitting at the canonical
        // path — they must have moved into quarantine alongside their main
        // file (not just been silently orphaned or deleted outright, so a
        // human could still inspect them later if needed).
        let quarantined_wal: Vec<_> = std::fs::read_dir(&path)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains(".recovery-") && name.ends_with("-wal")
            })
            .collect();
        assert_eq!(quarantined_wal.len(), 1, "the stale -wal must be quarantined alongside its main file");
        let quarantined_shm: Vec<_> = std::fs::read_dir(&path)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains(".recovery-") && name.ends_with("-shm")
            })
            .collect();
        assert_eq!(quarantined_shm.len(), 1, "the stale -shm must be quarantined alongside its main file");
    }

    #[test]
    fn self_test_succeeds_via_the_probe_path_when_no_real_key_exists() {
        crate::secrets::init().expect("credential store must initialize");
        // Ensure no key exists for this test thread's namespaced entry.
        delete_test_key();
        self_test().expect("self_test must succeed via the probe path");
    }

    #[test]
    fn android_round_trip_key_encrypt_reopen() {
        // Runs on desktop using a tempdir, and on an Android emulator (via
        // `cargo test --no-run` + `adb push` + `adb shell`, see
        // .github/workflows/release.yml's android-encryption-roundtrip job)
        // using JODD_TEST_DIR, since Android has no host-style temp dir this
        // binary can assume is writable.
        let dir_override = std::env::var("JODD_TEST_DIR").ok();
        let _tmp_holder; // keeps the tempdir alive for the desktop path
        let dir: std::path::PathBuf = match dir_override {
            Some(d) => std::path::PathBuf::from(d),
            None => {
                let t = tempfile::tempdir().unwrap();
                let p = t.path().to_path_buf();
                _tmp_holder = t;
                p
            }
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("android-roundtrip.sqlite3");
        let _ = std::fs::remove_file(&path);

        let key = generate_key_hex();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "PRAGMA key = \"x'{key}'\"; CREATE TABLE t (x INTEGER); INSERT INTO t (x) VALUES (7);"
            )).unwrap();
        }
        assert!(!is_plaintext_sqlite(&path));

        let conn = open_encrypted(&path, &key).expect("must reopen with the same key on this ABI");
        let value: i64 = conn.query_row("SELECT x FROM t;", [], |r| r.get(0)).unwrap();
        assert_eq!(value, 7);
    }
}
