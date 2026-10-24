//! Tauri face of backup and restore (spec 2026-10-23 backup-restore). Thin on
//! purpose: the work is in `backup::run` and `backup::restore`; this module
//! looks up the account and the destination, fetches and verifies the bundle
//! for a restore, and tells the UI what happened.
//!
//! A backup reads the account's cache and writes to a destination; a restore
//! writes SQLite only and the sync worker pushes the rows afterwards. The one
//! remote call is `ensure_full_listing`: an account with no complete listing
//! yet gets one first (the same full sync as the refresh button). The owner
//! started the backup or restore, so this is an explicit user-triggered
//! refresh, which the local-first doctrine allows.

use crate::accounts::{Account, AccountStatus};
use crate::backup::bundle::OpenBundle;
use crate::backup::config::{self, BackupConfig, DestKind, DestinationConfig, LastBackup};
use crate::backup::destination::{Destination, VersionEntry};
use crate::backup::restore::apply::{apply_restore, RestoreReport};
use crate::backup::restore::plan::{plan_restore, Action, RestorePlan, RestoreRequest};
use crate::backup::run::{run_backup, run_backup_in, sweep_stale_work_files, BackupOutcome};
use crate::backup::Trigger;
use crate::db::Db;
use crate::AppState;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{Emitter, State};

const DAMAGED: &str = "This backup file is damaged (checksum mismatch).";

/// Runs `sync` (a full listing of the account) when the account has no
/// first-complete-listing mark yet. Backup and restore both refuse an
/// account without that mark, and the app's own full listing runs only on
/// a 10-minute timer that pauses while the window is in the background, so
/// waiting for it can take indefinitely (live pass 2026-10-24). An
/// incomplete listing sets no mark; the caller then refuses as before.
async fn ensure_full_listing<F, Fut>(db: &Db, account_id: &str, sync: F) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let marked = |db: &Db| db.full_listing_at(account_id).map(|m| m.is_some()).map_err(|e| e.to_string());
    if marked(db)? {
        return Ok(());
    }
    crate::log!("backup: {account_id} has no complete listing yet; running a full sync first");
    if let Err(e) = sync().await {
        crate::log!("backup: full sync of {account_id} failed: {e}");
        return Err(format!("Couldn't sync this account first: {e}"));
    }
    if marked(db)? {
        crate::log!("backup: full sync of {account_id} complete");
    } else {
        crate::log!("backup: full sync of {account_id} was incomplete; no mark set");
    }
    Ok(())
}

/// `ensure_full_listing` through the app's own full listing (`list_notes`).
async fn sync_first(state: &State<'_, AppState>, account_id: &str) -> Result<(), String> {
    let id = account_id.to_string();
    ensure_full_listing(&state.db, account_id, || async move {
        crate::list_notes(id, state.clone()).await.map(|_| ())
    })
    .await
}

/// Sent when a backup finishes, on success and on failure alike.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupFinished {
    account_id: String,
    destination_id: String,
    ok: bool,
    message: String,
}

/// The account `account_id` names, if `usable` accepts it.
fn find_account(accounts: &[Account], account_id: &str, usable: fn(&Account) -> bool) -> Option<Account> {
    accounts.iter().find(|a| a.id == account_id && usable(a)).cloned()
}

/// A backup only reads the cache, so an account waiting to be removed
/// (Draining, spec §7) can still be backed up: arguably the most useful moment.
fn can_back_up(a: &Account) -> bool {
    a.is_active() || a.status == AccountStatus::Draining
}

fn backup_source(state: &AppState, account_id: &str) -> Result<Account, String> {
    find_account(&state.accounts.lock().unwrap(), account_id, can_back_up)
        .ok_or_else(|| format!("No account '{account_id}' to back up."))
}

/// A restore target writes notes, so it must be active.
fn active_account(state: &AppState, account_id: &str) -> Result<Account, String> {
    find_account(&state.accounts.lock().unwrap(), account_id, Account::is_active)
        .ok_or_else(|| format!("No active account '{account_id}'."))
}

fn destination_config(id: &str) -> Result<DestinationConfig, String> {
    config::load()
        .destinations
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("No backup destination '{id}'."))
}

fn destination(id: &str) -> Result<Box<dyn Destination>, String> {
    Ok(config::destination_for(&destination_config(id)?))
}

/// The status line of a finished backup. `backupSummary` in
/// `src/lib/backup/state.ts` says the same; change both together.
fn backup_summary(outcome: &BackupOutcome) -> String {
    let mut msg = format!("Backed up {} notes", outcome.notes);
    let mut counts = Vec::new();
    if outcome.partial > 0 {
        counts.push(format!("{} partial", outcome.partial));
    }
    if outcome.skipped > 0 {
        counts.push(format!("{} skipped", outcome.skipped));
    }
    if !counts.is_empty() {
        msg.push_str(&format!(" ({})", counts.join(", ")));
    }
    msg
}

