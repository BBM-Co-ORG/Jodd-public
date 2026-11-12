//! Android in-app update: check Jodd-public's latest release, download its
//! APK into the app cache, verify it, and hand it to Android's system
//! Package installer.
//!
//! Why this exists (measured on a Galaxy Tab S7, Android 13, 2026-11-10):
//! installing a release APK by tapping it in **Google Files** failed five
//! times with "The app wasn't installed." Play Protect returned ALLOW every
//! time, yet PackageManager never started the install and logged no reason.
//! The same file installed through Samsung My Files → **Package installer**
//! in 21 s, and over adb. Google Files runs its own install session; the
//! system Package installer runs the one that works. So Jodd hands the file
//! to the system installer itself (`ACTION_INSTALL_PACKAGE`, which only the
//! installer answers — a plain `ACTION_VIEW` also offered two AI apps in a
//! chooser on that tablet), and the user never goes through a file manager.
//!
//! Trust: the APK must be the asset named for the release's own version, at
//! Jodd-public's download path, and must match the size and SHA-256 that
//! GitHub's API publishes for it. The frontend only names a version; the
//! install command re-reads the release itself and never accepts a URL or a
//! hash from the UI. Android still refuses an APK signed with another key.

use serde::{Deserialize, Serialize};

/// Jodd-public is the repository releases are published from (since v0.39.0).
const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/BBM-Co-ORG/Jodd-public/releases/latest";
const DOWNLOAD_PREFIX: &str = "https://github.com/BBM-Co-ORG/Jodd-public/releases/download/";
/// A release APK is ~30 MB; anything far outside that is not one.
const MAX_APK_BYTES: u64 = 200 * 1024 * 1024;

/// What `pick_update` decided, with what the install needs. Internal: the
/// frontend sees `super::AvailableUpdate`, which carries neither the URL nor
/// the hash — `install` re-reads the release itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AndroidUpdate {
    pub version: String,
    pub apk_url: String,
    pub size: u64,
    /// Lowercase hex, without the `sha256:` prefix.
    pub sha256: String,
    pub release_url: String,
}

impl From<AndroidUpdate> for super::AvailableUpdate {
    fn from(u: AndroidUpdate) -> Self {
        super::AvailableUpdate {
            version: u.version,
            size: Some(u.size),
            release_url: u.release_url,
            method: if super::ANDROID_SELF_INSTALL {
                super::InstallMethod::AndroidInstaller
            } else {
                super::InstallMethod::ReleasePage
            },
            msi: false,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    size: u64,
    /// `sha256:<hex>`; GitHub fills it for every asset uploaded since mid-2025.
    digest: Option<String>,
    browser_download_url: String,
}

/// `0.45.0` or `v0.45.0` → (0, 45, 0). Anything else — a pre-release suffix
/// such as `-dryrun.3`, fewer or more parts — is not a version Jodd offers.
pub(crate) fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let v = (next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(v)
}

/// The asset name the release workflow gives the signed APK.
pub(crate) fn apk_asset_name(version: &str) -> String {
    format!("Jodd_v{version}_android-universal.apk")
}

/// Decide whether `release` is an update over `current`, and if so which file
/// to fetch. Pure, so every refusal is testable without a network.
pub(crate) fn pick_update(release: &Release, current: &str) -> Result<Option<AndroidUpdate>, String> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let latest = parse_version(&release.tag_name)
        .ok_or_else(|| format!("unexpected release tag {:?}", release.tag_name))?;
    let installed =
        parse_version(current).ok_or_else(|| format!("unexpected app version {current:?}"))?;
    if latest <= installed {
        return Ok(None);
    }
    let version = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    let name = apk_asset_name(&version);
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("release {version} has no Android APK ({name})"))?;
    let expected_url = format!("{DOWNLOAD_PREFIX}{}/{name}", release.tag_name);
    if asset.browser_download_url != expected_url {
        return Err(format!("release {version}'s APK is not at Jodd-public's download path"));
    }
    if asset.size == 0 || asset.size > MAX_APK_BYTES {
        return Err(format!("release {version}'s APK has an implausible size ({} bytes)", asset.size));
    }
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|h| h.to_ascii_lowercase())
        .ok_or_else(|| format!("release {version}'s APK has no SHA-256 to check it against"))?;
    Ok(Some(AndroidUpdate {
        version,
        apk_url: asset.browser_download_url.clone(),
        size: asset.size,
        sha256,
        release_url: release.html_url.clone(),
    }))
}

