//! Running one backup: snapshot the account's cache, write a bundle zip to a
//! scratch file, hash it, and publish it to a destination. The scratch file is
//! removed whether or not the publish worked.

use super::bundle::{write_bundle, BundleSource};
use super::destination::{Destination, VersionEntry};
use super::manifest::Manifest;
use super::naming::{account_slug, version_stamp};
use super::Trigger;
use crate::accounts::{backend_prefix, Account};
use crate::db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Error code for an account that has never completed a full listing.
const NOT_SYNCED_YET: &str = "NOT_SYNCED_YET";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupOutcome {
    pub stamp: String,
    pub notes: usize,
    /// Notes whose bundle copy is flagged as losing something (`jodd.partial`).
    pub partial: usize,
    /// Notes the writer could not put in the bundle.
    pub skipped: usize,
    pub bytes: u64,
}

/// One lock per destination and account slug. `sweep_orphans` removes an
/// in-flight `.partial`, so a sweep must never overlap a publish to the same
/// folder; the same lock makes the free-stamp check below race-free.
fn slug_lock(key: String) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut map = LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key).or_default().clone()
}

/// How old a scratch file must be before a sweep may assume its run is over.
const STALE_AFTER: Duration = Duration::from_secs(3600);

/// Remove scratch files a crashed or abandoned run left in `dir`: bundle zips
/// (`<uuid>.zip`, `restore-<stamp>.zip`) and partial downloads (`*.part`)
/// older than an hour, except `keep`, the file the caller is about to use.
/// Best-effort: the disk this guards is often nearly full, but a file that
/// cannot be removed must not stop a backup or a restore.
pub(crate) fn sweep_stale_work_files(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".zip") || name.ends_with(".part")) || keep == Some(path.as_path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok());
        if meta.is_file() && age.is_some_and(|a| a > STALE_AFTER) {
            if let Err(e) = std::fs::remove_file(&path) {
                crate::log!("backup: could not remove the stale scratch file {}: {e}", path.display());
            }
        }
    }
}

/// Back `account` up to `dest`, scratching under `paths::cache_base()/backup`.
pub async fn run_backup(
    db: Arc<Db>,
    account: &Account,
    dest: &dyn Destination,
    trigger: Trigger,
    only: Option<HashSet<String>>,
) -> Result<BackupOutcome, String> {
    let work_dir = crate::paths::cache_base()
        .ok_or_else(|| "no cache directory to stage the backup in".to_string())?
        .join("backup");
    run_backup_in(db, account, dest, trigger, only, &work_dir, crate::db::now_ms()).await
}

/// `run_backup` with the scratch directory and the clock supplied, so tests
/// never touch the owner's real directories.
///
/// The snapshot, the zip and the hash all run on a blocking thread with a
/// cloned `Arc<Db>`, and only after the destination lock is held, so a queued
/// backup holds no snapshot while it waits. Attachment bytes are read one at a
/// time while the zip is written, never all at once.
pub async fn run_backup_in(
    db: Arc<Db>,
    account: &Account,
    dest: &dyn Destination,
    trigger: Trigger,
    only: Option<HashSet<String>>,
    work_dir: &Path,
    now_ms: i64,
) -> Result<BackupOutcome, String> {
    let synced = {
        let (db, id) = (db.clone(), account.id.clone());
        tokio::task::spawn_blocking(move || db.full_listing_at(&id))
            .await
            .map_err(|e| format!("backup task failed: {e}"))?
            .map_err(|e| e.to_string())?
    };
    if synced.is_none() {
        return Err(NOT_SYNCED_YET.to_string());
    }

    let slug = account_slug(&account.id);
    let lock = slug_lock(format!("{}\0{slug}", dest.lock_key()));
    let _serialised = lock.lock().await;

    // Never publish onto an existing stamp: two backups in one second would
    // otherwise overwrite each other's version.
    let taken: HashSet<String> = dest.list_versions(&slug).await?.into_iter().map(|v: VersionEntry| v.stamp).collect();
    let mut created_ms = now_ms;
    while taken.contains(&version_stamp(created_ms)) {
        created_ms += 1000;
    }
    let stamp = version_stamp(created_ms);

    // Housekeeping only: a stray file we cannot remove must not stop a backup.
    if let Err(e) = dest.sweep_orphans(&slug).await {
        crate::log!("backup: could not sweep leftovers for {slug}: {e}");
    }

    sweep_stale_work_files(work_dir, None);
    let tmp = work_dir.join(format!("{}.zip", uuid::Uuid::new_v4()));
    let result = build_and_publish(db, dest, account, trigger, only, created_ms, &slug, &stamp, &tmp).await;
    let _ = std::fs::remove_file(&tmp);
    result
}