fn now_ms() -> i64 {
    crate::db::now_ms()
}

#[tauri::command]
pub async fn backup_destinations() -> Result<Vec<DestinationConfig>, String> {
    let destinations = config::load().destinations;
    crate::log!("backup: {} destination(s) listed", destinations.len());
    Ok(destinations)
}

#[tauri::command]
pub async fn backup_add_local_destination(name: String, path: String) -> Result<DestinationConfig, String> {
    let (name, path) = (name.trim().to_string(), path.trim().to_string());
    if name.is_empty() {
        return Err("Give the destination a name.".to_string());
    }
    if path.is_empty() {
        return Err("Choose a folder for the backups.".to_string());
    }
    crate::log!("backup: adding destination {path}");
    let dest = DestinationConfig { id: uuid::Uuid::new_v4().to_string(), name, kind: DestKind::Local { path } };
    // Saved only once the folder is proven writable.
    if let Err(e) = config::destination_for(&dest).probe().await {
        crate::log!("backup: destination refused: {e}");
        return Err(e);
    }
    let mut cfg = config::load();
    cfg.destinations.push(dest.clone());
    config::save(&cfg)?;
    crate::log!("backup: destination added ({} in total)", cfg.destinations.len());
    Ok(dest)
}

/// Forgets the config entry. Never touches the files at the destination.
#[tauri::command]
pub async fn backup_remove_destination(id: String) -> Result<(), String> {
    let cfg = config::load();
    let destinations: Vec<DestinationConfig> = cfg.destinations.into_iter().filter(|d| d.id != id).collect();
    config::save(&BackupConfig { destinations })
}

#[tauri::command]
pub async fn backup_run(
    account_id: String,
    destination_id: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<BackupOutcome, String> {
    let result = async {
        let account = backup_source(&state, &account_id)?;
        let dest = destination(&destination_id)?;
        sync_first(&state, &account.id).await?;
        run_backup(state.db.clone(), &account, dest.as_ref(), Trigger::Manual, None).await
    }
    .await;
    let (ok, message) = match &result {
        Ok(outcome) => (true, backup_summary(outcome)),
        Err(e) => (false, e.clone()),
    };
    if let Err(e) = config::record_status(LastBackup {
        account_id: account_id.clone(),
        destination_id: destination_id.clone(),
        at_ms: now_ms(),
        ok,
        message: message.clone(),
    }) {
        crate::log!("backup: could not record the status of {account_id}: {e}");
    }
    crate::log!("backup: {account_id} → {destination_id}: {}", if ok { message.as_str() } else { "failed" });
    let _ = app.emit("backup-finished", BackupFinished { account_id, destination_id, ok, message });
    result
}

#[tauri::command]
pub async fn backup_statuses() -> Result<Vec<LastBackup>, String> {
    let statuses = config::statuses();
    crate::log!("backup: {} status(es) listed", statuses.len());
    Ok(statuses)
}

#[tauri::command]
pub async fn backup_list_slugs(destination_id: String) -> Result<Vec<String>, String> {
    destination(&destination_id)?.list_slugs().await
}

#[tauri::command]
pub async fn backup_list_versions(destination_id: String, slug: String) -> Result<Vec<VersionEntry>, String> {
    destination(&destination_id)?.list_versions(&slug).await
}

#[tauri::command]
pub async fn backup_delete_version(destination_id: String, slug: String, stamp: String) -> Result<(), String> {
    destination(&destination_id)?.delete_version(&slug, &stamp).await
}

/// Lowercase hex sha256 of a file, read in chunks on a blocking thread.
async fn file_sha256(path: PathBuf) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).map_err(|e| format!("read {}: {e}", path.display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
    })
    .await
    .map_err(|e| format!("restore task failed: {e}"))?
}

/// Whether a file already in the restore cache can be used as it is: only
/// when its hash is known and equals the manifest's.
fn cached_zip_is_usable(cached_sha: Option<&str>, manifest_sha: &str) -> bool {
    !manifest_sha.is_empty() && cached_sha == Some(manifest_sha)
}

