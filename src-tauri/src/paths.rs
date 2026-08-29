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

/// Pure resolution rule, extracted so it is testable without touching the
/// process-global OnceLocks (which cannot be reset between tests).
fn resolve(explicit: Option<&PathBuf>, fallback: Option<PathBuf>) -> Option<PathBuf> {
    explicit.cloned().or(fallback)
}

/// Called once from the Tauri `.setup()` hook. Later calls are ignored.
pub fn init(config_base: PathBuf, data_base: PathBuf) {
    let _ = CONFIG_BASE.set(config_base);
    let _ = DATA_BASE.set(data_base);
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

/// Directory CONTAINING the `jodd` config folder. Callers keep their existing
/// `.join("jodd")` so desktop paths stay byte-identical.
pub fn config_base() -> Option<PathBuf> {
    resolve(CONFIG_BASE.get(), config_fallback())
}

/// Directory CONTAINING the `jodd` data folder (SQLite cache).
pub fn data_base() -> Option<PathBuf> {
    resolve(DATA_BASE.get(), data_fallback())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            head.contains(".config_dir()") && head.contains(".data_dir()"),
            "the setup hook must resolve paths with config_dir()/data_dir()"
        );
        assert!(
            !head.contains("app_config_dir") && !head.contains("app_data_dir"),
            "the setup hook resolves paths with app_config_dir()/app_data_dir(), which append \
             the bundle identifier and orphan every existing install's accounts.json and cache"
        );
    }
}
