//! Local Folder vaults on Android (All files access). Pure helpers compile on
//! every target so the host test suite covers them; the plugin wiring below
//! (Task 3) is Android-only. Spec:
//! docs/superpowers/specs/2026-09-29-localfs-android-design.md.

use std::path::{Component, Path, PathBuf};

pub const INTERNAL_ROOT: &str = "/storage/emulated/0";
const EXTERNAL_STORAGE_AUTHORITY: &str = "com.android.externalstorage.documents";

pub const NEEDS_ACCESS: &str = "Without All files access, Jodd can't use a folder that other apps can see.";
pub const WRONG_STORAGE: &str = "Choose a folder on the tablet's internal storage.";
pub const NOT_TOP_LEVEL: &str = "Choose a folder inside internal storage, not its top level.";
pub const NOT_ANDROID_DIR: &str = "Choose a folder outside Android/ — other apps can't see inside it.";

/// `content://com.android.externalstorage.documents/tree/primary%3ASync%2FJodd`
/// → `/storage/emulated/0/Sync/Jodd`. Only the primary internal volume and
/// only a TREE uri; everything else is `WRONG_STORAGE`.
pub fn tree_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let rest = uri
        .strip_prefix("content://")
        .and_then(|r| r.strip_prefix(EXTERNAL_STORAGE_AUTHORITY))
        .and_then(|r| r.strip_prefix("/tree/"))
        .ok_or_else(|| WRONG_STORAGE.to_string())?;
    // The document id is one percent-encoded path segment; decode it as a URI
    // path (so `+` stays a plus — `urlencoding::decode` does not map `+`).
    let doc_id = urlencoding::decode(rest.split('/').next().unwrap_or(""))
        .map_err(|_| WRONG_STORAGE.to_string())?
        .into_owned();
    let (volume, rel) = doc_id.split_once(':').ok_or_else(|| WRONG_STORAGE.to_string())?;
    if volume != "primary" {
        return Err(WRONG_STORAGE.to_string());
    }
    let rel = rel.trim_matches('/');
    let path = if rel.is_empty() { PathBuf::from(INTERNAL_ROOT) } else { Path::new(INTERNAL_ROOT).join(rel) };
    validate_shape(&path)?;
    // Rebuild from components: drops interior `.` and empty segments so equal
    // folders compare equal (duplicate-path check).
    Ok(path.components().collect())
}

/// The trusted check `add_local_account` runs on Android, whatever the UI did.
pub fn validate_android_vault_path(path: &Path, access_granted: bool) -> Result<(), String> {
    if !access_granted {
        return Err(NEEDS_ACCESS.to_string());
    }
    validate_shape(path)
}

/// Every check `add_local_account` makes on Android, in order: access and
/// path shape FIRST, then "is it a directory", then the write probe. Access
/// comes before `is_dir` because without All files access a real shared
/// folder can look missing, and the user must be told to turn access on
/// (`NEEDS_ACCESS`) rather than that the path is "not a directory".
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn check_android_vault(path: &Path, access_granted: bool) -> Result<(), String> {
    validate_android_vault_path(path, access_granted)?;
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }
    probe_writable(path)
}

/// Create and remove a probe file: the folder is really writable by Jodd now,
/// not merely present. Leaves nothing behind, and never touches a file that
/// was already there (create_new; on a name clash use a unique name).
pub fn probe_writable(dir: &Path) -> Result<(), String> {
    let open = |p: &Path| std::fs::OpenOptions::new().write(true).create_new(true).open(p);
    let mut probe = dir.join(".jodd-probe");
    let file = match open(&probe) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            probe = dir.join(format!(".jodd-probe-{}", uuid::Uuid::new_v4()));
            open(&probe)
        }
        r => r,
    };
    file.map_err(|_| "Jodd can't write to this folder.".to_string())?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn validate_shape(path: &Path) -> Result<(), String> {
    if path.as_os_str().to_string_lossy().contains('\0') {
        return Err(WRONG_STORAGE.to_string());
    }
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(WRONG_STORAGE.to_string());
    }
    let rel = path.strip_prefix(INTERNAL_ROOT).map_err(|_| WRONG_STORAGE.to_string())?;
    let mut parts = rel.components();
    match parts.next() {
        None => Err(NOT_TOP_LEVEL.to_string()),
        Some(Component::Normal(first)) if first.to_str().is_some_and(|s| s.eq_ignore_ascii_case("Android")) => Err(NOT_ANDROID_DIR.to_string()),
        Some(Component::Normal(_)) => Ok(()),
        Some(_) => Err(WRONG_STORAGE.to_string()),
    }
}

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::Runtime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderAccess {
    pub granted: bool,
    pub supported: bool,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug, Deserialize)]
