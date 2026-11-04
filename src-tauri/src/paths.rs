// Platform filesystem seam. Everything else in this crate that needs a
// user-data location goes through here instead of calling `dirs::` directly,
// because `dirs::` has no correct answer on Android — the app-private
// sandbox is only discoverable through the Tauri app handle.
//
// Deliberate asymmetry: on desktop the fallback is the HISTORICAL
// `dirs::config_dir()` / `dirs::data_dir()`, not the Tauri-resolved app dir.
// Tauri resolves to `<base>/co.bbmedia.jodd` whereas this app has always used
// `<base>/jodd`. Adopting the Tauri path would silently orphan every existing
// install's accounts.json. This seam exists to make the platform difference
// explicit and testable, NOT to relocate desktop data.
use std::path::PathBuf;
use std::sync::OnceLock;

static CONFIG_BASE: OnceLock<PathBuf> = OnceLock::new();
static DATA_BASE: OnceLock<PathBuf> = OnceLock::new();
static CACHE_BASE: OnceLock<PathBuf> = OnceLock::new();

/// Pure resolution rule, extracted so it is testable without touching the
/// process-global OnceLocks (which cannot be reset between tests).
fn resolve(explicit: Option<&PathBuf>, fallback: Option<PathBuf>) -> Option<PathBuf> {
    explicit.cloned().or(fallback)
}

/// Called once from the Tauri `.setup()` hook. Later calls are ignored.
pub fn init(config_base: PathBuf, data_base: PathBuf, cache_base: PathBuf) {
    let _ = CONFIG_BASE.set(config_base);
    let _ = DATA_BASE.set(data_base);
    let _ = CACHE_BASE.set(cache_base);
}

#[cfg(not(target_os = "android"))]
fn config_fallback() -> Option<PathBuf> {
    dirs::config_dir()
}

#[cfg(target_os = "android")]
fn config_fallback() -> Option<PathBuf> {
    None // no valid answer before init(); callers degrade gracefully
}

#[cfg(not(target_os = "android"))]
fn data_fallback() -> Option<PathBuf> {
    dirs::data_dir()
}

#[cfg(target_os = "android")]
fn data_fallback() -> Option<PathBuf> {
    None
}

#[cfg(not(target_os = "android"))]
fn cache_fallback() -> Option<PathBuf> {
    dirs::cache_dir()
}

#[cfg(target_os = "android")]
fn cache_fallback() -> Option<PathBuf> {
    None
}

/// Directory CONTAINING the `jodd` config folder. Callers keep their existing
/// `.join("jodd")` so desktop paths stay byte-identical.
pub fn config_base() -> Option<PathBuf> {
    resolve(CONFIG_BASE.get(), config_fallback())
}

/// Directory CONTAINING the `jodd` data folder (SQLite cache).
pub fn data_base() -> Option<PathBuf> {
    resolve(DATA_BASE.get(), data_fallback())
}

/// Directory CONTAINING the `jodd` cache folder (scratch files the OS may
/// reclaim — on Android, the app cache dir, which it clears under pressure).
pub fn cache_base() -> Option<PathBuf> {
    resolve(CACHE_BASE.get(), cache_fallback())
}

/// Prefix of the per-source scratch entries under `cache_base()/jodd/` that
/// URL ingest creates for downloaded media.
pub const INGEST_TEMP_PREFIX: &str = "ingest-";

/// An ingest entry younger than this is left alone: an installed Jodd and
/// `tauri dev` share the cache folder, so a fresh one may be the other
/// instance's download in progress.
const INGEST_TEMP_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Delete whatever a crashed or killed ingest left under `cache_base()/jodd/`.
/// Call once at startup, before any ingest can run.
pub fn sweep_ingest_temp() {
    if let Some(base) = cache_base() {
        sweep_ingest_temp_in(&base, std::time::SystemTime::now());
    }
}