/// Downloaded APKs that are not newer than `current` are spent: delete them.
/// File names carry the version (`Jodd_v0.46.0.apk`); anything else in the
/// folder — a `.part` from a killed download included — goes too.
pub(crate) fn prune_spent_downloads(dir: &std::path::Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let installed = parse_version(current);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let keep = name
            .strip_prefix("Jodd_v")
            .and_then(|r| r.strip_suffix(".apk"))
            .and_then(parse_version)
            .zip(installed)
            .is_some_and(|(v, i)| v > i);
        if !keep {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
async fn fetch_latest(current: &str) -> Result<Option<AndroidUpdate>, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        // GitHub's API rejects requests without a User-Agent. Product and
        // version only — nothing about the user or the device.
        .user_agent(format!("Jodd/{current}"))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get(RELEASES_LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("couldn't reach GitHub: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub answered {} for the latest release", resp.status()));
    }
    let release: Release = resp.json().await.map_err(|e| format!("unreadable release data: {e}"))?;
    pick_update(&release, current)
}

// ─── Android: the Kotlin side ───────────────────────────────────────────────

use tauri::plugin::{Builder, TauriPlugin};
use tauri::Runtime;

#[cfg(target_os = "android")]
pub(crate) struct UpdateHandle<R: Runtime>(pub tauri::plugin::PluginHandle<R>);

/// Registers the Kotlin `UpdatePlugin` on Android; a no-op plugin elsewhere.
/// Does no IPC during setup (gotcha #32).
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("jodd-update")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin("co.bbmedia.jodd", "UpdatePlugin")?;
                _app.manage(UpdateHandle(handle));
            }
            Ok(())
        })
        .build()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Deserialize)]
struct Granted {
    granted: bool,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Deserialize)]
struct Folder {
    path: String,
}

/// The folder APKs are downloaded to, as `UpdatePlugin.kt` defines it — the
/// only folder its `installApk` accepts. Never derived here: 0.46.0 used
/// Tauri's `cache_dir()`, which on Android is the external cache, and every
/// install was refused (S23 FE, 2026-11-12).
#[cfg(target_os = "android")]
async fn updates_dir(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    let folder = plugin(app)?
        .0
        .run_mobile_plugin_async::<Folder>("updatesDir", ())
        .await
        .map_err(|e| e.to_string())?;
    Ok(std::path::PathBuf::from(folder.path))
}

#[cfg(target_os = "android")]
fn plugin(app: &tauri::AppHandle) -> Result<tauri::State<'_, UpdateHandle<tauri::Wry>>, String> {
    use tauri::Manager;
    app.try_state::<UpdateHandle<tauri::Wry>>().ok_or_else(|| "the update plugin is not registered".to_string())
}

/// One install at a time: a second tap while a download runs would race the
/// first for the same file.
#[cfg(target_os = "android")]
static INSTALLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "android")]
struct InstallingGuard;
#[cfg(target_os = "android")]
impl Drop for InstallingGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    received: u64,
    total: u64,
}

