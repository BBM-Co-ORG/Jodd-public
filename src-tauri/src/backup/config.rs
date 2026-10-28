//! Where backups go (`backup.json`) and how the last one went
//! (`backup_status.json`). The `_from`/`_to`/`_in` functions take an explicit
//! path so tests never touch the real config or data directories; the
//! wrappers resolve the path under `paths::config_base()` / `data_base()`,
//! in the `jodd` folder beside the other app-level config files.

use super::destination::{Destination, LocalDest};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DestinationConfig {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub kind: DestKind,
}

/// M2 adds `Ssh`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum DestKind {
    Local { path: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BackupConfig {
    #[serde(default)]
    pub destinations: Vec<DestinationConfig>,
}

/// The outcome of the most recent backup of one account to one destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastBackup {
    pub account_id: String,
    pub destination_id: String,
    pub at_ms: i64,
    pub ok: bool,
    pub message: String,
}

pub fn destination_for(cfg: &DestinationConfig) -> Box<dyn Destination> {
    match &cfg.kind {
        DestKind::Local { path } => Box::new(LocalDest { root: PathBuf::from(path) }),
    }
}

/// Write `value` as JSON to `path` through a temp file in the same directory.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let dir = path.parent().ok_or("path has no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let name = path.file_name().ok_or("path has no file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let json = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize: {e}"))?;
    fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename to {}: {e}", path.display())
    })
}

/// Empty when the file is missing or unreadable as JSON. A file that does
/// not parse is renamed to `<name>.bad` (replacing an older one) first, so the
/// next save cannot overwrite what the owner may want back — a newer Jodd's
/// config, or a hand edit gone wrong.
fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            crate::log!("backup: could not read {}: {e}", path.display());
            return T::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            let mut bad = path.as_os_str().to_owned();
            bad.push(".bad");
            let bad = PathBuf::from(bad);
            match fs::rename(path, &bad) {
                Ok(()) => crate::log!("backup: {} is not valid JSON ({e}); kept it as {}", path.display(), bad.display()),
                Err(re) => crate::log!(
                    "backup: {} is not valid JSON ({e}) and could not be set aside: {re}",
                    path.display()
                ),
            }
            T::default()
        }
    }
}

pub fn load_from(path: &Path) -> BackupConfig {
    read_json(path)
}

pub fn save_to(path: &Path, cfg: &BackupConfig) -> Result<(), String> {
    write_json_atomic(path, cfg)
}

pub fn statuses_in(path: &Path) -> Vec<LastBackup> {
    read_json(path)
}

/// Replaces the entry for the same account and destination.
pub fn record_status_in(path: &Path, s: LastBackup) -> Result<(), String> {
    let mut all = statuses_in(path);
    all.retain(|x| !(x.account_id == s.account_id && x.destination_id == s.destination_id));
    all.push(s);
    write_json_atomic(path, &all)
}

fn config_path() -> Option<PathBuf> {
    Some(crate::paths::config_base()?.join("jodd").join("backup.json"))
}

fn status_path() -> Option<PathBuf> {
    Some(crate::paths::data_base()?.join("jodd").join("backup_status.json"))
}

const NO_DIR: &str = "no config dir on this OS";

pub fn load() -> BackupConfig {
    config_path().map(|p| load_from(&p)).unwrap_or_default()
}

pub fn save(cfg: &BackupConfig) -> Result<(), String> {
    save_to(&config_path().ok_or(NO_DIR)?, cfg)
}

pub fn record_status(s: LastBackup) -> Result<(), String> {
    record_status_in(&status_path().ok_or(NO_DIR)?, s)
}

pub fn statuses() -> Vec<LastBackup> {
    status_path().map(|p| statuses_in(&p)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("backup.json");
        assert_eq!(load_from(&p), BackupConfig::default(), "missing file is empty");
        let cfg = BackupConfig {
            destinations: vec![DestinationConfig {
                id: "d1".into(),
                name: "USB".into(),
                kind: DestKind::Local { path: "/Volumes/usb".into() },
            }],
        };
        save_to(&p, &cfg).unwrap();
        assert_eq!(load_from(&p), cfg);
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"kind\": \"local\""), "{raw}");
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temp file left behind");
    }

    #[test]
    fn corrupt_config_loads_empty_and_is_kept_aside() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("backup.json");
        let bad = d.path().join("backup.json.bad");
        std::fs::write(&bad, "an older bad file").unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load_from(&p), BackupConfig::default());
        // The next save cannot destroy what the owner may want back.
        assert!(!p.exists());
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
        save_to(&p, &BackupConfig::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&bad).unwrap(), "{ not json");
    }

    #[test]
    fn a_missing_file_is_a_silent_empty() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load_from(&d.path().join("backup.json")), BackupConfig::default());
        assert!(statuses_in(&d.path().join("backup_status.json")).is_empty());
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0, "nothing set aside");
    }

    #[test]
    fn corrupt_statuses_load_empty_and_are_kept_aside() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("backup_status.json");
        std::fs::write(&p, "[{ truncated").unwrap();
        record_status_in(&p, status("gmail:a@x", "d1", 1, true)).unwrap();
        assert_eq!(statuses_in(&p).len(), 1);
        assert_eq!(std::fs::read_to_string(d.path().join("backup_status.json.bad")).unwrap(), "[{ truncated");
    }

    fn status(acct: &str, dest: &str, at: i64, ok: bool) -> LastBackup {
        LastBackup {
            account_id: acct.into(),
            destination_id: dest.into(),
            at_ms: at,
            ok,
            message: if ok { "ok".into() } else { "disk full".into() },
        }
    }

    #[test]
    fn record_status_replaces_same_account_and_destination() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("backup_status.json");
        assert!(statuses_in(&p).is_empty());
        record_status_in(&p, status("gmail:a@x", "d1", 1, true)).unwrap();
        record_status_in(&p, status("gmail:a@x", "d2", 2, true)).unwrap();
        record_status_in(&p, status("gmail:b@x", "d1", 3, true)).unwrap();
        record_status_in(&p, status("gmail:a@x", "d1", 4, false)).unwrap();
        let all = statuses_in(&p);
        assert_eq!(all.len(), 3);
        let a1: Vec<_> = all.iter().filter(|s| s.account_id == "gmail:a@x" && s.destination_id == "d1").collect();
        assert_eq!(a1.len(), 1);
        assert_eq!(a1[0].at_ms, 4);
        assert!(!a1[0].ok);
    }

    #[test]
    fn destination_for_local_points_at_its_path() {
        let cfg = DestinationConfig {
            id: "d1".into(),
            name: "USB".into(),
            kind: DestKind::Local { path: "/Volumes/usb".into() },
        };
        let _dest: Box<dyn Destination> = destination_for(&cfg);
    }
}
