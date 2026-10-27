//! Where backup versions live. A destination stores `<stamp>.jodd-backup.zip`
//! and `<stamp>.jodd-backup.json` under `jodd-backups/<account-slug>/`; a
//! version exists only when its `.json` exists, so a crash between the two
//! renames leaves an invisible orphan zip that `sweep_orphans` collects.
//!
//! `sweep_orphans` is not safe to run concurrently with a `publish` to the
//! same slug (it would remove the in-flight `.partial`); callers serialise
//! backups per account.

use super::manifest::Manifest;
use super::naming::parse_stamp;
use async_trait::async_trait;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const ROOT_DIR: &str = "jodd-backups";
const ZIP_SUFFIX: &str = ".jodd-backup.zip";
const MANIFEST_SUFFIX: &str = ".jodd-backup.json";
const PARTIAL_SUFFIX: &str = ".partial";
const PROBE_NAME: &str = ".jodd-probe";

/// One backup version as listed: where it is and what it holds.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub slug: String,
    pub stamp: String,
    pub manifest: Manifest,
}

#[async_trait]
pub trait Destination: Send + Sync {
    /// Names the physical location, so callers can serialise work that must
    /// not overlap on it (a sweep must not run during a publish).
    fn lock_key(&self) -> String;
    /// Account slugs that have a folder here.
    async fn list_slugs(&self) -> Result<Vec<String>, String>;
    /// Complete versions (those with a manifest), newest first.
    async fn list_versions(&self, slug: &str) -> Result<Vec<VersionEntry>, String>;
    /// Copy `zip` in, then write `manifest`; the version appears with the manifest.
    async fn publish(&self, slug: &str, stamp: &str, zip: &Path, manifest: &Manifest) -> Result<(), String>;
    async fn fetch_zip(&self, slug: &str, stamp: &str, to: &Path) -> Result<(), String>;
    /// Removes the manifest first (the version disappears), then the zip.
    /// Deleting a version that is already gone succeeds.
    async fn delete_version(&self, slug: &str, stamp: &str) -> Result<(), String>;
    /// Removes `.partial` files and zips with no manifest; returns how many.
    async fn sweep_orphans(&self, slug: &str) -> Result<usize, String>;
    /// Creates `jodd-backups/`, writes and removes a probe file.
    async fn probe(&self) -> Result<(), String>;
}

/// A folder on this machine (a USB drive, a synced folder, a network mount).
#[derive(Clone, Debug)]
pub struct LocalDest {
    pub root: PathBuf,
}

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("backup task failed: {e}"))?
}

/// A slug names one directory directly under `jodd-backups/`.
fn check_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() || slug == "." || slug == ".." || slug.contains(['/', '\\', '\0']) {
        return Err(format!("invalid backup folder name: {slug:?}"));
    }
    Ok(())
}

fn check_stamp(stamp: &str) -> Result<(), String> {
    parse_stamp(stamp).map(|_| ()).ok_or_else(|| format!("invalid backup stamp: {stamp:?}"))
}

fn zip_name(stamp: &str) -> String {
    format!("{stamp}{ZIP_SUFFIX}")
}