/// The bundle zip for `stamp`, in `cache_dir` as `restore-<stamp>.zip`.
/// Reuses a file there whose sha256 equals `expected_sha`; otherwise fetches
/// into a scratch name, checks the hash, and only then renames it into place,
/// so a damaged download never sits under the cache name. Sweeps what earlier
/// runs left behind first, since the cache shares a nearly full disk.
async fn verified_zip(
    dest: &dyn Destination,
    slug: &str,
    stamp: &str,
    expected_sha: &str,
    cache_dir: &Path,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("mkdir {}: {e}", cache_dir.display()))?;
    let cached = cache_dir.join(format!("restore-{stamp}.zip"));
    sweep_stale_work_files(cache_dir, Some(&cached));
    if cached.exists() {
        let sha = file_sha256(cached.clone()).await.ok();
        if cached_zip_is_usable(sha.as_deref(), expected_sha) {
            // Fresh again, so a sweep elsewhere cannot take it mid-restore.
            if let Err(e) = std::fs::File::options()
                .write(true)
                .open(&cached)
                .and_then(|f| f.set_modified(std::time::SystemTime::now()))
            {
                crate::log!("restore: could not mark {} as in use: {e}", cached.display());
            }
            return Ok(cached);
        }
    }
    let scratch = cache_dir.join(format!("restore-{stamp}.zip.{}.part", uuid::Uuid::new_v4().simple()));
    let fetched = async {
        dest.fetch_zip(slug, stamp, &scratch).await?;
        let sha = file_sha256(scratch.clone()).await?;
        if !cached_zip_is_usable(Some(&sha), expected_sha) {
            return Err(DAMAGED.to_string());
        }
        std::fs::rename(&scratch, &cached).map_err(|e| format!("rename to {}: {e}", cached.display()))
    }
    .await;
    if fetched.is_err() {
        let _ = std::fs::remove_file(&scratch);
    }
    fetched?;
    Ok(cached)
}

/// Scratch space for backups and downloaded restore bundles.
fn backup_cache_dir() -> Result<PathBuf, String> {
    Ok(crate::paths::cache_base()
        .ok_or_else(|| "no cache directory to stage the backup in".to_string())?
        .join("backup"))
}

/// The version the request names, found by listing so the stamp that reaches
/// a file name is one the destination itself reported.
async fn find_version(dest: &dyn Destination, req: &RestoreRequest) -> Result<VersionEntry, String> {
    dest.list_versions(&req.slug)
        .await?
        .into_iter()
        .find(|v| v.stamp == req.stamp)
        .ok_or_else(|| "That backup version no longer exists.".to_string())
}

/// Fetch and verify the requested version's zip into `cache_dir`. Returns its
/// path and its sha256, which names the backup in `restore_map`.
async fn fetch_bundle(dest: &dyn Destination, req: &RestoreRequest, cache_dir: &Path) -> Result<(PathBuf, String), String> {
    let version = find_version(dest, req).await?;
    let zip = verified_zip(dest, &req.slug, &req.stamp, &version.manifest.zip_sha256, cache_dir).await?;
    Ok((zip, version.manifest.zip_sha256))
}

/// Open the bundle and plan against the target's current cache. Returns the
/// open bundle too, so an apply reads the file it planned.
async fn open_and_plan(
    db: Arc<Db>,
    target: &Account,
    req: &RestoreRequest,
    zip: PathBuf,
) -> Result<(OpenBundle, RestorePlan), String> {
    let (target, req) = (target.clone(), req.clone());
    tokio::task::spawn_blocking(move || {
        let bundle = OpenBundle::open(&zip)?;
        let plan = plan_restore(&db, &target, &bundle, &req)?;
        Ok::<_, String>((bundle, plan))
    })
    .await
    .map_err(|e| format!("restore task failed: {e}"))?
}

/// The preview keeps the downloaded zip, so the apply that usually follows
/// reuses it; one never applied is swept an hour later.
#[tauri::command]
pub async fn restore_preview(req: RestoreRequest, state: State<'_, AppState>) -> Result<RestorePlan, String> {
    let target = active_account(&state, &req.target_account)?;
    let dest = destination(&req.destination_id)?;
    sync_first(&state, &target.id).await?;
    let (zip, _) = fetch_bundle(dest.as_ref(), &req, &backup_cache_dir()?).await?;
    open_and_plan(state.db.clone(), &target, &req, zip).await.map(|(_, plan)| plan)
}

/// Existing notes a plan would overwrite.
fn replaced_uuids(plan: &RestorePlan) -> HashSet<String> {
    plan.notes
        .iter()
        .filter_map(|p| match &p.action {
            Action::Replace { existing_uuid } => Some(existing_uuid.clone()),
            _ => None,
        })
        .collect()
}

