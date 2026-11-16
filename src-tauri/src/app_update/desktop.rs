//! Windows and macOS updates (spec 2026-11-11-app-updater-design.md §5.2).
//!
//! Jodd reads `latest.json` itself and decides from it — the version, the
//! size, and whether there is an installer for how this copy was installed.
//! Only the install goes through tauri-plugin-updater, which gets the same
//! JSON (`restore_update`) and checks the signature against the key in
//! tauri.conf.json, including the version the signature was bound to.
//!
//! Why not the plugin's own `check()`: for a newer version with no entry for
//! this install it returns `Err(TargetsNotFound)` with no version attached, so
//! the "Jodd X is available — Download" fallback could not be built from it.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use super::android::parse_version;
use super::quiesce::{quiesce, INSTALL_QUIESCE};
use super::{
    install_method, release_page_url, AvailableUpdate, InstallMethod, InstallOutcome, Os, UpdateCheck,
    MACOS_SELF_INSTALL,
};

pub(crate) const ENDPOINT: &str = "https://github.com/BBM-Co-ORG/Jodd-public/releases/latest/download/latest.json";
pub(crate) const DOWNLOAD_PREFIX: &str = "https://github.com/BBM-Co-ORG/Jodd-public/releases/download/";

/// The plugin's name for how this copy was installed, mirroring its
/// `installer_for_bundle_type` (updater.rs): `None` when unknown.
pub(crate) fn installer_name(bundle: Option<tauri::utils::config::BundleType>) -> Option<&'static str> {
    use tauri::utils::config::BundleType;
    match bundle? {
        BundleType::Msi => Some("msi"),
        BundleType::Nsis => Some("nsis"),
        // tauri-utils' bundle_type() reports App for every macOS copy,
        // however it was installed (DMG included).
        BundleType::App => Some("app"),
        _ => None,
    }
}

/// Manifest keys to look for, in the plugin's order: `{os}-{arch}-{installer}`,
/// then `{os}-{arch}`. latest.json never carries the bare key, so an install
/// whose kind is unknown finds nothing and is offered the release page.
pub(crate) fn target_keys(os: &str, arch: &str, installer: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(i) = installer {
        keys.push(format!("{os}-{arch}-{i}"));
    }
    keys.push(format!("{os}-{arch}"));
    keys
}

/// This copy's keys, named as the plugin names them (`updater_os`,
/// `updater_arch`). Jodd ships desktop builds for Windows and macOS only.
fn current_target_keys(os: Os) -> Result<Vec<String>, String> {
    let os = match os {
        Os::Windows => "windows",
        Os::MacOs => "darwin",
        Os::Android => return Err("Android updates do not use latest.json".to_string()),
    };
    let arch = if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" };
    Ok(target_keys(os, arch, installer_name(tauri::utils::platform::bundle_type())))
}

fn desktop_os() -> Result<Os, String> {
    Os::current().ok_or_else(|| "in-app updates are only built for Windows and macOS".to_string())
}