fn manifest_name(stamp: &str) -> String {
    format!("{stamp}{MANIFEST_SUFFIX}")
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> String {
    format!("{what} {}: {e}", path.display())
}

/// Create `<dir>/.<name>.partial`, let `write` fill it, `sync_all` that same
/// write handle (Windows `FlushFileBuffers` needs write access), then rename
/// to `<dir>/<name>`. The partial is removed if anything fails.
fn write_then_rename(
    dir: &Path,
    name: &str,
    write: impl FnOnce(&mut fs::File) -> std::io::Result<()>,
) -> Result<(), String> {
    let partial = dir.join(format!(".{name}{PARTIAL_SUFFIX}"));
    let target = dir.join(name);
    let done = fs::File::create(&partial)
        .and_then(|mut f| {
            write(&mut f)?;
            f.sync_all()
        })
        .map_err(|e| io_err("write", &partial, e))
        .and_then(|()| fs::rename(&partial, &target).map_err(|e| io_err("rename to", &target, e)));
    if done.is_err() {
        let _ = fs::remove_file(&partial);
    }
    done
}

impl LocalDest {
    fn backups_dir(&self) -> PathBuf {
        self.root.join(ROOT_DIR)
    }
}

#[async_trait]
impl Destination for LocalDest {
    fn lock_key(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    async fn list_slugs(&self) -> Result<Vec<String>, String> {
        let dir = self.backups_dir();
        blocking(move || {
            let rd = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                Err(e) => return Err(io_err("read", &dir, e)),
            };
            let mut out = vec![];
            for entry in rd {
                let entry = entry.map_err(|e| io_err("read", &dir, e))?;
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    out.push(entry.file_name().to_string_lossy().to_string());
                }
            }
            out.sort();
            Ok(out)
        })
        .await
    }

    async fn list_versions(&self, slug: &str) -> Result<Vec<VersionEntry>, String> {
        check_slug(slug)?;
        let dir = self.backups_dir().join(slug);
        let slug = slug.to_string();
        blocking(move || {
            let rd = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                Err(e) => return Err(io_err("read", &dir, e)),
            };
            let mut out: Vec<(i64, VersionEntry)> = vec![];
            for entry in rd {
                let entry = entry.map_err(|e| io_err("read", &dir, e))?;
                let name = entry.file_name().to_string_lossy().to_string();
                let Some(stamp) = name.strip_suffix(MANIFEST_SUFFIX) else { continue };
                let Some(ms) = parse_stamp(stamp) else { continue };
                // A manifest we cannot read is not a version we can offer.
                let Ok(bytes) = fs::read(entry.path()) else { continue };
                let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else { continue };
                out.push((ms, VersionEntry { slug: slug.clone(), stamp: stamp.to_string(), manifest }));
            }
            out.sort_by(|a, b| b.0.cmp(&a.0));
            Ok(out.into_iter().map(|(_, v)| v).collect())
        })
        .await
    }

    async fn publish(&self, slug: &str, stamp: &str, zip: &Path, manifest: &Manifest) -> Result<(), String> {
        check_slug(slug)?;
        check_stamp(stamp)?;
        let dir = self.backups_dir().join(slug);
        let zip = zip.to_path_buf();
        let stamp = stamp.to_string();
        let json = serde_json::to_vec_pretty(manifest).map_err(|e| format!("manifest: {e}"))?;
        blocking(move || {
            fs::create_dir_all(&dir).map_err(|e| io_err("create", &dir, e))?;
            let zip_file = zip_name(&stamp);
            write_then_rename(&dir, &zip_file, |out| {
                let mut src = fs::File::open(&zip)?;
                std::io::copy(&mut src, out).map(|_| ())
            })
            .map_err(|e| format!("{e} (from {})", zip.display()))?;
            let manifest_file = manifest_name(&stamp);
            let r = write_then_rename(&dir, &manifest_file, |out| out.write_all(&json));
            if r.is_err() {
                // No manifest means no version; do not leave the zip behind.
                let _ = fs::remove_file(dir.join(&zip_file));
            }
            r
        })
        .await
    }

    async fn fetch_zip(&self, slug: &str, stamp: &str, to: &Path) -> Result<(), String> {
        check_slug(slug)?;
        check_stamp(stamp)?;
        let from = self.backups_dir().join(slug).join(zip_name(stamp));
        let to = to.to_path_buf();
        blocking(move || fs::copy(&from, &to).map(|_| ()).map_err(|e| io_err("copy", &from, e))).await
    }

    async fn delete_version(&self, slug: &str, stamp: &str) -> Result<(), String> {
        check_slug(slug)?;
        check_stamp(stamp)?;
        let dir = self.backups_dir().join(slug);
        let stamp = stamp.to_string();
        blocking(move || {
            for name in [manifest_name(&stamp), zip_name(&stamp)] {
                let p = dir.join(name);
                match fs::remove_file(&p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_err("delete", &p, e)),
                }
            }
            Ok(())
        })
        .await
    }

    async fn sweep_orphans(&self, slug: &str) -> Result<usize, String> {
        check_slug(slug)?;
        let dir = self.backups_dir().join(slug);
        blocking(move || {
            let rd = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(io_err("read", &dir, e)),
            };
            let mut removed = 0;
            for entry in rd {
                let entry = entry.map_err(|e| io_err("read", &dir, e))?;
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let orphan = if name.starts_with('.') && name.ends_with(PARTIAL_SUFFIX) {
                    true
                } else if let Some(stamp) = name.strip_suffix(ZIP_SUFFIX) {
                    parse_stamp(stamp).is_some() && !dir.join(manifest_name(stamp)).exists()
                } else {
                    false
                };
                if orphan {
                    let p = entry.path();
                    fs::remove_file(&p).map_err(|e| io_err("delete", &p, e))?;
                    removed += 1;
                }
            }
            Ok(removed)
        })
        .await
    }

    async fn probe(&self) -> Result<(), String> {
        let dir = self.backups_dir();
        blocking(move || {
            fs::create_dir_all(&dir).map_err(|e| io_err("create", &dir, e))?;
            let p = dir.join(PROBE_NAME);
            fs::write(&p, b"jodd").map_err(|e| io_err("write", &p, e))?;
            fs::remove_file(&p).map_err(|e| io_err("delete", &p, e))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::manifest::{Manifest, FORMAT_VERSION};
    use crate::backup::Trigger;

    fn manifest(created_ms: i64) -> Manifest {
        Manifest {
            jodd_backup: FORMAT_VERSION,
            account_id: "gmail:a@x".into(),
            backend: "gmail".into(),
            root_label: "Notes".into(),
            created_ms,
            app_version: "0.0.0".into(),
            device: "macos".into(),
            trigger: Trigger::Manual,
            keep: false,
            zip_bytes: 3,
            zip_sha256: "abc".into(),
            notes: vec![],
            folders: vec![],
            skipped: vec![],
        }
    }

    const SLUG: &str = "gmail_a@x";
    const MS_A: i64 = 1_792_747_800_000; // 2026-10-23T09-30-00Z
    const MS_B: i64 = MS_A + 3_600_000;

    fn zip_in(dir: &std::path::Path, bytes: &[u8]) -> std::path::PathBuf {
        let p = dir.join("src.zip");
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn dest(root: &std::path::Path) -> LocalDest {
        LocalDest { root: root.to_path_buf() }
    }

    #[tokio::test]
    async fn publish_then_list_shows_version() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let stamp = crate::backup::naming::version_stamp(MS_A);
        let zip = zip_in(work.path(), b"zip");
        dst.publish(SLUG, &stamp, &zip, &manifest(MS_A)).await.unwrap();

        assert_eq!(dst.list_slugs().await.unwrap(), vec![SLUG.to_string()]);
        let v = dst.list_versions(SLUG).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].slug, SLUG);
        assert_eq!(v[0].stamp, stamp);
        assert_eq!(v[0].manifest, manifest(MS_A));

        let out = work.path().join("fetched.zip");
        dst.fetch_zip(SLUG, &stamp, &out).await.unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"zip");

        let names: Vec<String> = std::fs::read_dir(d.path().join("jodd-backups").join(SLUG))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".partial")), "{names:?}");
    }

    #[tokio::test]
    async fn zip_without_manifest_is_invisible_and_swept() {
        let d = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let dir = d.path().join("jodd-backups").join(SLUG);
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = crate::backup::naming::version_stamp(MS_A);
        let z = dir.join(format!("{stamp}.jodd-backup.zip"));
        std::fs::write(&z, b"zip").unwrap();

        assert!(dst.list_versions(SLUG).await.unwrap().is_empty());
        assert_eq!(dst.sweep_orphans(SLUG).await.unwrap(), 1);
        assert!(!z.exists());
    }

    #[tokio::test]
    async fn sweep_removes_partials_and_keeps_complete_versions() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let stamp = crate::backup::naming::version_stamp(MS_A);
        dst.publish(SLUG, &stamp, &zip_in(work.path(), b"zip"), &manifest(MS_A)).await.unwrap();
        let dir = d.path().join("jodd-backups").join(SLUG);
        let partial = dir.join(format!(".{}.jodd-backup.zip.partial", crate::backup::naming::version_stamp(MS_B)));
        std::fs::write(&partial, b"half").unwrap();

        assert_eq!(dst.sweep_orphans(SLUG).await.unwrap(), 1);
        assert!(!partial.exists());
        assert_eq!(dst.list_versions(SLUG).await.unwrap().len(), 1);
        assert_eq!(dst.sweep_orphans(SLUG).await.unwrap(), 0);
        assert_eq!(dst.sweep_orphans("never-backed-up").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn failed_publish_leaves_no_version() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let blocker = d.path().join("jodd-backups");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let stamp = crate::backup::naming::version_stamp(MS_A);

        let r = dst.publish(SLUG, &stamp, &zip_in(work.path(), b"zip"), &manifest(MS_A)).await;
        assert!(r.is_err());

        std::fs::remove_file(&blocker).unwrap();
        std::fs::create_dir(&blocker).unwrap();
        assert!(dst.list_versions(SLUG).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn manifest_write_failure_leaves_no_version_and_no_zip() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let stamp = crate::backup::naming::version_stamp(MS_A);
        let dir = d.path().join("jodd-backups").join(SLUG);
        std::fs::create_dir_all(&dir).unwrap();
        // A directory where the manifest's .partial must go: the zip lands, the manifest cannot.
        std::fs::create_dir(dir.join(format!(".{stamp}.jodd-backup.json.partial"))).unwrap();

        let r = dst.publish(SLUG, &stamp, &zip_in(work.path(), b"zip"), &manifest(MS_A)).await;
        assert!(r.is_err());
        assert!(!dir.join(format!("{stamp}.jodd-backup.zip")).exists());
        assert!(dst.list_versions(SLUG).await.unwrap().is_empty());
        assert_eq!(dst.sweep_orphans(SLUG).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn versions_sort_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        for ms in [MS_A, MS_B, MS_A - 86_400_000] {
            let stamp = crate::backup::naming::version_stamp(ms);
            dst.publish(SLUG, &stamp, &zip_in(work.path(), b"z"), &manifest(ms)).await.unwrap();
        }
        let stamps: Vec<String> = dst.list_versions(SLUG).await.unwrap().into_iter().map(|v| v.stamp).collect();
        let want: Vec<String> = [MS_B, MS_A, MS_A - 86_400_000]
            .iter()
            .map(|m| crate::backup::naming::version_stamp(*m))
            .collect();
        assert_eq!(stamps, want);
    }

    #[tokio::test]
    async fn delete_removes_both_files() {
        let d = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        let stamp = crate::backup::naming::version_stamp(MS_A);
        dst.publish(SLUG, &stamp, &zip_in(work.path(), b"zip"), &manifest(MS_A)).await.unwrap();
        dst.delete_version(SLUG, &stamp).await.unwrap();
        let dir = d.path().join("jodd-backups").join(SLUG);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(dst.list_versions(SLUG).await.unwrap().is_empty());
        // Deleting what is already gone is not an error.
        dst.delete_version(SLUG, &stamp).await.unwrap();
    }

    #[tokio::test]
    async fn slug_and_stamp_cannot_escape_the_backup_folder() {
        let d = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        assert!(dst.list_versions("..").await.is_err());
        assert!(dst.list_versions("a/b").await.is_err());
        assert!(dst.delete_version(SLUG, "../../x").await.is_err());
        assert!(dst.fetch_zip(SLUG, "nope", &d.path().join("o")).await.is_err());
    }

    #[tokio::test]
    async fn probe_creates_folder_and_leaves_nothing_behind() {
        let d = tempfile::tempdir().unwrap();
        let dst = dest(d.path());
        dst.probe().await.unwrap();
        let dir = d.path().join("jodd-backups");
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);

        let f = tempfile::tempdir().unwrap();
        let file = f.path().join("a-file");
        std::fs::write(&file, b"x").unwrap();
        assert!(dest(&file).probe().await.is_err());
    }
}