/// The write half of a restore. A plan that replaces notes is applied only
/// after those notes have been backed up to the same destination
/// (`Trigger::PreRestore`, only the notes it will overwrite): a Replace
/// destroys the only copy of the note's current text, so if the safety backup
/// fails nothing is written. `after_apply` runs once apply has been attempted,
/// whether or not it succeeded, because a restore that fails part-way has
/// still written folders and notes the UI must be told about (gotcha #6); it
/// does not run when the safety backup stopped the restore.
async fn safety_backup_then_apply(
    db: Arc<Db>,
    target: Account,
    dest: &dyn Destination,
    plan: RestorePlan,
    mut bundle: OpenBundle,
    backup_id: String,
    work_dir: &Path,
    after_apply: impl FnOnce(),
) -> Result<RestoreReport, String> {
    let replaced = replaced_uuids(&plan);
    if !replaced.is_empty() {
        let wanted = replaced.len();
        let outcome =
            run_backup_in(db.clone(), &target, dest, Trigger::PreRestore, Some(replaced), work_dir, crate::db::now_ms())
                .await
                .map_err(|e| format!("Restore stopped: the safety backup of the notes it would replace failed ({e})."))?;
        // The writer can leave a note out and still succeed; a Replace of a
        // note the backup does not hold would destroy its only copy.
        if outcome.notes != wanted || outcome.skipped > 0 {
            return Err("Restore stopped: the safety backup did not include every note it would replace.".to_string());
        }
    }
    let applied =
        tokio::task::spawn_blocking(move || apply_restore(&db, &target, &plan, &mut bundle, &backup_id)).await;
    after_apply();
    applied.map_err(|e| format!("restore task failed: {e}"))?
}

/// Fetch, plan again and apply. Planned again, not trusted from the preview:
/// nothing may change between what the owner confirmed and what is written
/// without being seen. The downloaded zip is removed afterwards, whether or
/// not the restore worked, once the bundle is closed (Windows cannot delete
/// an open file).
async fn restore_from(
    db: Arc<Db>,
    target: Account,
    dest: &dyn Destination,
    req: &RestoreRequest,
    cache_dir: &Path,
    after_apply: impl FnOnce(),
) -> Result<RestoreReport, String> {
    let (zip, backup_id) = fetch_bundle(dest, req, cache_dir).await?;
    let result = async {
        let (bundle, plan) = open_and_plan(db.clone(), &target, req, zip.clone()).await?;
        safety_backup_then_apply(db, target, dest, plan, bundle, backup_id, cache_dir, after_apply).await
    }
    .await;
    if let Err(e) = std::fs::remove_file(&zip) {
        crate::log!("restore: could not remove the downloaded bundle {}: {e}", zip.display());
    }
    result
}