/// The id in a minisign public key's comment, after checking it is the id of
/// the key that follows. `pubkey_b64` is the `.pub` file as `tauri signer
/// generate` writes it: base64 of "untrusted comment: …\n<base64 key>".
pub(crate) fn pubkey_id(pubkey_b64: &str) -> Result<String, String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let text = b64.decode(pubkey_b64.trim()).map_err(|e| format!("pubkey is not base64: {e}"))?;
    let text = String::from_utf8(text).map_err(|_| "pubkey is not text".to_string())?;
    let mut lines = text.lines();
    let id = lines
        .next()
        .and_then(|l| l.strip_prefix("untrusted comment: minisign public key: "))
        .ok_or("pubkey has no minisign comment")?
        .trim()
        .to_uppercase();
    let key = b64
        .decode(lines.next().ok_or("pubkey has no key line")?.trim())
        .map_err(|e| format!("pubkey's key line is not base64: {e}"))?;
    // minisign: "Ed" + 8-byte key id (little-endian) + 32-byte Ed25519 key.
    if key.len() != 42 || &key[..2] != b"Ed" {
        return Err("pubkey is not an Ed25519 minisign key".to_string());
    }
    let real = format!("{:016X}", u64::from_le_bytes(key[2..10].try_into().unwrap()));
    if real != id {
        return Err(format!("pubkey's comment says {id}, but the key is {real}"));
    }
    Ok(id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DesktopPick {
    pub version: String,
    pub size: Option<u64>,
    pub has_entry: bool,
    /// Windows: installed from the MSI, so Windows asks for permission.
    pub msi: bool,
}

/// Whether `manifest` offers an update over `current`, and how. Pure, so every
/// case is testable without a network.
pub(crate) fn pick_desktop_update(
    manifest: &serde_json::Value,
    current: &str,
    keys: &[String],
) -> Result<Option<DesktopPick>, String> {
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("latest.json has no version")?;
    let latest = strict_version(version).ok_or_else(|| format!("latest.json has an unreadable version {version:?}"))?;
    let mine = parse_version(current).ok_or_else(|| format!("this copy's version {current:?} is unreadable"))?;
    if latest <= mine {
        return Ok(None);
    }
    let platforms = manifest.get("platforms").and_then(|p| p.as_object());
    let found = keys.iter().find_map(|k| platforms.and_then(|p| p.get(k)).map(|e| (k, e)));
    let Some((key, entry)) = found else {
        return Ok(Some(DesktopPick { version: version.to_string(), size: None, has_entry: false, msi: false }));
    };
    let url = entry
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| format!("latest.json: {key} has no url"))?;
    vet_desktop_update(version, url)?;
    Ok(Some(DesktopPick {
        version: version.to_string(),
        size: entry.get("size").and_then(|s| s.as_u64()),
        has_entry: true,
        msi: key.ends_with("-msi"),
    }))
}

/// `X.Y.Z` and nothing else — no `v`, no suffix. The version goes into the
/// download path, so a loose parse here would let a manifest name a dry run.
fn strict_version(s: &str) -> Option<(u64, u64, u64)> {
    if s.starts_with('v') {
        return None;
    }
    parse_version(s)
}

/// The installer file names release.yml publishes, as
/// scripts/updater-manifest.mjs `assetNameFor` names them.
fn asset_names(version: &str) -> [String; 3] {
    [
        format!("Jodd_{version}_x64-setup.exe"),
        format!("Jodd_{version}_x64_en-US.msi"),
        "Jodd_aarch64.app.tar.gz".to_string(),
    ]
}

/// The download must be one of the three installers at Jodd-public's path for
/// exactly this version. The signature check is the real guarantee; this stops
/// a manifest that points anywhere else.
pub(crate) fn vet_desktop_update(version: &str, download_url: &str) -> Result<(), String> {
    if strict_version(version).is_none() {
        return Err(format!("latest.json has an unreadable version {version:?}"));
    }
    let dir = format!("{DOWNLOAD_PREFIX}v{version}/");
    let ok = download_url
        .strip_prefix(&dir)
        .is_some_and(|name| asset_names(version).iter().any(|n| n == name));
    if ok {
        Ok(())
    } else {
        Err(format!("latest.json points at {download_url}, which is not a Jodd {version} installer on Jodd-public"))
    }
}

/// A failed fetch of latest.json, in words.
pub(crate) fn fetch_status_message(status: u16) -> String {
    match status {
        // A release without latest.json — every release before this feature.
        404 => "The latest release has no update information yet.".to_string(),
        403 | 429 => format!("GitHub is limiting requests right now (HTTP {status}). Try again later."),
        _ => format!("Couldn't read the update information (HTTP {status})."),
    }
}

// ─── Network and install ────────────────────────────────────────────────────

async fn fetch_manifest(current: &str) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(format!("Jodd/{current}"))
        .build()
        .map_err(|e| format!("couldn't set up the update check: {e}"))?;
    let resp = client
        .get(ENDPOINT)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach GitHub to check for updates: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(fetch_status_message(status.as_u16()));
    }
    resp.json().await.map_err(|e| format!("latest.json is not readable: {e}"))
}

fn available(os: Os, p: DesktopPick) -> AvailableUpdate {
    AvailableUpdate {
        release_url: release_page_url(&p.version),
        method: install_method(os, MACOS_SELF_INSTALL, p.has_entry),
        version: p.version,
        size: p.size,
        msi: p.msi,
    }
}

pub(crate) async fn check(app: &tauri::AppHandle) -> Result<UpdateCheck, String> {
    let os = desktop_os()?;
    let current = app.package_info().version.to_string();
    let manifest = fetch_manifest(&current).await?;
    let pick = pick_desktop_update(&manifest, &current, &current_target_keys(os)?)?;
    Ok(UpdateCheck { current, available: pick.map(|p| available(os, p)) })
}