/// Download `update` to `dest`, hashing as it streams, and keep the file only
/// if size and SHA-256 both match. A complete earlier download is reused.
#[cfg(target_os = "android")]
async fn download_verified(
    app: &tauri::AppHandle,
    update: &AndroidUpdate,
    dest: &std::path::Path,
    current: &str,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use tauri::Emitter;

    if let Ok(bytes) = std::fs::read(dest) {
        if bytes.len() as u64 == update.size && to_hex(&Sha256::digest(&bytes)) == update.sha256 {
            return Ok(());
        }
        let _ = std::fs::remove_file(dest);
    }
    let part = dest.with_extension("apk.part");
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .timeout(std::time::Duration::from_secs(15 * 60))
        .user_agent(format!("Jodd/{current}"))
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client
        .get(&update.apk_url)
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: GitHub answered {}", resp.status()));
    }
    let mut file = std::fs::File::create(&part).map_err(|e| format!("couldn't save the download: {e}"))?;
    let mut hasher = Sha256::new();
    let mut received: u64 = 0;
    let mut last_pct = u64::MAX;
    let result: Result<(), String> = async {
        while let Some(chunk) = resp.chunk().await.map_err(|e| format!("download interrupted: {e}"))? {
            received += chunk.len() as u64;
            if received > update.size {
                return Err("the download is larger than the release says".to_string());
            }
            hasher.update(&chunk);
            file.write_all(&chunk).map_err(|e| format!("couldn't save the download: {e}"))?;
            let pct = received * 100 / update.size;
            if pct != last_pct {
                last_pct = pct;
                let _ = app.emit("app-update-progress", Progress { received, total: update.size });
            }
        }
        file.sync_all().map_err(|e| format!("couldn't save the download: {e}"))?;
        if received != update.size {
            return Err(format!("the download stopped at {received} of {} bytes", update.size));
        }
        if to_hex(&hasher.finalize()) != update.sha256 {
            return Err("the download doesn't match the release's SHA-256; nothing was installed".to_string());
        }
        Ok(())
    }
    .await;
    drop(file);
    match result {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| format!("couldn't save the download: {e}")),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

// ─── What mod.rs's commands call on Android ─────────────────────────────────

/// Read the latest release. Network only — touches no notes or accounts.
#[cfg(target_os = "android")]
pub(crate) async fn check(app: &tauri::AppHandle) -> Result<super::UpdateCheck, String> {
    let current = app.package_info().version.to_string();
    if let Ok(dir) = updates_dir(app).await {
        prune_spent_downloads(&dir, &current);
    }
    let available = fetch_latest(&current).await?.map(Into::into);
    Ok(super::UpdateCheck { current, available })
}

/// Download, verify and hand `version` to the system installer. `version` is
/// only a confirmation of what the user saw: the release is re-read here, and
/// if the latest is no longer that version the command refuses rather than
/// installing something the user did not choose.
#[cfg(target_os = "android")]
pub(crate) async fn install(app: &tauri::AppHandle, version: &str) -> Result<super::InstallOutcome, String> {
    use super::InstallOutcome;
    use std::sync::atomic::Ordering;
    if !super::ANDROID_SELF_INSTALL {
        // The manifest has no REQUEST_INSTALL_PACKAGES: install from the release page.
        return Err("Install this update from the release page.".to_string());
    }
    let app = app.clone();
    // Check "Install unknown apps" first, so nobody waits through a
    // download only to be told to go to Settings. Only checked here:
    // asking is `request_install_permission`, a call of its own, because
    // it may never return (UpdatePlugin.kt) and must not hold this one.
    let access = plugin(&app)?
        .0
        .run_mobile_plugin_async::<Granted>("canInstallPackages", ())
        .await
        .map_err(|e| e.to_string())?;
    if !access.granted {
        return Ok(InstallOutcome::NeedsPermission);
    }

    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("an update is already downloading".to_string());
    }
    let _guard = InstallingGuard;

    let current = app.package_info().version.to_string();
    let update = fetch_latest(&current)
        .await?
        .ok_or_else(|| format!("Jodd {current} is already the latest version"))?;
    if update.version != version {
        return Err(format!(
            "the latest release is now {}, not {version}; check again",
            update.version
        ));
    }
    let dir = updates_dir(&app).await?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create the download folder: {e}"))?;
    prune_spent_downloads(&dir, &current);
    let dest = dir.join(format!("Jodd_v{}.apk", update.version));
    download_verified(&app, &update, &dest, &current).await?;

    #[derive(Serialize)]
    struct InstallArgs {
        path: String,
    }
    plugin(&app)?
        .0
        .run_mobile_plugin_async::<serde_json::Value>(
            "installApk",
            InstallArgs { path: dest.to_string_lossy().into_owned() },
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(InstallOutcome::HandedOff)
}

