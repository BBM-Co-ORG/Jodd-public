//! When the settings folder on this device began.
//!
//! On Android an uninstall, an install signed with another key, or Clear
//! storage wipes the app's files, and with them the AI settings, linked TVs
//! and accounts (`allowBackup="false"`, so nothing comes back). Users could not
//! tell that apart from Jodd losing a setting. This file is written once, in
//! the same folder as those settings: if a setting is gone and this date moved
//! forward, the folder was wiped; if the date is older than the setting, the
//! loss is a Jodd bug.
//!
//! Installs that already had settings when this shipped cannot know their real
//! start, so they record `found_existing` and Settings says "or earlier".

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::log;

const FILE_NAME: &str = "data_since.json";

/// Files that only exist once the user has set something up. Their presence
/// means the folder predates this marker.
const USER_FILES: [&str; 3] = ["accounts.json", "app_llm.json", "app_tv_links.json"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DataSince {
    pub since_ms: i64,
    pub found_existing: bool,
}

fn dir() -> Option<std::path::PathBuf> {
    Some(crate::paths::config_base()?.join("jodd"))
}

/// Called once from `.setup()`, right after `paths::init`. Never fails the
/// launch: a diagnostic that cannot be written is only logged.
pub fn ensure() {
    let Some(dir) = dir() else { return };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    match ensure_in(&dir, now) {
        Ok((d, true)) => log!("data_since: started {} (found_existing={})", d.since_ms, d.found_existing),
        Ok((_, false)) => {}
        Err(e) => log!("data_since: could not record ({e})"),
    }
}

/// Returns the marker and whether this call wrote it.
fn ensure_in(dir: &Path, now_ms: i64) -> Result<(DataSince, bool), String> {
    if let Some(d) = read_in(dir) {
        return Ok((d, false));
    }
    fs::create_dir_all(dir).map_err(|e| format!("mkdir: {:?}", e.kind()))?;
    let path = dir.join(FILE_NAME);
    // An unreadable marker is replaced; the folder it sat in is not new.
    let found_existing = path.exists() || USER_FILES.iter().any(|f| dir.join(f).exists());
    let d = DataSince { since_ms: now_ms, found_existing };
    let text = serde_json::to_string(&d).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp"));
    fs::write(&tmp, text).map_err(|e| format!("write: {:?}", e.kind()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename: {:?}", e.kind())
    })?;
    Ok((d, true))
}

pub fn read() -> Option<DataSince> {
    read_in(&dir()?)
}

fn read_in(dir: &Path) -> Option<DataSince> {
    serde_json::from_str(&fs::read_to_string(dir.join(FILE_NAME)).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_folder_records_now_and_is_not_existing() {
        let dir = tempfile::tempdir().unwrap();
        let jodd = dir.path().join("jodd");
        let (d, wrote) = ensure_in(&jodd, 1_000).unwrap();
        assert!(wrote);
        assert_eq!(d, DataSince { since_ms: 1_000, found_existing: false });
        assert_eq!(read_in(&jodd), Some(d));
    }

    #[test]
    fn it_is_written_once_and_never_moves() {
        let dir = tempfile::tempdir().unwrap();
        ensure_in(dir.path(), 1_000).unwrap();
        let (d, wrote) = ensure_in(dir.path(), 9_000).unwrap();
        assert!(!wrote);
        assert_eq!(d.since_ms, 1_000);
    }

    #[test]
    fn a_folder_with_settings_already_in_it_says_so() {
        for f in USER_FILES {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join(f), "{}").unwrap();
            let (d, _) = ensure_in(dir.path(), 5_000).unwrap();
            assert!(d.found_existing, "{f} should count as existing settings");
        }
    }

    #[test]
    fn other_files_do_not_count_as_settings() {
        // The log settings file is written before anything the user chose.
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("app_settings.json"), "{}").unwrap();
        let (d, _) = ensure_in(dir.path(), 5_000).unwrap();
        assert!(!d.found_existing);
    }

    #[test]
    fn an_unreadable_marker_is_replaced_and_marked_existing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(FILE_NAME), "garbage").unwrap();
        let (d, wrote) = ensure_in(dir.path(), 7_000).unwrap();
        assert!(wrote);
        assert_eq!(d, DataSince { since_ms: 7_000, found_existing: true });
    }
}