struct PickedFolder {
    uri: Option<String>,
}

#[cfg_attr(target_os = "android", allow(dead_code))]
pub(crate) fn desktop_refusal() -> &'static str {
    "Local Folder picking through this command is Android-only"
}

#[cfg(target_os = "android")]
pub(crate) struct StorageHandle<R: Runtime>(pub tauri::plugin::PluginHandle<R>);

/// Registers the Kotlin `StoragePlugin` on Android; a no-op plugin elsewhere.
/// Does no IPC during setup (gotcha #32).
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("jodd-storage")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            {
                use tauri::Manager;
                let handle = _api.register_android_plugin("co.bbmedia.jodd", "StoragePlugin")?;
                _app.manage(StorageHandle(handle));
            }
            Ok(())
        })
        .build()
}

#[cfg(target_os = "android")]
fn handle(app: &tauri::AppHandle) -> Result<tauri::State<'_, StorageHandle<tauri::Wry>>, String> {
    use tauri::Manager;
    app.try_state::<StorageHandle<tauri::Wry>>().ok_or_else(|| "the storage plugin is not registered".to_string())
}

/// Errors are propagated, never folded into `false`: a plugin/JNI failure is
/// not "access is off", and callers must not show the "turn on All files
/// access" message for it.
#[cfg(target_os = "android")]
pub(crate) async fn has_all_files_access(app: &tauri::AppHandle) -> Result<bool, String> {
    let access = handle(app)?
        .0
        .run_mobile_plugin_async::<FolderAccess>("hasAllFilesAccess", ())
        .await
        .map_err(|e| e.to_string())?;
    Ok(access.granted)
}

#[tauri::command]
pub(crate) async fn local_folder_access(app: tauri::AppHandle) -> Result<FolderAccess, String> {
    #[cfg(target_os = "android")]
    return handle(&app)?.0.run_mobile_plugin_async::<FolderAccess>("hasAllFilesAccess", ()).await.map_err(|e| e.to_string());
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(desktop_refusal().to_string())
    }
}

#[tauri::command]
pub(crate) async fn request_local_folder_access(app: tauri::AppHandle) -> Result<FolderAccess, String> {
    #[cfg(target_os = "android")]
    return handle(&app)?.0.run_mobile_plugin_async::<FolderAccess>("requestAllFilesAccess", ()).await.map_err(|e| e.to_string());
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(desktop_refusal().to_string())
    }
}

