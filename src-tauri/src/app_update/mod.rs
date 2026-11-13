//! In-app updates on every platform (spec 2026-11-11-app-updater-design.md).
//!
//! One contract for the frontend — `check_app_update`, `install_app_update`,
//! `request_install_permission` — and one place that decides how an update is
//! installed, `install_method`. Each platform has its own way to do it:
//!
//! - **Android** (`android.rs`): Jodd-public's latest release through GitHub's
//!   API. Only says an update exists while `ANDROID_SELF_INSTALL` is off; on,
//!   the APK is checked against GitHub's SHA-256 and handed to Android's
//!   Package installer.
//! - **Windows** (`desktop.rs`): `latest.json` from the latest release, the
//!   installer verified against Jodd's updater key by tauri-plugin-updater,
//!   which runs it and exits.
//! - **macOS** (`desktop.rs`): the same `latest.json`, but only to say an
//!   update exists; the release page installs it. See `MACOS_SELF_INSTALL`.

use serde::Serialize;

pub(crate) mod android;
#[cfg(not(target_os = "android"))]
pub(crate) mod desktop;
#[cfg(not(target_os = "android"))]
pub(crate) mod quiesce;

pub use android::init;

/// Off until macOS builds are signed with a Developer ID: an ad-hoc build
/// that replaces itself is a new app to the Keychain, and the refresh-token
/// prompt it triggers cost the Gmail account on 2026-10-26 (gotcha #46).
/// Turning it on is spec §9 — this switch plus desktop signing, nothing else.
pub(crate) const MACOS_SELF_INSTALL: bool = false;

/// Off since Google Play Protect blocked v0.46.0 as a "harmful app" on the
/// S23 FE and the Tab S7 (2026-11-11). Same signer as 0.45.0; the only new
/// permission was `REQUEST_INSTALL_PACKAGES`, which installing needs. With it
/// off, Android only says an update exists and opens the release page, as
/// macOS does, and the manifest does not declare the permission (a test pins
/// the two together).
pub(crate) const ANDROID_SELF_INSTALL: bool = false;

/// Where releases are published (since v0.39.0).
pub(crate) const RELEASES_BASE: &str = "https://github.com/BBM-Co-ORG/Jodd-public/releases";

#[cfg_attr(target_os = "android", allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Os {
    Android,
    Windows,
    MacOs,
}

#[cfg_attr(target_os = "android", allow(dead_code))]
impl Os {
    /// The platform this build runs on, or None where Jodd ships no build.
    pub(crate) fn current() -> Option<Os> {
        if cfg!(target_os = "android") {
            Some(Os::Android)
        } else if cfg!(target_os = "windows") {
            Some(Os::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Os::MacOs)
        } else {
            None
        }
    }
}

/// How the update on offer gets installed. The frontend picks its button by
/// this and never guesses the platform for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum InstallMethod {
    /// Android's Package installer, after a SHA-256-checked download.
    AndroidInstaller,
    /// tauri-plugin-updater: signature-checked download, then the installer.
    DesktopInstaller,
    /// Only open the release page in the browser.
    ReleasePage,
}

/// `has_entry`: whether `latest.json` has an installer for how this copy was
/// installed (desktop only; Android does not read it).
#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn install_method(os: Os, macos_self_install: bool, has_entry: bool) -> InstallMethod {
    match os {
        Os::Android => InstallMethod::AndroidInstaller,
        Os::Windows if has_entry => InstallMethod::DesktopInstaller,
        Os::MacOs if macos_self_install && has_entry => InstallMethod::DesktopInstaller,
        Os::Windows | Os::MacOs => InstallMethod::ReleasePage,
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn release_page_url(version: &str) -> String {
    format!("{RELEASES_BASE}/tag/v{version}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    /// Bytes, when the release says.
    pub size: Option<u64>,
    /// The GitHub release page.
    pub release_url: String,
    pub(crate) method: InstallMethod,
    /// Windows, installed from the MSI: Windows asks for permission (UAC).
    pub msi: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current: String,
    pub available: Option<AvailableUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum InstallOutcome {
    /// Android: the system installer is on screen; Android asks the user from here.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    HandedOff,
    /// Android: "Install unknown apps" is off for Jodd and the user did not turn it on.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    NeedsPermission,
    /// Windows: the installer is running and Jodd is exiting. Usually never
    /// seen — the process exits first — so `app-update-restarting` is emitted
    /// before it.
    Restarting,
}

#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn install_permission_refusal() -> &'static str {
    "Install unknown apps is an Android setting; there is nothing to allow here"
}

// ─── Commands ───────────────────────────────────────────────────────────────

/// Is there a newer Jodd? Network only — touches no notes or accounts.
#[tauri::command]
pub(crate) async fn check_app_update(app: tauri::AppHandle) -> Result<UpdateCheck, String> {
    #[cfg(target_os = "android")]
    {
        android::check(&app).await
    }
    #[cfg(not(target_os = "android"))]
    {
        desktop::check(&app).await
    }
}

/// Desktop: download and verify `version` without installing it, so the
/// frontend can lock and save the editors before `install_app_update` closes
/// Jodd. Android downloads inside `install_app_update` and refuses this.
#[tauri::command]
pub(crate) async fn download_app_update(app: tauri::AppHandle, version: String) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let _ = (app, version);
        Err("on Android, install_app_update downloads the update itself".to_string())
    }
    #[cfg(not(target_os = "android"))]
    {
        desktop::download(&app, &version).await
    }
}

