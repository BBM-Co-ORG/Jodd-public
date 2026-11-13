//! The user's linked TVs ("What's on my TV", spec 2026-11-02 §5.2).
//!
//! Everything that is not secret lives in a flag file beside the app config,
//! so the Settings list renders without a keychain read (gotcha #15). Each TV's
//! screen ID — the thing that lets anyone control it — lives in the OS
//! keychain, one entry per TV, and is read only when the user presses
//! "What's on my TV". The file holds `screen_hash` instead, so "this TV is
//! already linked" is answered without touching the keychain.
//!
//! Ordering is the crash-safety rule: on link the keychain is written before
//! the file, on unpair the file before the keychain, so a crash leaves at worst
//! an orphaned keychain entry (unreachable: no row names its `tv_id`), never a
//! row without a secret.
//!
//! The screen ID never lands in the file, a log line, a `Debug` print or an
//! error string from this module.

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ingest::tv_lounge::Pairing;
use crate::log;

pub const MAX_TVS: usize = 5;

const KC_SERVICE: &str = "jodd";
const FILE_NAME: &str = "app_tv_links.json";
const CORRUPT_NAME: &str = "app_tv_links.json.corrupt";

/// Where each TV's screen ID is kept. The real one is the OS keychain; tests
/// use an in-memory fake.
pub trait TvSecrets: Send + Sync {
    fn get(&self, tv_id: &str) -> Result<Option<String>, String>;
    fn set(&self, tv_id: &str, screen_id: &str) -> Result<(), String>;
    /// A missing entry is `Ok`.
    fn delete(&self, tv_id: &str) -> Result<(), String>;
}

pub struct KeychainSecrets;

fn kc_key(tv_id: &str) -> String {
    format!("tv_screen::{tv_id}")
}