#[tauri::command]
pub async fn restore_apply(
    req: RestoreRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<RestoreReport, String> {
    let target = active_account(&state, &req.target_account)?;
    let dest = destination(&req.destination_id)?;
    sync_first(&state, &target.id).await?;
    let target_id = target.id.clone();
    let work_dir = backup_cache_dir()?;
    // Gotcha #6: apply writes SQLite directly; the UI learns of the new notes
    // and folders only through this nudge (App.svelte's remote-changed ->
    // requestRefresh -> loadNotes). Sent after a failed apply too.
    let nudge = app.clone();
    let nudge_id = target_id.clone();
    let report = restore_from(state.db.clone(), target, dest.as_ref(), &req, &work_dir, move || {
        let _ = nudge.emit("remote-changed", nudge_id);
    })
    .await?;

    crate::log!(
        "restore: {} → {}: {} created, {} replaced, {} skipped, {} refused",
        req.slug, target_id, report.created, report.replaced, report.skipped, report.refused.len()
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod ensure_full_listing_tests {
        use super::*;
        use std::cell::Cell;

        const ACCT: &str = "gmail:a@x";

        #[tokio::test]
        async fn a_marked_account_is_not_synced_again() {
            let db = crate::test_support::temp_db();
            db.record_full_listing(ACCT).unwrap();
            let calls = Cell::new(0);
            ensure_full_listing(&db, ACCT, || async { calls.set(calls.get() + 1); Ok(()) }).await.unwrap();
            assert_eq!(calls.get(), 0);
        }

        #[tokio::test]
        async fn an_unmarked_account_is_synced_once_and_marked() {
            let db = crate::test_support::temp_db();
            let calls = Cell::new(0);
            ensure_full_listing(&db, ACCT, || async {
                calls.set(calls.get() + 1);
                db.record_full_listing(ACCT).unwrap();
                Ok(())
            })
            .await
            .unwrap();
            assert_eq!(calls.get(), 1);
            assert!(db.full_listing_at(ACCT).unwrap().is_some());
        }

        #[tokio::test]
        async fn an_incomplete_sync_leaves_the_refusal_to_the_caller() {
            let db = crate::test_support::temp_db();
            ensure_full_listing(&db, ACCT, || async { Ok(()) }).await.unwrap();
            assert_eq!(db.full_listing_at(ACCT).unwrap(), None);
        }

        #[tokio::test]
        async fn a_failed_sync_is_reported() {
            let db = crate::test_support::temp_db();
            let err = ensure_full_listing(&db, ACCT, || async { Err("offline".to_string()) }).await.unwrap_err();
            assert_eq!(err, "Couldn't sync this account first: offline");
        }
    }
    use crate::backup::destination::LocalDest;
    use crate::backup::manifest::{Manifest, FORMAT_VERSION};

    const SLUG: &str = "gmail_a@x";
    const STAMP: &str = "2026-10-23T09-30-00Z";

    fn sha_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn manifest(sha: &str) -> Manifest {
        Manifest {
            jodd_backup: FORMAT_VERSION,
            account_id: "gmail:a@x".into(),
            backend: "gmail".into(),
            root_label: "Notes".into(),
            created_ms: 1_792_747_800_000,
            app_version: "0.0.0".into(),
            device: "macos".into(),
            trigger: Trigger::Manual,
            keep: false,
            zip_bytes: 0,
            zip_sha256: sha.into(),
            notes: vec![],
            folders: vec![],
            skipped: vec![],
        }
    }

    /// A destination holding one version whose zip is `bytes`.
    async fn dest_with(root: &Path, bytes: &[u8], manifest_sha: &str) -> LocalDest {
        let dest = LocalDest { root: root.join("dest") };
        let src = root.join("src.zip");
        std::fs::write(&src, bytes).unwrap();
        dest.publish(SLUG, STAMP, &src, &manifest(manifest_sha)).await.unwrap();
        dest
    }

    #[test]
    fn a_cached_zip_is_usable_only_on_a_known_matching_hash() {
        assert!(cached_zip_is_usable(Some("abc"), "abc"));
        assert!(!cached_zip_is_usable(Some("abd"), "abc"));
        assert!(!cached_zip_is_usable(None, "abc"));
        // A manifest with no hash vouches for nothing.
        assert!(!cached_zip_is_usable(Some(""), ""));
    }

    #[tokio::test]
    async fn verified_zip_fetches_checks_and_caches() {
        let d = tempfile::tempdir().unwrap();
        let bytes = b"the zip".as_slice();
        let dest = dest_with(d.path(), bytes, &sha_hex(bytes)).await;
        let cache = d.path().join("cache");
        let got = verified_zip(&dest, SLUG, STAMP, &sha_hex(bytes), &cache).await.unwrap();
        assert_eq!(got, cache.join(format!("restore-{STAMP}.zip")));
        assert_eq!(std::fs::read(&got).unwrap(), bytes);
        let leftovers: Vec<_> = std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 1, "no scratch file left behind: {leftovers:?}");
    }

    #[tokio::test]
    async fn verified_zip_reuses_a_matching_cached_file_without_fetching() {
        let d = tempfile::tempdir().unwrap();
        let bytes = b"the zip".as_slice();
        // Nothing at the destination: a fetch would fail, so success proves reuse.
        let dest = LocalDest { root: d.path().join("empty") };
        let cache = d.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join(format!("restore-{STAMP}.zip")), bytes).unwrap();
        let got = verified_zip(&dest, SLUG, STAMP, &sha_hex(bytes), &cache).await.unwrap();
        assert_eq!(std::fs::read(got).unwrap(), bytes);
    }

    #[tokio::test]
    async fn verified_zip_replaces_a_stale_cached_file() {
        let d = tempfile::tempdir().unwrap();
        let bytes = b"the zip".as_slice();
        let dest = dest_with(d.path(), bytes, &sha_hex(bytes)).await;
        let cache = d.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        // Another backup that shared the stamp, or a truncated file.
        std::fs::write(cache.join(format!("restore-{STAMP}.zip")), b"stale").unwrap();
        let got = verified_zip(&dest, SLUG, STAMP, &sha_hex(bytes), &cache).await.unwrap();
        assert_eq!(std::fs::read(got).unwrap(), bytes);
    }

    #[tokio::test]
    async fn verified_zip_refuses_a_damaged_fetch_and_caches_nothing() {
        let d = tempfile::tempdir().unwrap();
        // The manifest promises a hash the stored zip does not have.
        let dest = dest_with(d.path(), b"damaged", &sha_hex(b"original")).await;
        let cache = d.path().join("cache");
        let err = verified_zip(&dest, SLUG, STAMP, &sha_hex(b"original"), &cache).await.unwrap_err();
        assert_eq!(err, "This backup file is damaged (checksum mismatch).");
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0, "a damaged file must not be cached");
    }

    #[tokio::test]
    async fn verified_zip_sweeps_stale_files_but_keeps_and_freshens_the_one_it_reuses() {
        use crate::backup::run::tests::aged_file;
        let d = tempfile::tempdir().unwrap();
        let bytes = b"the zip".as_slice();
        let dest = LocalDest { root: d.path().join("empty") };
        let cache = d.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        let two_hours = std::time::Duration::from_secs(2 * 3600);
        let stale = aged_file(&cache, "restore-2026-10-01T09-30-00Z.zip", two_hours);
        let reused = aged_file(&cache, &format!("restore-{STAMP}.zip"), two_hours);
        std::fs::write(&reused, bytes).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&reused)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - two_hours)
            .unwrap();

        let got = verified_zip(&dest, SLUG, STAMP, &sha_hex(bytes), &cache).await.unwrap();
        assert_eq!(got, reused);
        assert!(!stale.exists(), "an abandoned restore download is swept");
        let age = std::fs::metadata(&got).unwrap().modified().unwrap().elapsed().unwrap();
        assert!(age < std::time::Duration::from_secs(60), "reuse marks it fresh, so no sweep takes it mid-restore");
    }

    #[test]
    fn only_replace_actions_are_backed_up_first() {
        use crate::backup::restore::plan::PlannedNote;
        let note = |action| PlannedNote {
            source_uuid: "s".into(),
            title: "t".into(),
            target_folder: "Notes".into(),
            action,
            losses: vec![],
        };
        let plan = RestorePlan {
            notes: vec![
                note(Action::Create),
                note(Action::Replace { existing_uuid: "u1".into() }),
                note(Action::KeepBoth { existing_uuid: "u2".into() }),
                note(Action::Replace { existing_uuid: "u3".into() }),
            ],
            folders_to_create: vec![],
        };
        assert_eq!(replaced_uuids(&plan), HashSet::from(["u1".to_string(), "u3".to_string()]));
    }

    #[test]
    fn an_account_waiting_to_be_removed_can_be_backed_up_but_not_restored_into() {
        // Spec §7: pending_removal (Draining) is arguably the most useful moment.
        let with = |id: &str, status| {
            let mut a = account();
            a.id = id.into();
            a.status = status;
            a
        };
        let all = vec![
            with("gmail:active@x", AccountStatus::Active),
            with("gmail:draining@x", AccountStatus::Draining),
            with("gmail:inactive@x", AccountStatus::Inactive),
        ];
        let source = |id: &str| find_account(&all, id, can_back_up).map(|a| a.id);
        let target = |id: &str| find_account(&all, id, Account::is_active).map(|a| a.id);
        assert_eq!(source("gmail:active@x").as_deref(), Some("gmail:active@x"));
        assert_eq!(source("gmail:draining@x").as_deref(), Some("gmail:draining@x"));
        assert_eq!(source("gmail:inactive@x"), None);
        assert_eq!(source("gmail:nobody@x"), None);
        assert_eq!(target("gmail:active@x").as_deref(), Some("gmail:active@x"));
        assert_eq!(target("gmail:draining@x"), None);
    }

    #[test]
    fn the_finished_event_is_camel_case() {
        let json = serde_json::to_value(BackupFinished {
            account_id: "a".into(),
            destination_id: "d".into(),
            ok: true,
            message: "m".into(),
        })
        .unwrap();
        assert_eq!(json, serde_json::json!({"accountId":"a","destinationId":"d","ok":true,"message":"m"}));
    }

    #[test]
    fn the_summary_names_partial_and_skipped_notes_only_when_there_are_some() {
        let outcome = |partial, skipped| BackupOutcome { stamp: "s".into(), notes: 812, partial, skipped, bytes: 1 };
        assert_eq!(backup_summary(&outcome(0, 0)), "Backed up 812 notes");
        assert_eq!(backup_summary(&outcome(0, 3)), "Backed up 812 notes (3 skipped)");
        // Spec §11: the completion report shows the partial notes, with the count.
        assert_eq!(backup_summary(&outcome(5, 0)), "Backed up 812 notes (5 partial)");
        assert_eq!(backup_summary(&outcome(5, 3)), "Backed up 812 notes (5 partial, 3 skipped)");
    }

    // ---- the safety backup before a Replace -------------------------------

    use crate::accounts::{BackendKind, LlmConfig};
    use crate::backup::body::content_sha256;
    use crate::backup::bundle::{write_bundle, BundleSource};
    use crate::backup::manifest::Manifest as BackupManifest;
    use crate::backup::restore::plan::{ExistingChoice, RestoreRequest as Req};
    use crate::backup::restore::transform::Placement;
    use crate::db::{RemotePin, SyncState};
    use crate::provenance::Actor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ACCT: &str = "gmail:a@x.com";
    const U1: &str = "3F2A9C1B-0000-4000-8000-000000000001";
    const U2: &str = "7D4E2F10-0000-4000-8000-000000000002";
    const ORIGINAL: &str = "<div>Alpha</div><div>alpha body</div>";
    const EDITED: &str = "<div>Alpha</div><div>edited since the backup</div>";

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

    /// Two notes backed up into a bundle file, then the first edited, so a
    /// Replace restoring it has something to destroy. Returns a plan that
    /// replaces only the first note.
    fn replace_scenario(dir: &Path) -> (Arc<Db>, Account, OpenBundle, RestorePlan) {
        let acct = account();
        let db = crate::test_support::temp_db();
        for (uuid, title, body) in [(U1, "Alpha", ORIGINAL), (U2, "Beta", "<div>Beta</div><div>beta body</div>")] {
            let mut n = crate::test_support::note(ACCT, uuid).title(title).body(body).build();
            n.x_mail_created_date = Some("Thu, 4 Jun 2026 01:19:50 +0700".into());
            db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
        }
        db.record_full_listing(ACCT).unwrap();

        let read = db.backup_read(ACCT).unwrap();
        let source = BundleSource {
            account_id: ACCT,
            backend: "gmail",
            root_label: "Notes",
            trigger: Trigger::Manual,
            created_ms: 1_792_747_800_000,
            app_version: "0.0.0-test",
            device: "test",
            read: &read,
            only: None,
        };
        let path = dir.join("backup.zip");
        let mut fetch = |u: &str, c: &str| db.attachment_bytes(ACCT, u, c).map_err(|e| e.to_string());
        write_bundle(&source, std::fs::File::create(&path).unwrap(), &mut fetch).unwrap();
        let bundle = OpenBundle::open(&path).unwrap();

        db.apply_local_edit(U1, ACCT, "Alpha", EDITED, "Notes", &Actor::Human).unwrap();
        let req = Req {
            destination_id: "d".into(),
            slug: "s".into(),
            stamp: "2026-10-23T09-30-00Z".into(),
            source_uuids: vec![U1.to_string()],
            target_account: ACCT.to_string(),
            placement: Placement::Original,
            existing: ExistingChoice::Replace,
        };
        let plan = plan_restore(&db, &acct, &bundle, &req).unwrap();
        assert_eq!(replaced_uuids(&plan), HashSet::from([U1.to_string()]), "scenario must plan a Replace");
        (Arc::new(db), acct, bundle, plan)
    }

    /// A destination that cannot be written to.
    struct ReadOnlyDest;

    #[async_trait::async_trait]
    impl Destination for ReadOnlyDest {
        fn lock_key(&self) -> String {
            "read-only".into()
        }
        async fn list_slugs(&self) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        async fn list_versions(&self, _: &str) -> Result<Vec<VersionEntry>, String> {
            Ok(vec![])
        }
        async fn publish(&self, _: &str, _: &str, _: &Path, _: &BackupManifest) -> Result<(), String> {
            Err("disk is read-only".into())
        }
        async fn fetch_zip(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
            Err("nothing here".into())
        }
        async fn delete_version(&self, _: &str, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn sweep_orphans(&self, _: &str) -> Result<usize, String> {
            Ok(0)
        }
        async fn probe(&self) -> Result<(), String> {
            Err("disk is read-only".into())
        }
    }

    #[tokio::test]
    async fn a_failed_safety_backup_stops_the_restore_before_any_write() {
        let d = tempfile::tempdir().unwrap();
        let (db, acct, bundle, plan) = replace_scenario(d.path());
        let nudges = AtomicUsize::new(0);

        let err = safety_backup_then_apply(
            db.clone(),
            acct,
            &ReadOnlyDest,
            plan,
            bundle,
            "backup-1".into(),
            &d.path().join("work"),
            || {
                nudges.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap_err();

        assert!(err.starts_with("Restore stopped: the safety backup of the notes it would replace failed"), "{err}");
        assert!(err.contains("disk is read-only"), "{err}");
        let note = db.get(U1, ACCT).unwrap().unwrap();
        assert_eq!(note.body_html, EDITED, "the note the restore would have replaced is untouched");
        assert_eq!(note.local_version, 2, "no write of any kind reached it");
        assert_eq!(nudges.load(Ordering::SeqCst), 0, "apply never ran, so there is nothing to refresh");
    }

    #[tokio::test]
    async fn the_replaced_note_is_backed_up_before_the_restore_overwrites_it() {
        let d = tempfile::tempdir().unwrap();
        let (db, acct, bundle, plan) = replace_scenario(d.path());
        let dest = LocalDest { root: d.path().join("dest") };
        let nudges = AtomicUsize::new(0);

        let report = safety_backup_then_apply(
            db.clone(),
            acct,
            &dest,
            plan,
            bundle,
            "backup-1".into(),
            &d.path().join("work"),
            || {
                nudges.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap();

        assert_eq!((report.replaced, report.created), (1, 0));
        assert_eq!(nudges.load(Ordering::SeqCst), 1);

        // The edit landed...
        // (Gmail re-wraps the title on save, so compare content, not bytes.)
        let after = db.get(U1, ACCT).unwrap().unwrap();
        assert!(after.body_html.contains("alpha body"), "{}", after.body_html);
        assert!(!after.body_html.contains("edited since the backup"), "{}", after.body_html);
        assert_eq!(after.sync_state, SyncState::Dirty);

        // ...and the text it destroyed is in a PreRestore version holding only that note.
        let slug = crate::backup::naming::account_slug(ACCT);
        let versions = dest.list_versions(&slug).await.unwrap();
        assert_eq!(versions.len(), 1);
        let m = &versions[0].manifest;
        assert_eq!(m.trigger, Trigger::PreRestore);
        assert_eq!(m.notes.len(), 1, "only the replaced note: {:?}", m.notes);
        assert_eq!(m.notes[0].uuid, U1);
        assert_eq!(m.notes[0].content_sha256, content_sha256(EDITED));
    }

    #[tokio::test]
    async fn a_safety_backup_missing_a_replaced_note_stops_the_restore() {
        let d = tempfile::tempdir().unwrap();
        let (db, acct, bundle, mut plan) = replace_scenario(d.path());
        // A note the snapshot cannot hold (gone from the cache since planning).
        let mut gone = plan.notes[0].clone();
        gone.action = Action::Replace { existing_uuid: "0BADF00D-0000-4000-8000-000000000009".into() };
        plan.notes.push(gone);
        let dest = LocalDest { root: d.path().join("dest") };
        let nudges = AtomicUsize::new(0);

        let err = safety_backup_then_apply(
            db.clone(),
            acct,
            &dest,
            plan,
            bundle,
            "backup-1".into(),
            &d.path().join("work"),
            || {
                nudges.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Restore stopped: the safety backup did not include every note it would replace.");
        let note = db.get(U1, ACCT).unwrap().unwrap();
        assert_eq!(note.body_html, EDITED, "the note the restore would have replaced is untouched");
        assert_eq!(nudges.load(Ordering::SeqCst), 0);
    }

    /// `ACCT` backed up to a local destination; returns the request that
    /// restores all of it into `gmail:b@x.com`.
    async fn published(dir: &Path) -> (Arc<Db>, LocalDest, Account, Req) {
        let db = crate::test_support::temp_db();
        let n = crate::test_support::note(ACCT, U1).title("Alpha").body(ORIGINAL).build();
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
        db.record_full_listing(ACCT).unwrap();
        let db = Arc::new(db);
        let dest = LocalDest { root: dir.join("dest") };
        let out = run_backup_in(db.clone(), &account(), &dest, Trigger::Manual, None, &dir.join("work"), 1_792_747_800_000)
            .await
            .unwrap();
        let mut target = account();
        target.id = "gmail:b@x.com".into();
        target.email = "b@x.com".into();
        let req = Req {
            destination_id: "d".into(),
            slug: crate::backup::naming::account_slug(ACCT),
            stamp: out.stamp,
            source_uuids: vec![U1.to_string()],
            target_account: target.id.clone(),
            placement: Placement::Original,
            existing: ExistingChoice::KeepBoth,
        };
        (db, dest, target, req)
    }

    fn cached_zips(cache: &Path) -> Vec<String> {
        std::fs::read_dir(cache)
            .map(|d| d.map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_restore_removes_its_downloaded_bundle_afterwards() {
        let d = tempfile::tempdir().unwrap();
        let (db, dest, target, req) = published(d.path()).await;
        db.record_full_listing(&target.id).unwrap();
        let cache = d.path().join("cache");

        let report = restore_from(db.clone(), target, &dest, &req, &cache, || {}).await.unwrap();
        assert_eq!(report.created, 1);
        assert!(cached_zips(&cache).is_empty(), "{:?}", cached_zips(&cache));
    }

    #[tokio::test]
    async fn a_failed_restore_removes_its_downloaded_bundle_too() {
        let d = tempfile::tempdir().unwrap();
        // The target never finished a full listing, so planning refuses.
        let (db, dest, target, req) = published(d.path()).await;
        let cache = d.path().join("cache");

        let err = restore_from(db, target, &dest, &req, &cache, || {}).await.unwrap_err();
        assert_eq!(err, "TARGET_NOT_SYNCED_YET");
        assert!(cached_zips(&cache).is_empty(), "{:?}", cached_zips(&cache));
    }

    #[tokio::test]
    async fn a_plan_with_no_replace_makes_no_safety_backup() {
        let d = tempfile::tempdir().unwrap();
        let (db, acct, bundle, _) = replace_scenario(d.path());
        let req = Req {
            destination_id: "d".into(),
            slug: "s".into(),
            stamp: "2026-10-23T09-30-00Z".into(),
            source_uuids: vec![U1.to_string()],
            target_account: ACCT.to_string(),
            placement: Placement::Original,
            existing: ExistingChoice::KeepBoth,
        };
        let plan = plan_restore(&db, &acct, &bundle, &req).unwrap();
        let dest = LocalDest { root: d.path().join("dest") };

        let report = safety_backup_then_apply(db, acct, &dest, plan, bundle, "b".into(), &d.path().join("work"), || {})
            .await
            .unwrap();
        assert_eq!((report.created, report.replaced), (1, 0));
        assert!(dest.list_slugs().await.unwrap().is_empty(), "Keep both destroys nothing, so nothing is backed up");
    }
}
