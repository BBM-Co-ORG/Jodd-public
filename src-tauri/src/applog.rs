// Optional persistent file logging for the `log!` macro. Motivation: when an
// account mysteriously disappears or a sync issue happens, the app's own
// diagnostic lines (`log!` calls throughout lib.rs / gmail/wire.rs) only ever
// went to stderr — invisible unless the app happened to be launched from a
// terminal at the time. macOS's unified log does NOT capture a GUI app's raw
// stderr, so by the time anyone thinks to look, the evidence is gone. This
// module gives `log!` a second, persistent destination so a future incident
// is diagnosable after the fact.
//
// User-toggleable (privacy / disk-space concern for a long-running app with a
// 5s sync tick), default ON so the next incident isn't lost to "logging was
// off".
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppLogSettings {
    pub file_logging_enabled: bool,
}

impl Default for AppLogSettings {
    fn default() -> Self {
        Self { file_logging_enabled: true }
    }
}

// `_under(base)` variants take the config-dir base as a parameter so tests
// can point them at a tempdir instead of the user's real config location —
// this module writes files (settings + a potentially-rotated log), which is
// riskier to leak into a test run against real user data than a read-only
// check would be.

fn settings_path_under(base: &std::path::Path) -> Result<PathBuf, String> {
    let dir = base.join("jodd");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    Ok(dir.join("app_settings.json"))
}

fn settings_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    settings_path_under(&base)
}

fn log_file_path_under(base: &std::path::Path) -> Result<PathBuf, String> {
    let dir = base.join("jodd").join("logs");
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    Ok(dir.join("jodd.log"))
}

/// Same base dir as accounts.json/google_oauth.json, `logs/` subfolder so the
/// (potentially large) log file doesn't clutter the small config files.
pub fn log_file_path() -> Result<PathBuf, String> {
    let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
    log_file_path_under(&base)
}

fn load_settings_from(path: &std::path::Path) -> AppLogSettings {
    if !path.exists() {
        return AppLogSettings::default();
    }
    fs::read_to_string(path)
        .ok()
        .and_then(|txt| serde_json::from_str(&txt).ok())
        .unwrap_or_default()
}

pub fn load_settings() -> AppLogSettings {
    match settings_path() {
        Ok(p) => load_settings_from(&p),
        Err(_) => AppLogSettings::default(),
    }
}

fn save_settings_to(path: &std::path::Path, settings: &AppLogSettings) -> Result<(), String> {
    let txt = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, txt).map_err(|e| format!("write {}: {}", path.display(), e))
}

fn save_settings(settings: &AppLogSettings) -> Result<(), String> {
    let p = settings_path()?;
    save_settings_to(&p, settings)
}

/// Renames the log file out of the way if it's grown past the cap. Pure
/// function of a path so it's testable without touching the real log file.
fn rotate_if_oversized(path: &std::path::Path) {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > MAX_LOG_BYTES {
            let _ = fs::rename(path, path.with_extension("log.old"));
        }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(true);
static FILE_HANDLE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

// Single-backup rotation cap. The sync worker ticks every 5s and logs on
// most ticks, so an unbounded file would grow indefinitely over weeks of
// uptime — this is a safety net, not a full rotation scheme.
const MAX_LOG_BYTES: u64 = 20 * 1024 * 1024;

/// Call once, as the very first thing in `run()` — before anything else logs
/// — so the persisted enabled/disabled choice is honored from the first line
/// (including the startup .env / credentials messages).
pub fn init() {
    let settings = load_settings();
    ENABLED.store(settings.file_logging_enabled, Ordering::Relaxed);
    if settings.file_logging_enabled {
        open_file();
    }
}

fn open_file() {
    let path = match log_file_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    rotate_if_oversized(&path);
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        // OnceLock — only the first call actually stores anything, matching
        // "open once for the process lifetime" (see set_enabled below).
        let _ = FILE_HANDLE.set(Mutex::new(Some(f)));
    }
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Toggle at runtime from the App Settings UI — no restart required.
/// Re-enabling after a disable resumes appending to the SAME file handle
/// opened at startup (or opens it fresh if this is the first-ever enable).
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    ENABLED.store(enabled, Ordering::Relaxed);
    if enabled && FILE_HANDLE.get().is_none() {
        open_file();
    }
    save_settings(&AppLogSettings { file_logging_enabled: enabled })
}