impl TvSecrets for KeychainSecrets {
    fn get(&self, tv_id: &str) -> Result<Option<String>, String> {
        let key = kc_key(tv_id);
        log!("keychain READ   {}/{}", KC_SERVICE, key);
        let entry = keyring_core::Entry::new(KC_SERVICE, &key).map_err(|_| "Could not access the TV key store.".to_string())?;
        match entry.get_password() {
            Ok(s) => Ok((!s.trim().is_empty()).then_some(s)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(_) => Err("Could not access the saved TV. Access may have been cancelled or denied.".into()),
        }
    }

    fn set(&self, tv_id: &str, screen_id: &str) -> Result<(), String> {
        keyring_core::Entry::new(KC_SERVICE, &kc_key(tv_id))
            .map_err(|_| "Could not access the TV key store.".to_string())?
            .set_password(screen_id)
            .map_err(|_| "Could not save the TV to the key store.".to_string())
    }

    fn delete(&self, tv_id: &str) -> Result<(), String> {
        let entry = keyring_core::Entry::new(KC_SERVICE, &kc_key(tv_id)).map_err(|_| "Could not access the TV key store.".to_string())?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err("Could not remove the TV from the key store.".into()),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TvHealth {
    Online,
    Unreachable,
    NeedsRepair,
}

/// What the frontend sees of a TV. Deliberately no screen ID, no hash.
#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct TvRow {
    pub tv_id: String,
    pub label: String,
    pub model: Option<String>,
    pub is_default: bool,
    pub needs_repair: bool,
    pub last_status: Option<TvHealth>,
}

#[derive(Debug, PartialEq)]
pub enum LinkError {
    TooMany,
    BelongsToOther { label: String },
    NotFound,
    Store(String),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct StoredTv {
    tv_id: String,
    label: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    screen_name: String,
    screen_hash: String,
    #[serde(default)]
    linked_at: i64,
    #[serde(default)]
    needs_repair: bool,
    #[serde(default)]
    last_checked_at: Option<i64>,
    #[serde(default)]
    last_status: Option<TvHealth>,
}

#[derive(Serialize, Deserialize, Debug)]
struct FileData {
    version: u32,
    #[serde(default)]
    device_id: Option<String>,
    #[serde(default)]
    default_tv: Option<String>,
    #[serde(default)]
    tvs: Vec<StoredTv>,
}

impl Default for FileData {
    fn default() -> Self {
        Self { version: 1, device_id: None, default_tv: None, tvs: Vec::new() }
    }
}

impl FileData {
    /// `default_tv` is `null` exactly when `tvs` is empty, and always names a row.
    fn fix_default(&mut self) {
        let valid = self.default_tv.as_ref().is_some_and(|d| self.tvs.iter().any(|t| &t.tv_id == d));
        if !valid {
            self.default_tv = self.tvs.iter().min_by_key(|t| t.linked_at).map(|t| t.tv_id.clone());
        }
    }

    fn row(&self, t: &StoredTv) -> TvRow {
        TvRow {
            tv_id: t.tv_id.clone(),
            label: t.label.clone(),
            model: t.model.clone(),
            is_default: self.default_tv.as_deref() == Some(t.tv_id.as_str()),
            needs_repair: t.needs_repair,
            last_status: t.last_status,
        }
    }

    fn index_of(&self, tv_id: &str) -> Result<usize, LinkError> {
        self.tvs.iter().position(|t| t.tv_id == tv_id).ok_or(LinkError::NotFound)
    }

    /// `base`, or `base (2)`, `base (3)`… when another row already has it.
    fn unique_label(&self, base: &str, except: Option<&str>) -> String {
        let taken = |l: &str| self.tvs.iter().any(|t| Some(t.tv_id.as_str()) != except && t.label == l);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base} ({n})")).find(|l| !taken(l)).expect("an unbounded range")
    }
}

fn clean(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// First 16 hex characters of SHA-256 of the screen ID: enough to tell two TVs
/// apart, not reversible.
fn screen_hash(screen_id: &str) -> String {
    Sha256::digest(screen_id.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub struct TvLinks {
    dir: PathBuf,
    secrets: Box<dyn TvSecrets>,
    lock: Mutex<()>,
}

impl TvLinks {
    pub fn new(dir: PathBuf, secrets: Box<dyn TvSecrets>) -> Self {
        Self { dir, secrets, lock: Mutex::new(()) }
    }

    /// `<config_base>/jodd` with the real keychain.
    pub fn app() -> Result<Self, String> {
        let base = crate::paths::config_base().ok_or("no config dir on this OS")?;
        let dir = base.join("jodd");
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
        Ok(Self::new(dir, Box::new(KeychainSecrets)))
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// A missing file is simply empty. An unreadable or unparseable one also
    /// counts as empty, but is first copied to `<file>.corrupt` (replacing any
    /// older copy) so the next save cannot destroy the only record of the
    /// user's TVs. Only the path and the error kind are logged, never contents.
    fn load(&self) -> FileData {
        let path = self.path();
        let parsed = match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<FileData>(&text).map_err(|e| format!("{:?}", e.classify())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileData::default()),
            Err(e) => Err(format!("{:?}", e.kind())),
        };
        let mut data = parsed.unwrap_or_else(|kind| {
            let kept = fs::copy(&path, self.dir.join(CORRUPT_NAME)).is_ok();
            log!("tv_link: {} is unreadable ({kind}); starting empty{}", path.display(), if kept { ", original kept as .corrupt" } else { "" });
            FileData::default()
        });
        data.fix_default();
        data
    }

    /// Temp file + rename, so a crash never leaves half a file.
    fn save(&self, data: &FileData) -> Result<(), LinkError> {
        let store = |what: &str, e: std::io::Error| LinkError::Store(format!("The TV list could not be saved ({what}: {e})."));
        fs::create_dir_all(&self.dir).map_err(|e| store("mkdir", e))?;
        let text = serde_json::to_string_pretty(data).map_err(|e| LinkError::Store(format!("The TV list could not be saved ({e}).")))?;
        let tmp = self.dir.join(format!("{FILE_NAME}.tmp"));
        fs::write(&tmp, text).map_err(|e| store("write", e))?;
        fs::rename(&tmp, self.path()).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            store("rename", e)
        })
    }

    /// Reads the file only — never the keychain. Default first, then link order.
    pub fn list(&self) -> Vec<TvRow> {
        let _g = self.guard();
        let data = self.load();
        let mut tvs: Vec<&StoredTv> = data.tvs.iter().collect();
        tvs.sort_by_key(|t| t.linked_at); // stable: ties keep file order
        let (default, rest): (Vec<_>, Vec<_>) = tvs.into_iter().partition(|t| data.default_tv.as_deref() == Some(t.tv_id.as_str()));
        default.into_iter().chain(rest).map(|t| data.row(t)).collect()
    }

    /// One UUID per install, minted on first use and kept.
    pub fn device_id(&self) -> Result<String, String> {
        let _g = self.guard();
        let mut data = self.load();
        if let Some(id) = data.device_id.clone().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        data.device_id = Some(id.clone());
        self.save(&data).map_err(|e| match e {
            LinkError::Store(m) => m,
            other => format!("{other:?}"),
        })?;
        Ok(id)
    }

    /// Adds a TV, or — when the same screen is already linked — refreshes that
    /// row and says so (`true`). The first TV linked becomes the default.
    pub fn link(&self, p: &Pairing, model: Option<&str>) -> Result<(TvRow, bool), LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        let hash = screen_hash(&p.screen_id);
        let model = clean(model);

        if let Some(i) = data.tvs.iter().position(|t| t.screen_hash == hash) {
            // The same TV again. Rewriting the secret is idempotent and heals a
            // row whose keychain entry went missing.
            let tv_id = data.tvs[i].tv_id.clone();
            self.secrets.set(&tv_id, &p.screen_id).map_err(LinkError::Store)?;
            let t = &mut data.tvs[i];
            t.screen_name = p.screen_name.clone();
            if model.is_some() {
                t.model = model;
            }
            t.needs_repair = false;
            // The old verdict was about a screen that did not answer; same as `repair`.
            t.last_status = None;
            t.last_checked_at = None;
            self.save(&data)?;
            return Ok((data.row(&data.tvs[i]), true));
        }

        if data.tvs.len() >= MAX_TVS {
            return Err(LinkError::TooMany);
        }

        let tv_id = uuid::Uuid::new_v4().to_string();
        let base = model
            .clone()
            .or_else(|| clean(Some(&p.screen_name)))
            .unwrap_or_else(|| {
                // The lowest "TV n" no row is using, so an unpair leaves no gap that a later link repeats.
                let n = (1..).find(|n| !data.tvs.iter().any(|t| t.label == format!("TV {n}"))).expect("an unbounded range");
                format!("TV {n}")
            });
        let label = data.unique_label(&base, None);

        // Keychain first: a crash after this leaves an unreachable orphan, never a row without a secret.
        self.secrets.set(&tv_id, &p.screen_id).map_err(LinkError::Store)?;

        data.tvs.push(StoredTv {
            tv_id: tv_id.clone(),
            label,
            model,
            screen_name: p.screen_name.clone(),
            screen_hash: hash,
            linked_at: now_ms(),
            needs_repair: false,
            last_checked_at: None,
            last_status: None,
        });
        if data.device_id.as_deref().is_none_or(str::is_empty) {
            data.device_id = Some(uuid::Uuid::new_v4().to_string());
        }
        data.fix_default();
        if let Err(e) = self.save(&data) {
            let _ = self.secrets.delete(&tv_id); // best effort: don't leave the orphan
            return Err(e);
        }
        let t = data.tvs.last().expect("just pushed");
        Ok((data.row(t), false))
    }

    /// A fresh code for this row: label, default and position stay.
    pub fn repair(&self, tv_id: &str, p: &Pairing, model: Option<&str>) -> Result<TvRow, LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        let i = data.index_of(tv_id)?;
        let hash = screen_hash(&p.screen_id);
        if let Some(other) = data.tvs.iter().find(|t| t.tv_id != tv_id && t.screen_hash == hash) {
            return Err(LinkError::BelongsToOther { label: other.label.clone() });
        }
        self.secrets.set(tv_id, &p.screen_id).map_err(LinkError::Store)?;
        let t = &mut data.tvs[i];
        t.screen_hash = hash;
        t.screen_name = p.screen_name.clone();
        if let Some(m) = clean(model) {
            t.model = Some(m);
        }
        t.needs_repair = false;
        // The old verdict was about the old screen.
        t.last_status = None;
        t.last_checked_at = None;
        self.save(&data)?;
        Ok(data.row(&data.tvs[i]))
    }

    pub fn rename(&self, tv_id: &str, label: &str) -> Result<TvRow, LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        let i = data.index_of(tv_id)?;
        let wanted = label.trim();
        if wanted.is_empty() {
            return Err(LinkError::Store("The name must not be blank.".into()));
        }
        data.tvs[i].label = data.unique_label(wanted, Some(tv_id));
        self.save(&data)?;
        Ok(data.row(&data.tvs[i]))
    }