#[allow(clippy::too_many_arguments)]
async fn build_and_publish(
    db: Arc<Db>,
    dest: &dyn Destination,
    account: &Account,
    trigger: Trigger,
    only: Option<HashSet<String>>,
    created_ms: i64,
    slug: &str,
    stamp: &str,
    tmp: &Path,
) -> Result<BackupOutcome, String> {
    let account_id = account.id.clone();
    let backend = backend_prefix(account.backend_kind);
    let root_label = account.effective_notes_label().to_string();
    let tmp_owned = tmp.to_path_buf();
    let manifest: Manifest = tokio::task::spawn_blocking(move || -> Result<Manifest, String> {
        if let Some(dir) = tmp_owned.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let read = db.backup_read(&account_id).map_err(|e| e.to_string())?;
        let src = BundleSource {
            account_id: &account_id,
            backend,
            root_label: &root_label,
            trigger,
            created_ms,
            app_version: env!("CARGO_PKG_VERSION"),
            device: std::env::consts::OS,
            read: &read,
            only: only.as_ref(),
        };
        let file = std::fs::File::create(&tmp_owned).map_err(|e| format!("create {}: {e}", tmp_owned.display()))?;
        let mut fetch = |uuid: &str, cid: &str| db.attachment_bytes(&account_id, uuid, cid).map_err(|e| e.to_string());
        let mut manifest = write_bundle(&src, file, &mut fetch)?;
        let mut hasher = Sha256::new();
        let mut finished = std::fs::File::open(&tmp_owned).map_err(|e| format!("open {}: {e}", tmp_owned.display()))?;
        let bytes = std::io::copy(&mut finished, &mut hasher).map_err(|e| format!("hash {}: {e}", tmp_owned.display()))?;
        manifest.zip_bytes = bytes;
        manifest.zip_sha256 = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Ok(manifest)
    })
    .await
    .map_err(|e| format!("backup task failed: {e}"))??;

    dest.publish(slug, stamp, tmp, &manifest).await?;
    Ok(BackupOutcome {
        stamp: stamp.to_string(),
        notes: manifest.notes.len(),
        partial: manifest.notes.iter().filter(|n| n.partial.is_some()).count(),
        skipped: manifest.skipped.len(),
        bytes: manifest.zip_bytes,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::accounts::{Account, AccountStatus, BackendKind, LlmConfig};
    use crate::backup::destination::LocalDest;
    use crate::db::RemotePin;
    use sha2::{Digest, Sha256};

    const ACCT: &str = "gmail:a@x.com";
    const NOW: i64 = 1_792_747_800_000; // 2026-10-23T09-30-00Z

    fn account() -> Account {
        Account {
            id: ACCT.to_string(),
            email: "a@x.com".to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: BackendKind::Gmail,
            root_dir: None,
            ssh_target: None,
            ssh_port: None,
            ssh_key: None,
            icloud_session_established: false,
            blocked_reason: None,
            sync_cursor: None,
            icloud_replica_id: None,
            status: AccountStatus::Active,
            pending_removal: false,
        }
    }

    fn seeded_db(uuids: &[&str]) -> Arc<Db> {
        let db = crate::test_support::temp_db();
        for u in uuids {
            let n = crate::test_support::note(ACCT, u).title(&format!("Note {u}")).build();
            db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
        }
        db.record_full_listing(ACCT).unwrap();
        Arc::new(db)
    }

    fn slug() -> String {
        crate::backup::naming::account_slug(ACCT)
    }

    #[tokio::test]
    async fn backup_refuses_before_first_full_listing() {
        let db = crate::test_support::temp_db();
        let n = crate::test_support::note(ACCT, "u1").build();
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
        let db = Arc::new(db);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };

        let r = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, work.path(), NOW).await;
        assert_eq!(r.unwrap_err(), "NOT_SYNCED_YET");
        assert!(dest.list_slugs().await.unwrap().is_empty());
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn backup_publishes_one_version_with_all_notes() {
        let db = seeded_db(&["u1", "u2", "u3"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };

        let out = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, work.path(), NOW)
            .await
            .unwrap();
        assert_eq!(out.notes, 3);
        assert_eq!(out.partial, 0);
        assert_eq!(out.skipped, 0);
        assert_eq!(out.stamp, crate::backup::naming::version_stamp(NOW));

        let versions = dest.list_versions(&slug()).await.unwrap();
        assert_eq!(versions.len(), 1);
        let v = &versions[0];
        assert_eq!(v.stamp, out.stamp);
        assert_eq!(v.manifest.notes.len(), 3);
        assert_eq!(v.manifest.account_id, ACCT);
        assert_eq!(v.manifest.backend, "gmail");
        assert_eq!(v.manifest.root_label, "Notes");
        assert_eq!(v.manifest.created_ms, NOW);
        assert_eq!(v.manifest.trigger, Trigger::Manual);
        assert_eq!(v.manifest.app_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(v.manifest.device, std::env::consts::OS);

        let fetched = work.path().join("fetched.zip");
        dest.fetch_zip(&slug(), &v.stamp, &fetched).await.unwrap();
        let bytes = std::fs::read(&fetched).unwrap();
        assert_eq!(v.manifest.zip_bytes, bytes.len() as u64);
        assert_eq!(out.bytes, bytes.len() as u64);
        let want: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(v.manifest.zip_sha256, want);

        // The temp zip is gone; only the file fetched above remains.
        let left: Vec<_> = std::fs::read_dir(work.path()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }

    #[tokio::test]
    async fn pre_restore_backup_contains_only_listed_notes() {
        let db = seeded_db(&["u1", "u2", "u3"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };
        let only: HashSet<String> = ["u2".to_string()].into_iter().collect();

        let out = run_backup_in(db.clone(), &account(), &dest, Trigger::PreRestore, Some(only), work.path(), NOW)
            .await
            .unwrap();
        assert_eq!(out.notes, 1);
        let v = &dest.list_versions(&slug()).await.unwrap()[0];
        assert_eq!(v.manifest.trigger, Trigger::PreRestore);
        assert_eq!(v.manifest.notes.len(), 1);
        assert_eq!(v.manifest.notes[0].uuid, "u2");
    }

    /// A destination whose `publish` always fails.
    struct FailingDest;

    #[async_trait::async_trait]
    impl Destination for FailingDest {
        fn lock_key(&self) -> String {
            "failing".into()
        }
        async fn list_slugs(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn list_versions(&self, _: &str) -> Result<Vec<VersionEntry>, String> {
            Ok(vec![])
        }
        async fn publish(&self, _: &str, _: &str, zip: &Path, _: &Manifest) -> Result<(), String> {
            assert!(zip.exists(), "the temp zip exists while publishing");
            Err("disk full".into())
        }
        async fn fetch_zip(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
            Err("n/a".into())
        }
        async fn delete_version(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn sweep_orphans(&self, _: &str) -> Result<usize, String> {
            Ok(0)
        }
        async fn probe(&self) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn temp_file_removed_on_failure() {
        let db = seeded_db(&["u1"]);
        let work = tempfile::tempdir().unwrap();

        let r = run_backup_in(db.clone(), &account(), &FailingDest, Trigger::Manual, None, work.path(), NOW).await;
        assert_eq!(r.unwrap_err(), "disk full");
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn back_to_back_backups_with_the_same_clock_are_distinct_versions() {
        let db = seeded_db(&["u1"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };

        let a = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, work.path(), NOW).await.unwrap();
        let b = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, work.path(), NOW).await.unwrap();
        assert_ne!(a.stamp, b.stamp);
        assert_eq!(b.stamp, crate::backup::naming::version_stamp(NOW + 1000));

        let versions = dest.list_versions(&slug()).await.unwrap();
        assert_eq!(versions.len(), 2);
        // The first version is untouched: its manifest still says NOW.
        let first = versions.iter().find(|v| v.stamp == a.stamp).unwrap();
        assert_eq!(first.manifest.created_ms, NOW);
        let second = versions.iter().find(|v| v.stamp == b.stamp).unwrap();
        assert_eq!(second.manifest.created_ms, NOW + 1000);
    }

    #[tokio::test]
    async fn concurrent_backups_to_one_account_both_publish() {
        let db = seeded_db(&["u1"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };
        let acct = account();

        let (a, b) = tokio::join!(
            run_backup_in(db.clone(), &acct, &dest, Trigger::Manual, None, work.path(), NOW),
            run_backup_in(db.clone(), &acct, &dest, Trigger::Scheduled, None, work.path(), NOW),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a.stamp, b.stamp);
        assert_eq!(dest.list_versions(&slug()).await.unwrap().len(), 2);
        // Neither run's sweep removed the other's in-flight file.
        let dir = root.path().join("jodd-backups").join(slug());
        let names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names.len(), 4, "{names:?}");
    }

    #[tokio::test]
    async fn attachment_bytes_reach_the_bundle() {
        let db = seeded_db(&["u1"]);
        db.upsert_attachment(
            ACCT,
            "u1",
            &crate::backend::gmail::wire::Attachment {
                content_id: "cid1".into(),
                mime_type: "image/png".into(),
                filename: Some("a.png".into()),
                x_apple_part_url: None,
                data: vec![7, 8, 9],
            },
        )
        .unwrap();
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = LocalDest { root: root.path().to_path_buf() };

        let out = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, work.path(), NOW)
            .await
            .unwrap();
        assert_eq!(out.skipped, 0);
        let fetched = work.path().join("fetched.zip");
        dest.fetch_zip(&slug(), &out.stamp, &fetched).await.unwrap();
        let mut open = crate::backup::bundle::OpenBundle::open(&fetched).unwrap();
        let refs = open.notes()[0].doc.jodd.as_ref().unwrap().attachments.clone();
        assert_eq!(refs.len(), 1);
        assert_eq!(open.attachment(&refs[0].path).unwrap(), vec![7, 8, 9]);
    }

    /// A local destination whose sweep always errors.
    struct SweepFails(LocalDest);

    #[async_trait::async_trait]
    impl Destination for SweepFails {
        fn lock_key(&self) -> String {
            self.0.lock_key()
        }
        async fn list_slugs(&self) -> Result<Vec<String>, String> {
            self.0.list_slugs().await
        }
        async fn list_versions(&self, slug: &str) -> Result<Vec<VersionEntry>, String> {
            self.0.list_versions(slug).await
        }
        async fn publish(&self, slug: &str, stamp: &str, zip: &Path, m: &Manifest) -> Result<(), String> {
            self.0.publish(slug, stamp, zip, m).await
        }
        async fn fetch_zip(&self, slug: &str, stamp: &str, to: &Path) -> Result<(), String> {
            self.0.fetch_zip(slug, stamp, to).await
        }
        async fn delete_version(&self, slug: &str, stamp: &str) -> Result<(), String> {
            self.0.delete_version(slug, stamp).await
        }
        async fn sweep_orphans(&self, _: &str) -> Result<usize, String> {
            Err("cannot delete".into())
        }
        async fn probe(&self) -> Result<(), String> {
            self.0.probe().await
        }
    }

    /// A file in `dir` whose modification time is `age` ago.
    pub(crate) fn aged_file(dir: &Path, name: &str, age: std::time::Duration) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_modified(std::time::SystemTime::now() - age).unwrap();
        p
    }

    const TWO_HOURS: std::time::Duration = std::time::Duration::from_secs(2 * 3600);

    #[test]
    fn stale_work_files_are_swept_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let old_restore = aged_file(d.path(), "restore-2026-10-23T09-30-00Z.zip", TWO_HOURS);
        let crashed = aged_file(d.path(), "3f2a9c1b-0000-4000-8000-000000000001.zip", TWO_HOURS);
        let scratch = aged_file(d.path(), "restore-2026-10-23T09-30-00Z.zip.abc.part", TWO_HOURS);
        let in_use = aged_file(d.path(), "restore-2026-10-22T09-30-00Z.zip", TWO_HOURS);
        let fresh = aged_file(d.path(), "restore-2026-10-24T09-30-00Z.zip", std::time::Duration::ZERO);
        let other = aged_file(d.path(), "notes.txt", TWO_HOURS);
        std::fs::create_dir(d.path().join("old.zip")).unwrap();

        sweep_stale_work_files(d.path(), Some(&in_use));

        for gone in [&old_restore, &crashed, &scratch] {
            assert!(!gone.exists(), "{} should be swept", gone.display());
        }
        for kept in [&in_use, &fresh, &other] {
            assert!(kept.exists(), "{} should be kept", kept.display());
        }
        assert!(d.path().join("old.zip").is_dir(), "a directory is never touched");
        // A missing work dir is not an error.
        sweep_stale_work_files(&d.path().join("absent"), None);
    }

    #[tokio::test]
    async fn a_backup_sweeps_a_crashed_backups_leftover() {
        let db = seeded_db(&["u1"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let leftover = aged_file(work.path(), "0a1b2c3d-0000-4000-8000-000000000009.zip", TWO_HOURS);
        let dest = LocalDest { root: root.path().to_path_buf() };

        run_backup_in(db, &account(), &dest, Trigger::Manual, None, work.path(), NOW).await.unwrap();
        assert!(!leftover.exists());
        assert_eq!(std::fs::read_dir(work.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_failing_sweep_does_not_stop_the_backup() {
        let db = seeded_db(&["u1"]);
        let root = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dest = SweepFails(LocalDest { root: root.path().to_path_buf() });

        run_backup_in(db, &account(), &dest, Trigger::Manual, None, work.path(), NOW).await.unwrap();
        assert_eq!(dest.list_versions(&slug()).await.unwrap().len(), 1);
    }
}