/// Called by the `log!` macro for every line. Cheap no-op when disabled —
/// one atomic load, no allocation, no lock.
pub fn write_line(line: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Some(mutex) = FILE_HANDLE.get() {
        if let Ok(mut guard) = mutex.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
}

/// Current log file size in bytes, for the Diagnostics UI ("N KB" next to
/// the path). 0 if the file doesn't exist yet (logging just enabled, or
/// never written to).
pub fn log_file_size() -> u64 {
    log_file_path()
        .ok()
        .and_then(|p| fs::metadata(&p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Drops the rotated `.log.old` backup at `path`'s sibling, if any. Split out
/// as a pure path function so it's testable against a tempdir.
fn remove_backup_at(path: &std::path::Path) {
    let backup = path.with_extension("log.old");
    if backup.exists() {
        let _ = fs::remove_file(&backup);
    }
}

/// Truncates the file at `path` to empty if it exists. Pure path function —
/// used by `clear_log` for the "logging currently disabled" case, and
/// directly testable against a tempdir.
fn truncate_file_at(path: &std::path::Path) -> Result<(), String> {
    if path.exists() {
        fs::write(path, "").map_err(|e| format!("truncate {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// User-triggered reset from the Diagnostics UI ("Clear log"), on top of the
/// automatic 20MB rotation — lets the user reclaim space immediately instead
/// of waiting for the file to hit the cap.
///
/// Truncates in place via the SAME open handle `write_line` appends through,
/// rather than deleting the path: unlinking a file that's still open leaves
/// the open fd writing into the now-nameless inode (invisible at the path,
/// disk space not reclaimed until the process exits) and nothing would exist
/// at `log_file_path()` for future appends to recreate without a restart.
/// Also drops the rotated `.log.old` backup, if any, for a full reset.
pub fn clear_log() -> Result<(), String> {
    let path = log_file_path()?;
    if let Some(mutex) = FILE_HANDLE.get() {
        let mut guard = mutex.lock().map_err(|_| "log file mutex poisoned".to_string())?;
        if let Some(f) = guard.as_mut() {
            f.set_len(0).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        }
    } else {
        // Logging is currently disabled (no open handle) but a file from an
        // earlier enabled period may still exist on disk — clear it too.
        truncate_file_at(&path)?;
    }
    remove_backup_at(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Every test builds its own tempdir base and calls the `_under`/`_from`/
    // `_to` variants directly — NEVER the bare `settings_path()` /
    // `log_file_path()` (those resolve to the user's real config dir, which
    // a test must not read or write).

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "jodd-applog-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        p.push(unique);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn default_settings_is_file_logging_enabled() {
        assert!(AppLogSettings::default().file_logging_enabled);
    }

    #[test]
    fn load_settings_from_missing_file_returns_default() {
        let base = tempdir();
        let path = settings_path_under(&base).unwrap();
        assert!(!path.exists());
        let loaded = load_settings_from(&path);
        assert!(loaded.file_logging_enabled);
    }

    #[test]
    fn save_then_load_settings_roundtrips() {
        let base = tempdir();
        let path = settings_path_under(&base).unwrap();
        save_settings_to(&path, &AppLogSettings { file_logging_enabled: false }).unwrap();
        let loaded = load_settings_from(&path);
        assert!(!loaded.file_logging_enabled);

        save_settings_to(&path, &AppLogSettings { file_logging_enabled: true }).unwrap();
        let loaded = load_settings_from(&path);
        assert!(loaded.file_logging_enabled);
    }

    #[test]
    fn load_settings_from_corrupt_file_returns_default_instead_of_panicking() {
        let base = tempdir();
        let path = settings_path_under(&base).unwrap();
        fs::write(&path, "not valid json").unwrap();
        let loaded = load_settings_from(&path);
        assert!(loaded.file_logging_enabled);
    }

    #[test]
    fn log_file_path_is_under_a_logs_subdirectory() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        assert_eq!(path.file_name().unwrap(), "jodd.log");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "logs");
    }

    #[test]
    fn rotate_if_oversized_leaves_small_file_untouched() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        fs::write(&path, "small").unwrap();
        rotate_if_oversized(&path);
        assert!(path.exists());
        assert!(!path.with_extension("log.old").exists());
    }

    #[test]
    fn rotate_if_oversized_renames_file_past_the_cap() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        let oversized = vec![b'x'; (MAX_LOG_BYTES + 1) as usize];
        fs::write(&path, &oversized).unwrap();
        rotate_if_oversized(&path);
        assert!(!path.exists(), "original should have been moved away");
        assert!(path.with_extension("log.old").exists());
    }

    #[test]
    fn truncate_file_at_empties_existing_content() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        fs::write(&path, "some log lines\nmore lines\n").unwrap();
        truncate_file_at(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn truncate_file_at_missing_file_is_a_no_op() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        assert!(!path.exists());
        truncate_file_at(&path).unwrap(); // must not error just because there's nothing to clear
    }

    #[test]
    fn remove_backup_at_deletes_the_log_old_sibling() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        let backup = path.with_extension("log.old");
        fs::write(&backup, "stale rotated content").unwrap();
        remove_backup_at(&path);
        assert!(!backup.exists());
    }

    #[test]
    fn remove_backup_at_missing_backup_is_a_no_op() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        remove_backup_at(&path); // no .log.old exists — must not panic or error
    }

    #[test]
    fn log_file_size_under_reflects_actual_bytes_on_disk() {
        let base = tempdir();
        let path = log_file_path_under(&base).unwrap();
        fs::write(&path, "12345").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().len(), 5);
    }
}