    pub fn set_default(&self, tv_id: &str) -> Result<(), LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        data.index_of(tv_id)?;
        data.default_tv = Some(tv_id.to_string());
        self.save(&data)
    }

    /// Forgets the TV on this device. The row goes first, then the secret: once
    /// the file is saved, a failed delete only leaves an unreachable orphan
    /// (no row names its `tv_id`), so it is logged and the unpair succeeds.
    pub fn unpair(&self, tv_id: &str) -> Result<(), LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        let i = data.index_of(tv_id)?;
        data.tvs.remove(i);
        if data.default_tv.as_deref() == Some(tv_id) {
            data.default_tv = None; // fix_default promotes the oldest remaining row
        }
        data.fix_default();
        self.save(&data)?;
        if let Err(e) = self.secrets.delete(tv_id) {
            log!("tv_link: unpaired {tv_id}, but its keychain entry could not be removed: {e}");
        }
        Ok(())
    }

    /// Records a check's outcome. `NeedsRepair` sets the flag, `Online` clears
    /// it, `Unreachable` leaves it; the status and time are always set. A model
    /// updates the model text, never the user's label.
    pub fn mark(&self, tv_id: &str, health: TvHealth, model: Option<&str>) -> Result<TvRow, LinkError> {
        let _g = self.guard();
        let mut data = self.load();
        let i = data.index_of(tv_id)?;
        let t = &mut data.tvs[i];
        match health {
            TvHealth::NeedsRepair => t.needs_repair = true,
            TvHealth::Online => t.needs_repair = false,
            TvHealth::Unreachable => {}
        }
        t.last_status = Some(health);
        t.last_checked_at = Some(now_ms());
        if let Some(m) = clean(model) {
            t.model = Some(m);
        }
        self.save(&data)?;
        Ok(data.row(&data.tvs[i]))
    }

    /// The screen ID, from the keychain — only inside a user-pressed command,
    /// off the async runtime. `Ok(None)` = the secret is missing (the caller
    /// marks the row `NeedsRepair`).
    pub fn screen_id(&self, tv_id: &str) -> Result<Option<String>, String> {
        self.secrets.get(tv_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// An in-memory keychain. Cloning shares the storage, so a test can keep a
    /// handle after boxing one into a `TvLinks`.
    #[derive(Clone, Default)]
    struct Mem {
        map: Arc<Mutex<HashMap<String, String>>>,
        panic_get: Arc<AtomicBool>,
        fail_set: Arc<AtomicBool>,
        fail_delete: Arc<AtomicBool>,
    }

    impl TvSecrets for Mem {
        fn get(&self, tv_id: &str) -> Result<Option<String>, String> {
            assert!(!self.panic_get.load(Ordering::SeqCst), "the keychain was read");
            Ok(self.map.lock().unwrap().get(tv_id).cloned())
        }
        fn set(&self, tv_id: &str, screen_id: &str) -> Result<(), String> {
            if self.fail_set.load(Ordering::SeqCst) {
                return Err("keychain locked".into());
            }
            self.map.lock().unwrap().insert(tv_id.into(), screen_id.into());
            Ok(())
        }
        fn delete(&self, tv_id: &str) -> Result<(), String> {
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err("keychain locked".into());
            }
            self.map.lock().unwrap().remove(tv_id);
            Ok(())
        }
    }

    fn setup() -> (tempfile::TempDir, Mem, TvLinks) {
        let dir = tempfile::tempdir().unwrap();
        let mem = Mem::default();
        let links = TvLinks::new(dir.path().to_path_buf(), Box::new(mem.clone()));
        (dir, mem, links)
    }

    fn pairing(screen: &str, name: &str) -> Pairing {
        Pairing { screen_id: screen.into(), screen_name: name.into() }
    }

    fn file_text(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("app_tv_links.json")).unwrap()
    }

    #[test]
    fn list_reads_no_keychain() {
        let (_d, mem, links) = setup();
        links.link(&pairing("screen-aaaa", "YouTube on TV"), Some("Hisense")).unwrap();
        mem.panic_get.store(true, Ordering::SeqCst);
        assert_eq!(links.list().len(), 1);
    }

    #[test]
    fn first_link_is_default() {
        let (_d, _m, links) = setup();
        let (a, already) = links.link(&pairing("s-a", "n"), Some("A")).unwrap();
        assert!(!already);
        assert!(a.is_default);
        let (b, _) = links.link(&pairing("s-b", "n"), Some("B")).unwrap();
        assert!(!b.is_default);
        assert_eq!(links.list().iter().filter(|r| r.is_default).count(), 1);
    }

    #[test]
    fn list_puts_default_first_then_link_order() {
        let (_d, _m, links) = setup();
        links.link(&pairing("s-a", "n"), Some("A")).unwrap();
        links.link(&pairing("s-b", "n"), Some("B")).unwrap();
        let (c, _) = links.link(&pairing("s-c", "n"), Some("C")).unwrap();
        links.set_default(&c.tv_id).unwrap();
        let labels: Vec<_> = links.list().into_iter().map(|r| r.label).collect();
        assert_eq!(labels, ["C", "A", "B"]);
    }

    #[test]
    fn unpairing_default_promotes_oldest() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("A")).unwrap();
        let (b, _) = links.link(&pairing("s-b", "n"), Some("B")).unwrap();
        links.link(&pairing("s-c", "n"), Some("C")).unwrap();
        links.set_default(&b.tv_id).unwrap();
        links.unpair(&b.tv_id).unwrap();
        let rows = links.list();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tv_id, a.tv_id);
        assert!(rows[0].is_default);
        assert_eq!(rows.iter().filter(|r| r.is_default).count(), 1);
    }

    #[test]
    fn unpairing_the_last_tv_leaves_no_default() {
        let (d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("A")).unwrap();
        links.unpair(&a.tv_id).unwrap();
        assert!(links.list().is_empty());
        let v: serde_json::Value = serde_json::from_str(&file_text(&d)).unwrap();
        assert!(v["default_tv"].is_null());
    }

    #[test]
    fn same_screen_twice_is_one_row() {
        let (_d, mem, links) = setup();
        let (first, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        mem.panic_get.store(true, Ordering::SeqCst); // the comparison must use screen_hash only
        let (second, already) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        assert!(already);
        assert_eq!(second.tv_id, first.tv_id);
        assert_eq!(links.list().len(), 1);
    }

    #[test]
    fn relinking_a_full_list_with_a_known_screen_is_not_too_many() {
        let (_d, _m, links) = setup();
        for i in 0..MAX_TVS {
            links.link(&pairing(&format!("s-{i}"), "n"), None).unwrap();
        }
        let (_, already) = links.link(&pairing("s-0", "n"), None).unwrap();
        assert!(already);
    }

    #[test]
    fn sixth_link_is_too_many() {
        let (_d, mem, links) = setup();
        for i in 0..MAX_TVS {
            links.link(&pairing(&format!("s-{i}"), "n"), None).unwrap();
        }
        assert_eq!(links.link(&pairing("s-new", "n"), None).unwrap_err(), LinkError::TooMany);
        assert_eq!(links.list().len(), MAX_TVS);
        assert_eq!(mem.map.lock().unwrap().len(), MAX_TVS, "a refused link leaves no secret behind");
    }

    #[test]
    fn repair_keeps_label_and_default_and_clears_needs_repair() {
        let (_d, mem, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        links.link(&pairing("s-b", "n"), Some("Other")).unwrap();
        links.rename(&a.tv_id, "Living room").unwrap();
        links.mark(&a.tv_id, TvHealth::NeedsRepair, None).unwrap();
        assert!(links.list()[0].needs_repair);

        let fixed = links.repair(&a.tv_id, &pairing("s-a-new", "YouTube on TV"), Some("Hisense A4")).unwrap();
        assert_eq!(fixed.tv_id, a.tv_id);
        assert_eq!(fixed.label, "Living room");
        assert_eq!(fixed.model.as_deref(), Some("Hisense A4"));
        assert!(fixed.is_default);
        assert!(!fixed.needs_repair);
        let rows = links.list();
        assert_eq!(rows[0].tv_id, a.tv_id, "position is kept");
        assert_eq!(mem.map.lock().unwrap().get(&a.tv_id).map(String::as_str), Some("s-a-new"));
        // the new screen is now the one that counts as "already linked"
        let (_, already) = links.link(&pairing("s-a-new", "n"), None).unwrap();
        assert!(already);
    }

    #[test]
    fn repair_with_another_rows_screen_is_belongs_to_other() {
        let (_d, mem, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        let (b, _) = links.link(&pairing("s-b", "n"), Some("Other")).unwrap();
        links.rename(&b.tv_id, "Bedroom").unwrap();
        mem.panic_get.store(true, Ordering::SeqCst);
        let err = links.repair(&a.tv_id, &pairing("s-b", "n"), None).unwrap_err();
        assert_eq!(err, LinkError::BelongsToOther { label: "Bedroom".into() });
        assert_eq!(mem.map.lock().unwrap().get(&a.tv_id).map(String::as_str), Some("s-a"), "nothing was changed");
    }

    #[test]
    fn repair_of_an_unknown_tv_is_not_found() {
        let (_d, _m, links) = setup();
        assert_eq!(links.repair("nope", &pairing("s", "n"), None).unwrap_err(), LinkError::NotFound);
    }

    #[test]
    fn labels_default_to_model_then_screen_name_then_tv_n() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "YouTube on TV"), Some("Hisense HiSmartTV A4")).unwrap();
        assert_eq!(a.label, "Hisense HiSmartTV A4");
        let (b, _) = links.link(&pairing("s-b", "Den TV"), None).unwrap();
        assert_eq!(b.label, "Den TV");
        let (c, _) = links.link(&pairing("s-c", "  "), Some("  ")).unwrap();
        assert_eq!(c.label, "TV 1");
        let (d, _) = links.link(&pairing("s-d", ""), None).unwrap();
        assert_eq!(d.label, "TV 2");
    }

    #[test]
    fn label_clash_gets_suffix() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "YouTube on TV"), Some("Hisense HiSmartTV A4")).unwrap();
        let (b, _) = links.link(&pairing("s-b", "YouTube on TV"), Some("Hisense HiSmartTV A4")).unwrap();
        let (c, _) = links.link(&pairing("s-c", "YouTube on TV"), Some("Hisense HiSmartTV A4")).unwrap();
        assert_eq!(a.label, "Hisense HiSmartTV A4");
        assert_eq!(b.label, "Hisense HiSmartTV A4 (2)");
        assert_eq!(c.label, "Hisense HiSmartTV A4 (3)");
    }

    #[test]
    fn rename_to_an_existing_label_gets_the_suffix_and_to_its_own_does_not() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Living room")).unwrap();
        let (b, _) = links.link(&pairing("s-b", "n"), Some("Other")).unwrap();
        assert_eq!(links.rename(&b.tv_id, "Living room").unwrap().label, "Living room (2)");
        assert_eq!(links.rename(&a.tv_id, " Living room ").unwrap().label, "Living room");
        assert_eq!(links.rename("nope", "x").unwrap_err(), LinkError::NotFound);
        assert!(matches!(links.rename(&a.tv_id, "   ").unwrap_err(), LinkError::Store(_)));
    }

    #[test]
    fn missing_secret_reads_as_needs_repair() {
        let (_d, mem, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        mem.map.lock().unwrap().remove(&a.tv_id); // deleted behind Jodd's back
        mem.panic_get.store(true, Ordering::SeqCst);
        assert_eq!(links.list().len(), 1, "list reads the file only");
        mem.panic_get.store(false, Ordering::SeqCst);
        assert_eq!(links.screen_id(&a.tv_id), Ok(None));
    }

    #[test]
    fn screen_id_reads_the_keychain() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        assert_eq!(links.screen_id(&a.tv_id), Ok(Some("s-a".to_string())));
    }

    #[test]
    fn link_writes_keychain_before_file() {
        let (d, mem, links) = setup();
        mem.fail_set.store(true, Ordering::SeqCst);
        assert!(matches!(links.link(&pairing("s-a", "n"), None).unwrap_err(), LinkError::Store(_)));
        assert!(links.list().is_empty(), "no row without a secret");
        assert!(!d.path().join("app_tv_links.json").exists() || file_text(&d).contains("\"tvs\": []"));
        // and the happy path leaves both
        mem.fail_set.store(false, Ordering::SeqCst);
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        assert!(mem.map.lock().unwrap().contains_key(&a.tv_id));
    }

    #[test]
    fn unpair_writes_file_before_keychain() {
        let (d, mem, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        mem.fail_delete.store(true, Ordering::SeqCst);
        assert_eq!(links.unpair(&a.tv_id), Ok(()), "an orphaned secret is harmless, so a failed delete is not an error");
        assert!(links.list().is_empty(), "the row is gone");
        assert!(!file_text(&d).contains(&a.tv_id), "and the file was saved without it");
        assert!(mem.map.lock().unwrap().contains_key(&a.tv_id), "the orphan is left alone");
        // a delete of a secret that is not there is not an error for the caller
        mem.fail_delete.store(false, Ordering::SeqCst);
        let (b, _) = links.link(&pairing("s-b", "n"), None).unwrap();
        mem.map.lock().unwrap().remove(&b.tv_id);
        links.unpair(&b.tv_id).unwrap();
        assert_eq!(links.unpair(&b.tv_id).unwrap_err(), LinkError::NotFound);
    }

    #[test]
    fn labels_reuse_the_lowest_free_tv_number() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", ""), None).unwrap();
        let (b, _) = links.link(&pairing("s-b", ""), None).unwrap();
        assert_eq!((a.label.as_str(), b.label.as_str()), ("TV 1", "TV 2"));
        links.unpair(&a.tv_id).unwrap();
        let (c, _) = links.link(&pairing("s-c", ""), None).unwrap();
        assert_eq!(c.label, "TV 1");
        let (d, _) = links.link(&pairing("s-d", ""), None).unwrap();
        assert_eq!(d.label, "TV 3");
    }

    #[test]
    fn relinking_a_known_screen_resets_the_old_verdict() {
        let (d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        links.mark(&a.tv_id, TvHealth::NeedsRepair, None).unwrap();
        let (row, already) = links.link(&pairing("s-a", "n"), None).unwrap();
        assert!(already);
        assert!(!row.needs_repair);
        assert_eq!(row.last_status, None);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.path().join("app_tv_links.json")).unwrap()).unwrap();
        assert!(v["tvs"][0]["last_checked_at"].is_null());
    }

    #[test]
    fn marking_an_unpaired_tv_is_not_found() {
        let (d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        links.unpair(&a.tv_id).unwrap();
        let before = file_text(&d);
        assert_eq!(links.mark(&a.tv_id, TvHealth::Online, Some("X")).unwrap_err(), LinkError::NotFound);
        assert_eq!(file_text(&d), before);
    }

    #[test]
    fn mark_follows_the_needs_repair_rules() {
        let (d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        assert_eq!(a.last_status, None);

        let r = links.mark(&a.tv_id, TvHealth::NeedsRepair, None).unwrap();
        assert!(r.needs_repair);
        assert_eq!(r.last_status, Some(TvHealth::NeedsRepair));

        // Unreachable leaves needs_repair as it was (here: true) ...
        let r = links.mark(&a.tv_id, TvHealth::Unreachable, None).unwrap();
        assert!(r.needs_repair);
        assert_eq!(r.last_status, Some(TvHealth::Unreachable));

        // ... Online clears it ...
        let r = links.mark(&a.tv_id, TvHealth::Online, None).unwrap();
        assert!(!r.needs_repair);
        assert_eq!(r.last_status, Some(TvHealth::Online));

        // ... and Unreachable leaves a false one false.
        let r = links.mark(&a.tv_id, TvHealth::Unreachable, None).unwrap();
        assert!(!r.needs_repair);

        let v: serde_json::Value = serde_json::from_str(&file_text(&d)).unwrap();
        assert!(v["tvs"][0]["last_checked_at"].is_i64() || v["tvs"][0]["last_checked_at"].is_u64());
        assert_eq!(v["tvs"][0]["last_status"], "unreachable");
    }

    #[test]
    fn mark_updates_the_model_but_never_the_label() {
        let (_d, _m, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense A4")).unwrap();
        links.rename(&a.tv_id, "Living room").unwrap();
        let r = links.mark(&a.tv_id, TvHealth::Online, Some("Hisense A5")).unwrap();
        assert_eq!(r.model.as_deref(), Some("Hisense A5"));
        assert_eq!(r.label, "Living room");
        let r = links.mark(&a.tv_id, TvHealth::Online, None).unwrap();
        assert_eq!(r.model.as_deref(), Some("Hisense A5"), "None keeps the model");
    }

    #[test]
    fn device_id_is_stable() {
        let (d, mem, links) = setup();
        let id = links.device_id().unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_eq!(links.device_id().unwrap(), id);
        links.link(&pairing("s-a", "n"), None).unwrap();
        assert_eq!(links.device_id().unwrap(), id, "linking keeps it");
        let again = TvLinks::new(d.path().to_path_buf(), Box::new(mem));
        assert_eq!(again.device_id().unwrap(), id);
    }

    #[test]
    fn rows_survive_a_new_instance() {
        let (d, mem, links) = setup();
        let (a, _) = links.link(&pairing("s-a", "n"), Some("Hisense")).unwrap();
        let again = TvLinks::new(d.path().to_path_buf(), Box::new(mem));
        assert_eq!(again.list(), vec![a]);
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_and_a_later_link_leaves_it_alone() {
        let (d, _m, links) = setup();
        let bad = b"{ \"version\": 1, \"tvs\": [ truncated";
        std::fs::write(d.path().join("app_tv_links.json"), bad).unwrap();
        assert!(links.list().is_empty());
        let corrupt = d.path().join("app_tv_links.json.corrupt");
        assert_eq!(std::fs::read(&corrupt).unwrap(), bad);
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        assert!(a.is_default);
        assert_eq!(std::fs::read(&corrupt).unwrap(), bad, "the save did not touch the kept copy");
        assert!(serde_json::from_str::<serde_json::Value>(&file_text(&d)).is_ok(), "the live file is fresh and valid");
    }

    #[test]
    fn a_newer_corrupt_file_replaces_the_older_copy() {
        let (d, _m, links) = setup();
        std::fs::write(d.path().join("app_tv_links.json.corrupt"), "old").unwrap();
        std::fs::write(d.path().join("app_tv_links.json"), "newer garbage").unwrap();
        links.list();
        assert_eq!(std::fs::read_to_string(d.path().join("app_tv_links.json.corrupt")).unwrap(), "newer garbage");
    }

    #[test]
    fn a_missing_file_makes_no_corrupt_copy() {
        let (d, _m, links) = setup();
        assert!(links.list().is_empty());
        links.link(&pairing("s-a", "n"), None).unwrap();
        assert!(!d.path().join("app_tv_links.json.corrupt").exists());
    }

    #[test]
    fn an_unreadable_file_counts_as_empty() {
        let (d, _m, links) = setup();
        std::fs::write(d.path().join("app_tv_links.json"), "{ not json").unwrap();
        assert!(links.list().is_empty());
        let (a, _) = links.link(&pairing("s-a", "n"), None).unwrap();
        assert!(a.is_default);
    }

    #[test]
    fn file_never_contains_screen_id() {
        let (d, mem, links) = setup();
        let secret = "SECRET-SCREEN-ID-0123456789";
        let (a, _) = links.link(&pairing(secret, "YouTube on TV"), Some("Hisense")).unwrap();
        assert!(!file_text(&d).contains(secret));
        let newer = "ANOTHER-SECRET-SCREEN-ID-987";
        links.repair(&a.tv_id, &pairing(newer, "YouTube on TV"), None).unwrap();
        let text = file_text(&d);
        assert!(!text.contains(secret) && !text.contains(newer));
        assert!(text.contains("screen_hash") && text.contains("\"version\": 1"));
        // nor in a Debug print of a row or an error
        assert!(!format!("{:?}", links.list()).contains(newer));
        mem.fail_set.store(true, Ordering::SeqCst);
        let err = links.link(&pairing("X-SCREEN-ID-LEAK", "n"), None).unwrap_err();
        assert!(!format!("{err:?}").contains("X-SCREEN-ID-LEAK"));
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let (d, _m, links) = setup();
        links.link(&pairing("s-a", "n"), None).unwrap();
        let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["app_tv_links.json"]);
    }

    #[test]
    fn a_row_serializes_with_snake_case_names() {
        let row = TvRow { tv_id: "t".into(), label: "L".into(), model: None, is_default: true, needs_repair: false, last_status: Some(TvHealth::NeedsRepair) };
        let v = serde_json::to_value(&row).unwrap();
        assert_eq!(v["last_status"], "needs_repair");
        assert_eq!(v["is_default"], true);
    }
}