/// The picked folder as a filesystem path, or `None` when the user cancelled.
#[tauri::command]
pub(crate) async fn pick_local_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    #[cfg(target_os = "android")]
    {
        let picked = handle(&app)?.0.run_mobile_plugin_async::<PickedFolder>("pickFolder", ()).await.map_err(|e| e.to_string())?;
        return match picked.uri {
            None => Ok(None),
            Some(uri) => tree_uri_to_path(&uri).map(|p| Some(p.to_string_lossy().into_owned())),
        };
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Err(desktop_refusal().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn check_android_vault_asks_for_access_before_saying_not_a_directory() {
        // Without All files access a real shared-storage folder can look
        // missing; the user must be told to turn access on, not "not a directory".
        let missing = Path::new("/storage/emulated/0/Sync/DoesNotExist-jodd-test");
        assert_eq!(check_android_vault(missing, false).unwrap_err(), NEEDS_ACCESS);
        assert_eq!(
            check_android_vault(missing, true).unwrap_err(),
            format!("not a directory: {}", missing.display())
        );
    }

    #[test]
    fn probe_writable_accepts_a_writable_dir_and_leaves_nothing() {
        let d = tempfile::tempdir().unwrap();
        probe_writable(d.path()).unwrap();
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn probe_writable_never_clobbers_an_existing_probe_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".jodd-probe"), b"mine").unwrap();
        probe_writable(d.path()).unwrap();
        assert_eq!(std::fs::read(d.path().join(".jodd-probe")).unwrap(), b"mine");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn probe_writable_refuses_a_read_only_dir() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = probe_writable(d.path());
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        if std::fs::write(d.path().join("x"), b"").is_ok() { return; } // running as root: chmod does not bite
        assert_eq!(r.unwrap_err(), "Jodd can't write to this folder.");
    }

    const EXT: &str = "content://com.android.externalstorage.documents/tree/";

    #[test]
    fn a_primary_tree_becomes_an_internal_storage_path() {
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync")).unwrap(), PathBuf::from("/storage/emulated/0/Sync"));
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync%2FJodd")).unwrap(), PathBuf::from("/storage/emulated/0/Sync/Jodd"));
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync%2FJodd%2F")).unwrap(), PathBuf::from("/storage/emulated/0/Sync/Jodd"));
    }

    /// Review Focus #2: URI path encoding — `+` is a plus, not a space.
    #[test]
    fn names_with_spaces_thai_plus_and_percent_survive() {
        let uri = format!("{EXT}primary%3ANotes%20%E0%B9%84%E0%B8%97%E0%B8%A2%2Fa%2Bb%2F100%25");
        assert_eq!(tree_uri_to_path(&uri).unwrap(), PathBuf::from("/storage/emulated/0/Notes ไทย/a+b/100%"));
    }

    #[test]
    fn other_volumes_and_providers_are_refused_with_the_internal_storage_message() {
        for uri in [
            format!("{EXT}1234-5678%3ANotes"),
            "content://com.android.providers.downloads.documents/tree/downloads".to_string(),
            "content://com.google.android.apps.docs.storage/tree/abc".to_string(),
            format!("content://com.android.externalstorage.documents/document/primary%3ASync"),
            "not a uri".to_string(),
            "".to_string(),
        ] {
            assert_eq!(tree_uri_to_path(&uri).unwrap_err(), WRONG_STORAGE, "{uri}");
        }
    }

    /// Review Focus #1: the storage root itself is not a vault.
    #[test]
    fn the_storage_root_is_refused() {
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3A")).unwrap_err(), NOT_TOP_LEVEL);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0"), true).unwrap_err(), NOT_TOP_LEVEL);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0/"), true).unwrap_err(), NOT_TOP_LEVEL);
    }

    /// Review Focus #3: Android/ is invisible to other apps on Android 11+.
    #[test]
    fn folders_under_android_are_refused() {
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3AAndroid%2Fdata%2Fx")).unwrap_err(), NOT_ANDROID_DIR);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0/Android/media/x"), true).unwrap_err(), NOT_ANDROID_DIR);
        // A folder merely NAMED like it elsewhere is fine.
        assert!(validate_android_vault_path(Path::new("/storage/emulated/0/Sync/Android"), true).is_ok());
    }

    #[test]
    fn validation_needs_access_and_the_internal_prefix() {
        let ok = Path::new("/storage/emulated/0/Sync/Jodd");
        assert!(validate_android_vault_path(ok, true).is_ok());
        assert_eq!(validate_android_vault_path(ok, false).unwrap_err(), NEEDS_ACCESS);
        assert_eq!(validate_android_vault_path(Path::new("/data/data/co.bbmedia.jodd/files"), true).unwrap_err(), WRONG_STORAGE);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0/../0/Sync"), true).unwrap_err(), WRONG_STORAGE);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0/Sync/../../1"), true).unwrap_err(), WRONG_STORAGE);
        assert_eq!(validate_android_vault_path(Path::new("relative/Sync"), true).unwrap_err(), WRONG_STORAGE);
    }

    #[test]
    fn android_dir_refusal_is_case_insensitive() {
        for p in ["android/data", "ANDROID/media/x", "Android/obb"] {
            let enc = p.replace('/', "%2F");
            assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3A{enc}")).unwrap_err(), NOT_ANDROID_DIR, "{p}");
            assert_eq!(validate_android_vault_path(&Path::new(INTERNAL_ROOT).join(p), true).unwrap_err(), NOT_ANDROID_DIR, "{p}");
        }
    }

    #[test]
    fn nul_and_invalid_utf8_are_refused() {
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync%00x")).unwrap_err(), WRONG_STORAGE);
        assert_eq!(validate_android_vault_path(Path::new("/storage/emulated/0/Sync\0x"), true).unwrap_err(), WRONG_STORAGE);
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3A%FF")).unwrap_err(), WRONG_STORAGE);
    }

    #[test]
    fn the_returned_path_is_normalised() {
        let want = PathBuf::from("/storage/emulated/0/Sync/x");
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync%2F.%2Fx")).unwrap(), want);
        assert_eq!(tree_uri_to_path(&format!("{EXT}primary%3ASync%2F%2Fx")).unwrap(), want);
    }

    #[test]
    fn a_trailing_document_part_is_ignored() {
        assert_eq!(
            tree_uri_to_path(&format!("{EXT}primary%3ASync/document/primary%3ASync%2FY")).unwrap(),
            PathBuf::from("/storage/emulated/0/Sync")
        );
    }

    /// The commands exist on every target (ipc_contract needs them registered)
    /// but only do anything on Android.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn the_commands_refuse_on_desktop() {
        assert_eq!(super::desktop_refusal(), "Local Folder picking through this command is Android-only");
    }
}