/// Install `version`, which is only a confirmation of what the user saw: each
/// platform re-reads the release and refuses if the latest has changed.
#[tauri::command]
pub(crate) async fn install_app_update(app: tauri::AppHandle, version: String) -> Result<InstallOutcome, String> {
    #[cfg(target_os = "android")]
    {
        android::install(&app, &version).await
    }
    #[cfg(not(target_os = "android"))]
    {
        desktop::install(&app, &version).await
    }
}

/// Android's Settings › Install unknown apps. Refuses elsewhere.
#[tauri::command]
pub(crate) async fn request_install_permission(app: tauri::AppHandle) -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        android::request_permission(&app).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(install_permission_refusal().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_self_install_stays_off_until_developer_id() {
        assert!(!MACOS_SELF_INSTALL, "spec §9: flip only together with Developer ID signing");
    }

    /// The switch and the manifest move together: installing needs the
    /// permission, and the permission alone is what Play Protect blocked.
    #[test]
    fn android_self_install_matches_the_manifest_permission() {
        let manifest = include_str!("../../gen/android/app/src/main/AndroidManifest.xml");
        let declared = manifest.contains(r#"<uses-permission android:name="android.permission.REQUEST_INSTALL_PACKAGES""#);
        assert_eq!(declared, ANDROID_SELF_INSTALL, "flip ANDROID_SELF_INSTALL and the manifest permission together");
        assert!(!ANDROID_SELF_INSTALL, "Play Protect blocked v0.46.0 as harmful while it was on");
    }

    /// One download folder, defined by UpdatePlugin.kt and shareable through
    /// the FileProvider. 0.46.0 downloaded into Tauri's cache_dir() (the
    /// external cache on Android) and installed only from activity.cacheDir,
    /// so every install was refused (S23 FE, 2026-11-12).
    #[test]
    fn android_update_folder_is_defined_once_and_shared() {
        let kotlin = include_str!("../../gen/android/app/src/main/java/co/bbmedia/jodd/UpdatePlugin.kt");
        let paths = include_str!("../../gen/android/app/src/main/res/xml/file_paths.xml");
        let rust = include_str!("android.rs");
        assert!(kotlin.contains(r#"File(activity.filesDir, "updates")"#), "UpdatePlugin.kt defines the folder");
        assert!(
            paths.contains(r#"<files-path name="updates" path="updates/" />"#),
            "file_paths.xml must share UpdatePlugin's folder, or the installer can't read the APK"
        );
        assert!(!rust.contains("paths::cache_base"), "android.rs asks Kotlin for the folder (updatesDir), never derives it");
    }

    #[test]
    fn install_method_per_platform() {
        assert_eq!(install_method(Os::Android, false, false), InstallMethod::AndroidInstaller);
        assert_eq!(install_method(Os::Windows, false, true), InstallMethod::DesktopInstaller);
        assert_eq!(install_method(Os::Windows, false, false), InstallMethod::ReleasePage);
        assert_eq!(install_method(Os::MacOs, false, true), InstallMethod::ReleasePage);
        assert_eq!(install_method(Os::MacOs, true, true), InstallMethod::DesktopInstaller);
        assert_eq!(install_method(Os::MacOs, true, false), InstallMethod::ReleasePage);
    }

    #[test]
    fn available_update_serializes_for_the_frontend() {
        let v = serde_json::to_value(AvailableUpdate {
            version: "0.47.0".into(),
            size: None,
            release_url: release_page_url("0.47.0"),
            method: InstallMethod::ReleasePage,
            msi: false,
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({"version":"0.47.0","size":null,
                "releaseUrl":"https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.47.0","method":"releasePage","msi":false})
        );
        assert_eq!(serde_json::to_value(InstallOutcome::Restarting).unwrap(), serde_json::json!({"kind":"restarting"}));
        assert_eq!(serde_json::to_value(InstallOutcome::HandedOff).unwrap(), serde_json::json!({"kind":"handedOff"}));
    }

    #[test]
    fn an_android_update_reaches_the_frontend_without_its_url_or_hash() {
        let a = android::AndroidUpdate {
            version: "0.46.0".into(),
            apk_url: "https://example.invalid/x.apk".into(),
            size: 30,
            sha256: "a".repeat(64),
            release_url: release_page_url("0.46.0"),
        };
        let v = serde_json::to_value(AvailableUpdate::from(a)).unwrap();
        // ANDROID_SELF_INSTALL is off: the release page installs it.
        assert_eq!(v["method"], "releasePage");
        assert_eq!(v["size"], 30);
        assert!(v.get("apkUrl").is_none() && v.get("sha256").is_none());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn request_install_permission_refuses_on_desktop() {
        assert!(install_permission_refusal().contains("Android"));
    }
}