/// One install at a time: a second click while the download runs would start
/// a second installer.
static INSTALLING: AtomicBool = AtomicBool::new(false);

struct InstallingGuard;
impl Drop for InstallingGuard {
    fn drop(&mut self) {
        INSTALLING.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone, Serialize)]
struct Progress {
    received: u64,
    total: u64,
}

#[derive(Clone, Serialize)]
struct Restarting {
    version: String,
}

/// A verified download waiting for its install (spec §6): the frontend locks
/// and saves the editors between `download` and `install`, and a refused
/// install ("still sending changes") retries without downloading again.
struct Downloaded {
    version: String,
    update: tauri_plugin_updater::Update,
    bytes: Vec<u8>,
}

static DOWNLOADED: std::sync::Mutex<Option<Downloaded>> = std::sync::Mutex::new(None);

fn take_downloaded(version: &str) -> Option<Downloaded> {
    DOWNLOADED.lock().unwrap().take().filter(|d| d.version == version)
}

/// Fetch latest.json, confirm it still offers `version` to this install, and
/// download and verify the installer. Callers hold `INSTALLING`.
async fn fetch_and_download(app: &tauri::AppHandle, os: Os, version: &str) -> Result<Downloaded, String> {
    let current = app.package_info().version.to_string();
    let manifest = fetch_manifest(&current).await?;
    let pick = pick_desktop_update(&manifest, &current, &current_target_keys(os)?)?
        .ok_or_else(|| format!("Jodd {current} is already the latest version"))?;
    if pick.version != version {
        return Err(format!("the latest release is now {}, not {version}; check again", pick.version));
    }
    match install_method(os, MACOS_SELF_INSTALL, pick.has_entry) {
        InstallMethod::DesktopInstaller => {}
        _ if os == Os::MacOs => return Err("macOS updates install from the release page".to_string()),
        _ => return Err(format!("this copy of Jodd can't update itself; download Jodd {version} from the release page")),
    }

    // The same JSON goes to the plugin, which keeps every check of its own:
    // the signature against tauri.conf.json's key, the signed version, HTTPS.
    let update = app
        .updater_builder()
        .header("User-Agent", format!("Jodd/{current}"))
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?
        .restore_update(manifest)
        .map_err(|e| format!("latest.json was refused: {e}"))?
        .ok_or_else(|| format!("Jodd {current} is already the latest version"))?;
    vet_desktop_update(&update.version, update.download_url.as_str())?;

    let mut received: u64 = 0;
    let mut last_sent: u64 = 0;
    let fallback_total = pick.size.unwrap_or(0);
    let bytes = update
        .download(
            |chunk, len| {
                received += chunk as u64;
                let total = len.unwrap_or(fallback_total);
                // Every 256 KiB, not every chunk: a 12 MB installer is ~1 500 chunks.
                if received - last_sent >= 256 * 1024 || received == total {
                    last_sent = received;
                    let _ = app.emit("app-update-progress", Progress { received, total });
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("The download failed: {e}"))?;
    Ok(Downloaded { version: version.to_string(), update, bytes })
}

fn begin() -> Result<InstallingGuard, String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("an update is already downloading".to_string());
    }
    Ok(InstallingGuard)
}

/// Step 1: download and verify, while the user can still type. Jodd keeps
/// running; nothing is installed.
pub(crate) async fn download(app: &tauri::AppHandle, version: &str) -> Result<(), String> {
    let os = desktop_os()?;
    let _installing = begin()?;
    if DOWNLOADED.lock().unwrap().as_ref().is_some_and(|d| d.version == version) {
        return Ok(());
    }
    let d = fetch_and_download(app, os, version).await?;
    *DOWNLOADED.lock().unwrap() = Some(d);
    Ok(())
}