fn sweep_ingest_temp_in(base: &std::path::Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(base.join("jodd")) else { return };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(INGEST_TEMP_PREFIX) {
            continue;
        }
        // An unreadable or future mtime is not "old": keep it for a later start.
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        if !modified.and_then(|m| now.duration_since(m).ok()).is_some_and(|age| age >= INGEST_TEMP_MIN_AGE) {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn the_sweep_removes_ingest_entries_and_nothing_else() {
        let base = tempfile::tempdir().unwrap();
        let jodd = base.path().join("jodd");
        std::fs::create_dir_all(jodd.join("ingest-abc")).unwrap();
        std::fs::write(jodd.join("ingest-abc/clip.mp4"), b"x").unwrap();
        std::fs::write(jodd.join("ingest-loose.m4a"), b"x").unwrap();
        std::fs::create_dir_all(jodd.join("other")).unwrap();
        std::fs::write(jodd.join("keep.txt"), b"x").unwrap();

        // Two hours from now, every entry is old enough.
        sweep_ingest_temp_in(base.path(), SystemTime::now() + Duration::from_secs(2 * 3600));

        assert!(!jodd.join("ingest-abc").exists());
        assert!(!jodd.join("ingest-loose.m4a").exists());
        assert!(jodd.join("other").exists());
        assert!(jodd.join("keep.txt").exists());
    }

    /// An installed Jodd and `tauri dev` share the cache folder: a file the
    /// other one is downloading right now must survive this one's start.
    #[test]
    fn the_sweep_spares_entries_modified_in_the_last_hour() {
        let base = tempfile::tempdir().unwrap();
        let jodd = base.path().join("jodd");
        std::fs::create_dir_all(&jodd).unwrap();
        let (fresh, old) = (jodd.join("ingest-fresh.mp4"), jodd.join("ingest-old.mp4"));
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&old, b"x").unwrap();
        let two_hours_ago = SystemTime::now() - Duration::from_secs(2 * 3600);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(two_hours_ago).unwrap();

        sweep_ingest_temp_in(base.path(), SystemTime::now());

        assert!(fresh.exists(), "modified a moment ago: another instance may own it");
        assert!(!old.exists(), "two hours old: left by a crash");
    }

    #[test]
    fn the_sweep_tolerates_a_missing_cache_folder() {
        sweep_ingest_temp_in(&PathBuf::from("/nonexistent/for/this/test"), SystemTime::now());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_cache_fallback_is_the_dirs_location() {
        assert_eq!(cache_base(), dirs::cache_dir());
    }

    #[test]
    fn resolve_prefers_the_explicit_value() {
        let explicit = PathBuf::from("/explicit");
        let got = resolve(Some(&explicit), Some(PathBuf::from("/fallback")));
        assert_eq!(got, Some(PathBuf::from("/explicit")));
    }

    #[test]
    fn resolve_uses_the_fallback_when_unset() {
        let got = resolve(None, Some(PathBuf::from("/fallback")));
        assert_eq!(got, Some(PathBuf::from("/fallback")));
    }

    #[test]
    fn resolve_is_none_when_neither_is_available() {
        assert_eq!(resolve(None, None), None);
    }

    // These two only pin the FALLBACK. `config_base()` returns
    // `config_fallback()` here because nothing in the test binary calls
    // `init()`, and `config_fallback()` is literally `dirs::config_dir()` — so
    // the assertion cannot fail, and it does not guard what it looks like it
    // guards. Kept because they do lock the fallback against being changed to
    // `app_config_dir()`-style values, and named to say only that.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_config_fallback_is_the_historical_dirs_location() {
        assert_eq!(config_base(), dirs::config_dir());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_data_fallback_is_the_historical_dirs_location() {
        assert_eq!(data_base(), dirs::data_dir());
    }

    /// The invariant that actually matters, and the only test here that can
    /// fail for the right reason.
    ///
    /// In a real run `init()` IS called, from Tauri's `.setup()`, and whatever
    /// it passes wins over the fallback — so the two tests above say nothing
    /// about the shipped path. The damage from getting it wrong is total and
    /// silent: `app_config_dir()` appends the bundle identifier, yielding
    /// `.../co.bbmedia.jodd` instead of the `.../jodd` every existing install
    /// uses, and the app starts up cheerfully with zero accounts. That was
    /// observed once during this branch's development, which is why it is
    /// worth an unusual test rather than a comment.
    ///
    /// Asserting on source text is crude, and it is the only option: the call
    /// site needs a live `AppHandle`, which a unit test has no way to build.
    /// A grep-shaped test that fails loudly beats an invariant with nothing
    /// watching it at all.
    ///
    /// Comments are stripped first, and that is load-bearing in both
    /// directions — the first draft of this test failed against correct code.
    /// The call site is preceded by a long comment that names `paths::init()`
    /// and warns against `app_config_dir()`/`app_data_dir()` by name, so an
    /// unfiltered scan anchors on the wrong `paths::init` and then trips the
    /// negative assertion on the very comment explaining why the code is right.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn setup_hook_uses_the_unsuffixed_dirs_not_the_bundle_id_ones() {
        let src = include_str!("lib.rs");
        let setup = src
            .split_once(".setup(|app|")
            .expect("lib.rs must still have a .setup hook")
            .1;

        let code: String = setup
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // Look only at the head of the hook, where paths::init is wired up.
        let head = &code[..code
            .find("paths::init")
            .expect("setup must call paths::init")];

        assert!(
            head.contains(".config_dir()") && head.contains(".data_dir()") && head.contains(".cache_dir()"),
            "the setup hook must resolve paths with config_dir()/data_dir()/cache_dir()"
        );
        assert!(
            !head.contains("app_config_dir") && !head.contains("app_data_dir") && !head.contains("app_cache_dir"),
            "the setup hook resolves paths with app_config_dir()/app_data_dir()/app_cache_dir(), which append \
             the bundle identifier and orphan every existing install's accounts.json and cache"
        );
    }
}