/// Open Settings › Install unknown apps for Jodd and say whether it is now
/// allowed. Separate from `install` and holding nothing: if Android recreates
/// the activity while Settings is open, this call never returns
/// (UpdatePlugin.kt), and the frontend must stay free to try again.
#[cfg(target_os = "android")]
pub(crate) async fn request_permission(app: &tauri::AppHandle) -> Result<bool, String> {
    let access = plugin(app)?
        .0
        .run_mobile_plugin_async::<Granted>("requestInstallPermission", ())
        .await
        .map_err(|e| e.to_string())?;
    Ok(access.granted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: Vec<Asset>) -> Release {
        Release {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            html_url: format!("https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/{tag}"),
            assets,
        }
    }

    fn apk(version: &str) -> Asset {
        let name = apk_asset_name(version);
        Asset {
            browser_download_url: format!("{DOWNLOAD_PREFIX}v{version}/{name}"),
            name,
            size: 30_426_016,
            digest: Some("sha256:83D1325C5E61525F6C108E1421C9F0ACC70C8427083B0C0D206BB26023B443A5".into()),
        }
    }

    #[test]
    fn versions_parse_only_as_plain_triples() {
        assert_eq!(parse_version("0.45.0"), Some((0, 45, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        for bad in ["0.45", "0.45.0.1", "v0.46.0-dryrun.3", "0.4x.0", "", "v", "0..1", "+1.2.3"] {
            assert_eq!(parse_version(bad), None, "{bad:?} must not parse");
        }
    }

    #[test]
    fn versions_compare_numerically_not_as_text() {
        // "0.10.0" > "0.9.9" numerically, but not as a string.
        let r = release("v0.10.0", vec![apk("0.10.0")]);
        assert_eq!(pick_update(&r, "0.9.9").unwrap().unwrap().version, "0.10.0");
    }

    #[test]
    fn a_newer_release_yields_its_apk_with_a_lowercase_hash() {
        let r = release("v0.46.0", vec![apk("0.46.0")]);
        let u = pick_update(&r, "0.45.0").unwrap().unwrap();
        assert_eq!(u.version, "0.46.0");
        assert_eq!(u.apk_url, format!("{DOWNLOAD_PREFIX}v0.46.0/Jodd_v0.46.0_android-universal.apk"));
        assert_eq!(u.sha256, "83d1325c5e61525f6c108e1421c9f0acc70c8427083b0c0d206bb26023b443a5");
        assert_eq!(u.size, 30_426_016);
    }

    #[test]
    fn the_same_or_an_older_release_is_not_an_update() {
        assert_eq!(pick_update(&release("v0.45.0", vec![apk("0.45.0")]), "0.45.0").unwrap(), None);
        assert_eq!(pick_update(&release("v0.44.0", vec![apk("0.44.0")]), "0.45.0").unwrap(), None);
    }

    #[test]
    fn drafts_and_prereleases_are_never_offered() {
        let mut r = release("v0.46.0", vec![apk("0.46.0")]);
        r.draft = true;
        assert_eq!(pick_update(&r, "0.45.0").unwrap(), None);
        r.draft = false;
        r.prerelease = true;
        assert_eq!(pick_update(&r, "0.45.0").unwrap(), None);
    }

    #[test]
    fn a_release_without_the_apk_is_an_error_not_silence() {
        let err = pick_update(&release("v0.46.0", vec![]), "0.45.0").unwrap_err();
        assert!(err.contains("no Android APK"), "{err}");
        // The asset for another version does not count.
        let err = pick_update(&release("v0.46.0", vec![apk("0.45.0")]), "0.45.0").unwrap_err();
        assert!(err.contains("no Android APK"), "{err}");
    }

    #[test]
    fn an_apk_hosted_anywhere_else_is_refused() {
        let mut a = apk("0.46.0");
        a.browser_download_url = "https://example.com/Jodd_v0.46.0_android-universal.apk".into();
        let err = pick_update(&release("v0.46.0", vec![a]), "0.45.0").unwrap_err();
        assert!(err.contains("download path"), "{err}");
        // Right host, wrong release folder.
        let mut a = apk("0.46.0");
        a.browser_download_url = format!("{DOWNLOAD_PREFIX}v0.45.0/Jodd_v0.46.0_android-universal.apk");
        assert!(pick_update(&release("v0.46.0", vec![a]), "0.45.0").is_err());
    }

    #[test]
    fn an_apk_without_a_usable_sha256_is_refused() {
        for digest in [None, Some("md5:abc".to_string()), Some("sha256:1234".to_string()), Some(format!("sha256:{}", "z".repeat(64)))] {
            let mut a = apk("0.46.0");
            a.digest = digest.clone();
            let err = pick_update(&release("v0.46.0", vec![a]), "0.45.0").unwrap_err();
            assert!(err.contains("SHA-256"), "{digest:?}: {err}");
        }
    }

    #[test]
    fn an_implausible_size_is_refused() {
        for size in [0, MAX_APK_BYTES + 1] {
            let mut a = apk("0.46.0");
            a.size = size;
            assert!(pick_update(&release("v0.46.0", vec![a]), "0.45.0").is_err(), "size {size}");
        }
    }

    #[test]
    fn a_dry_run_tag_is_an_error_rather_than_an_offer() {
        // /releases/latest never returns a pre-release, but a malformed tag
        // must not be read as some version and offered.
        let err = pick_update(&release("v0.46.0-dryrun.1", vec![]), "0.45.0").unwrap_err();
        assert!(err.contains("unexpected release tag"), "{err}");
    }

    #[test]
    fn real_release_json_parses() {
        // Trimmed from GitHub's response for v0.45.0 on 2026-11-10.
        let json = r#"{
          "tag_name": "v0.45.0", "draft": false, "prerelease": false,
          "html_url": "https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.45.0",
          "assets": [
            {"name": "Jodd_0.45.0_aarch64.dmg", "size": 12164405, "digest": "sha256:342beda02237f482a9327a17a8099aa3c123388f87f6bab6ce946057551aca9f",
             "browser_download_url": "https://github.com/BBM-Co-ORG/Jodd-public/releases/download/v0.45.0/Jodd_0.45.0_aarch64.dmg"},
            {"name": "Jodd_v0.45.0_android-universal.apk", "size": 30426016, "digest": "sha256:83d1325c5e61525f6c108e1421c9f0acc70c8427083b0c0d206bb26023b443a5",
             "browser_download_url": "https://github.com/BBM-Co-ORG/Jodd-public/releases/download/v0.45.0/Jodd_v0.45.0_android-universal.apk"}
          ]
        }"#;
        let r: Release = serde_json::from_str(json).unwrap();
        let u = pick_update(&r, "0.44.0").unwrap().unwrap();
        assert_eq!(u.version, "0.45.0");
        assert_eq!(u.sha256, "83d1325c5e61525f6c108e1421c9f0acc70c8427083b0c0d206bb26023b443a5");
    }

    #[test]
    fn spent_downloads_are_pruned_and_newer_ones_kept() {
        let d = tempfile::tempdir().unwrap();
        for f in ["Jodd_v0.44.0.apk", "Jodd_v0.45.0.apk", "Jodd_v0.46.0.apk", "Jodd_v0.46.0.apk.part", "stray.txt"] {
            std::fs::write(d.path().join(f), b"x").unwrap();
        }
        prune_spent_downloads(d.path(), "0.45.0");
        let mut left: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["Jodd_v0.46.0.apk"]);
    }

    #[test]
    fn hashes_compare_as_lowercase_hex() {
        use sha2::{Digest, Sha256};
        // Same form pick_update stores, so the comparison cannot fail on case.
        assert_eq!(
            to_hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pruning_a_missing_folder_is_harmless() {
        prune_spent_downloads(std::path::Path::new("/nonexistent/jodd-update-test"), "0.45.0");
    }

}