/// Step 2, after the frontend has locked and saved every editor: stop sync,
/// then run the installer, which on Windows exits the process.
pub(crate) async fn install(app: &tauri::AppHandle, version: &str) -> Result<InstallOutcome, String> {
    let os = desktop_os()?;
    let _installing = begin()?;
    let d = match take_downloaded(version) {
        Some(d) => d,
        // Not downloaded first (an older frontend, or a different version):
        // do it now. The editor lock still came first.
        None => fetch_and_download(app, os, version).await?,
    };

    // Keep the lanes held until the process ends (spec §6).
    let state = app.state::<crate::AppState>();
    let ids: Vec<String> = state.accounts.lock().unwrap().iter().map(|a| a.id.clone()).collect();
    let _lanes = match quiesce(&state.sync_schedule, &ids, INSTALL_QUIESCE).await {
        Ok(lanes) => lanes,
        Err(e) => {
            // Keep the verified file: Try again should not download it again.
            *DOWNLOADED.lock().unwrap() = Some(d);
            return Err(e);
        }
    };

    // Windows: install() starts the installer and exits the process, so this
    // event is the only word the frontend gets.
    let _ = app.emit("app-update-restarting", Restarting { version: version.to_string() });
    d.update.install(&d.bytes).map_err(|e| format!("The update couldn't be installed: {e}"))?;
    Ok(InstallOutcome::Restarting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest(version: &str) -> serde_json::Value {
        let url = |name: &str| format!("{DOWNLOAD_PREFIX}v{version}/{name}");
        json!({
            "version": version,
            "notes": "Notes.",
            "pub_date": "2026-11-12T00:00:00Z",
            "platforms": {
                "windows-x86_64-nsis": { "url": url(&format!("Jodd_{version}_x64-setup.exe")), "signature": "sig", "size": 12_000_000 },
                "windows-x86_64-msi": { "url": url(&format!("Jodd_{version}_x64_en-US.msi")), "signature": "sig", "size": 13_000_000 },
                "darwin-aarch64-app": { "url": url("Jodd_aarch64.app.tar.gz"), "signature": "sig" }
            }
        })
    }
    fn keys(installer: Option<&str>) -> Vec<String> {
        target_keys("windows", "x86_64", installer)
    }

    #[test]
    fn a_newer_manifest_with_this_installer_is_an_update() {
        let p = pick_desktop_update(&manifest("0.47.0"), "0.46.0", &keys(Some("nsis"))).unwrap().unwrap();
        assert_eq!(p, DesktopPick { version: "0.47.0".into(), size: Some(12_000_000), has_entry: true, msi: false });
        let p = pick_desktop_update(&manifest("0.47.0"), "0.46.0", &keys(Some("msi"))).unwrap().unwrap();
        assert!(p.msi && p.has_entry);
        assert_eq!(p.size, Some(13_000_000));
    }

    #[test]
    fn no_entry_for_this_install_still_reports_the_version() {
        // The release-page fallback (spec §3): an install whose kind is unknown.
        let p = pick_desktop_update(&manifest("0.47.0"), "0.46.0", &keys(None)).unwrap().unwrap();
        assert_eq!(p, DesktopPick { version: "0.47.0".into(), size: None, has_entry: false, msi: false });
    }

    #[test]
    fn same_or_older_is_no_update() {
        assert_eq!(pick_desktop_update(&manifest("0.47.0"), "0.47.0", &keys(Some("nsis"))).unwrap(), None);
        // A copy newer than the latest release (a dev build) is never offered a downgrade.
        assert_eq!(pick_desktop_update(&manifest("0.47.0"), "0.48.0", &keys(Some("nsis"))).unwrap(), None);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(pick_desktop_update(&manifest("0.10.0"), "0.9.9", &keys(Some("nsis"))).unwrap().is_some());
    }

    #[test]
    fn malformed_version_is_an_error() {
        for v in ["0.47", "v0.47.0-dryrun.3", ""] {
            let mut m = manifest("0.47.0");
            m["version"] = json!(v);
            let err = pick_desktop_update(&m, "0.46.0", &keys(Some("nsis"))).unwrap_err();
            assert!(err.contains("latest.json"), "{v:?}: {err}");
        }
        assert!(pick_desktop_update(&json!({"platforms": {}}), "0.46.0", &keys(Some("nsis"))).is_err());
    }

    #[test]
    fn missing_size_is_none() {
        let p = pick_desktop_update(&manifest("0.47.0"), "0.46.0", &target_keys("darwin", "aarch64", Some("app")))
            .unwrap()
            .unwrap();
        assert!(p.has_entry);
        assert_eq!(p.size, None);
    }

    #[test]
    fn an_entry_pointing_elsewhere_is_refused() {
        let mut m = manifest("0.47.0");
        m["platforms"]["windows-x86_64-nsis"]["url"] = json!("https://example.com/Jodd_0.47.0_x64-setup.exe");
        assert!(pick_desktop_update(&m, "0.46.0", &keys(Some("nsis"))).is_err());
    }

    #[test]
    fn vet_accepts_only_jodd_public_paths_for_that_version() {
        let ok = |name: &str| format!("{DOWNLOAD_PREFIX}v0.47.0/{name}");
        for name in ["Jodd_0.47.0_x64-setup.exe", "Jodd_0.47.0_x64_en-US.msi", "Jodd_aarch64.app.tar.gz"] {
            assert!(vet_desktop_update("0.47.0", &ok(name)).is_ok(), "{name}");
        }
        for bad in [
            "https://example.com/Jodd_0.47.0_x64-setup.exe".to_string(),
            format!("{DOWNLOAD_PREFIX}v0.46.0/Jodd_0.47.0_x64-setup.exe"),
            ok("Jodd_0.47.0_x64-setup.exe").replace("https://", "http://"),
            ok("Jodd_0.46.0_x64-setup.exe"),
            ok("evil.exe"),
            format!("{DOWNLOAD_PREFIX}v0.47.0-dryrun.3/Jodd_0.47.0_x64-setup.exe"),
        ] {
            assert!(vet_desktop_update("0.47.0", &bad).is_err(), "{bad}");
        }
        assert!(vet_desktop_update("0.47", &ok("Jodd_0.47_x64-setup.exe")).is_err());
    }

    #[test]
    fn target_keys_never_fall_back_from_msi_to_nsis() {
        let k = keys(Some("msi"));
        assert_eq!(k, vec!["windows-x86_64-msi".to_string(), "windows-x86_64".to_string()]);
        assert!(!k.contains(&"windows-x86_64-nsis".to_string()));
        assert_eq!(target_keys("darwin", "aarch64", Some("app"))[0], "darwin-aarch64-app");
    }

    #[test]
    fn installer_names_match_the_plugin() {
        use tauri::utils::config::BundleType;
        assert_eq!(installer_name(Some(BundleType::Nsis)), Some("nsis"));
        assert_eq!(installer_name(Some(BundleType::Msi)), Some("msi"));
        assert_eq!(installer_name(Some(BundleType::App)), Some("app"));
        assert_eq!(installer_name(None), None);
    }

    #[test]
    fn fetch_status_message_404_is_plain_words() {
        assert!(fetch_status_message(404).contains("no update information yet"));
        assert!(fetch_status_message(500).contains("500"));
    }

    #[test]
    fn updater_config_matches_the_release_workflow() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let u = &conf["plugins"]["updater"];
        assert_eq!(u["endpoints"], json!([ENDPOINT]));
        assert_eq!(u["requireSignedVersion"], true);
        assert_eq!(u["windows"]["installMode"], "passive");
        let key_id = pubkey_id(u["pubkey"].as_str().unwrap()).unwrap();
        let wf = include_str!("../../../.github/workflows/release-sign.yml");
        assert!(wf.contains(&format!("UPDATER_KEY_ID: '{key_id}'")), "pin {key_id} in release-sign.yml");
    }

    #[test]
    fn the_webview_cannot_drive_the_updater() {
        // Spec §5.2: installs start only from Rust, after the editor flush and
        // the quiesce; an `updater:` permission would let the page skip both.
        assert!(!include_str!("../../capabilities/default.json").contains("updater:"));
    }

    #[test]
    fn pubkey_id_checks_the_comment_against_the_key() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let fixture = include_str!("../../../scripts/fixtures/updater/test-only.key.pub");
        assert_eq!(pubkey_id(fixture).unwrap(), "6F0121AD1438D861");
        // The same key with a comment naming another id.
        let text = String::from_utf8(b64.decode(fixture.trim()).unwrap()).unwrap();
        let lied = text.replace("6F0121AD1438D861", "75C50D6EEC227190");
        let err = pubkey_id(&b64.encode(lied)).unwrap_err();
        assert!(err.contains("75C50D6EEC227190") && err.contains("6F0121AD1438D861"), "{err}");
        assert!(pubkey_id("not base64!").is_err());
    }

    #[test]
    fn endpoint_is_latest_never_a_tag() {
        // GitHub's "latest" never resolves to a draft, and dry runs are drafts.
        assert!(ENDPOINT.ends_with("/releases/latest/download/latest.json"));
    }
}
