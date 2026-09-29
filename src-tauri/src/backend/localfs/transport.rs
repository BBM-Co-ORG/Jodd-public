//! Filesystem-backed Transport + NoteStore + MetadataSidecar impls for LocalFsVertical (B3/B4).

use std::collections::HashMap;

use async_trait::async_trait;

use crate::backend::{
    Attachment, ChangeSet, DedupSummary, MessageIndex, MetadataSidecar, Note, NoteStore,
    RemoteFolder, SaveOp, SaveOutcome, SavedNote, SidecarKind, SidecarRecord, SyncCursor,
    Transport, TransportError, TrashedNote,
};

use super::LocalFsVertical;

// ── helpers ──────────────────────────────────────────────────────────────────

/// On Android a vault lives in shared storage under "All files access", which
/// the user can switch off in Settings at any time. That failure is about the
/// account, is fixed by the user, and must not push-block notes (gotcha #14),
/// so it is Transient with the remedy in its message. Everything else keeps
/// the old mapping.
pub const ACCESS_LOST: &str = "Jodd no longer has All files access — turn it on in Settings → Apps → Jodd.";

pub(crate) fn map_io(e: std::io::Error, android: bool) -> TransportError {
    if android && e.kind() == std::io::ErrorKind::PermissionDenied {
        return TransportError::Transient { source: anyhow::anyhow!(ACCESS_LOST) };
    }
    TransportError::Permanent { source: e.into() }
}

pub(crate) fn perm(e: std::io::Error) -> TransportError {
    map_io(e, cfg!(target_os = "android"))
}

/// A directory that exists but cannot be read (e.g. All files access revoked)
/// must fail a listing loudly: an empty `Ok` list would make the caller prune
/// every clean cached note. A missing directory (new vault) is not an error.
pub(crate) fn ensure_readable(dir: &std::path::Path) -> Result<(), TransportError> {
    match std::fs::read_dir(dir) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(perm(e)),
    }
}

/// Write `bytes` to `path` so no reader — Jodd's own scan, or a sync tool such
/// as Syncthing watching the folder — ever sees a half-written file: write a
/// temp file in the SAME directory (same filesystem, so the rename is atomic),
/// then rename it over the target. The temp name starts with `.` and ends in
/// `.tmp`, so `all_eml` never lists it even if a crash strands it, and carries
/// a random suffix so two concurrent writers of one note (`tauri dev` + the
/// installed app, or the app + `jodd-mcp`) never share a temp file.
/// Replacing the inode means a symlinked `.eml` becomes a regular file, and
/// custom modes and xattrs on the old file are not preserved.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_path_for(path)?;
    std::fs::write(&tmp, bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn temp_path_for(path: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name"))?;
    Ok(path.with_file_name(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    )))
}

/// Encode a relative path (e.g. `Notes/subA/subB/<uuid>.eml`) into a flat
/// filename safe for storage in `.trash/`.
///
/// Encoding rules (applied in order so they compose cleanly):
///   1. `%` → `%25`  (must be first so the sentinel isn't double-encoded)
///   2. `/` → `%2F`
///
/// Example: `Notes/subA/U.eml` → `Notes%2FsubA%2FU.eml`
pub(crate) fn trash_encode(rel: &str) -> String {
    rel.replace('%', "%25").replace('/', "%2F")
}

/// Decode a trash filename back to the original relative path.
///
/// Decoding rules (applied in order, reverse of encode):
///   1. `%2F` → `/`
///   2. `%25` → `%`
pub(crate) fn trash_decode(name: &str) -> String {
    name.replace("%2F", "/").replace("%25", "%")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_notes_dir_fails_the_listing_and_a_missing_one_is_empty() {
        use crate::backend::NoteStore;
        use std::os::unix::fs::PermissionsExt;
        let vault = tempfile::tempdir().unwrap();
        let v = crate::backend::localfs::LocalFsVertical::new(vault.path().to_path_buf(), "a".into());
        // No Notes/ yet: a brand-new vault lists empty, in both entry points.
        assert!(v.list_all_notes(&Default::default()).await.unwrap().0.is_empty());
        assert!(v.list_notes_in_folder("Notes", &Default::default()).await.unwrap().is_empty());

        let notes = vault.path().join("Notes");
        std::fs::create_dir(&notes).unwrap();
        std::fs::set_permissions(&notes, std::fs::Permissions::from_mode(0o000)).unwrap();
        let all = v.list_all_notes(&Default::default()).await;
        let one = v.list_notes_in_folder("Notes", &Default::default()).await;
        let root_bypass = std::fs::read_dir(&notes).is_ok();
        std::fs::set_permissions(&notes, std::fs::Permissions::from_mode(0o755)).unwrap();
        if root_bypass { return; } // running as root: chmod does not bite
        assert!(all.is_err());
        assert!(one.is_err());
    }

    #[test]
    fn a_permission_error_on_android_is_transient_and_names_the_remedy() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        match super::map_io(e, true) {
            crate::backend::TransportError::Transient { source } => assert_eq!(source.to_string(), super::ACCESS_LOST),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_io_errors_and_desktop_stay_permanent() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(super::map_io(denied, false), crate::backend::TransportError::Permanent { .. }));
        let other = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(matches!(super::map_io(other, true), crate::backend::TransportError::Permanent { .. }));
    }

    use super::{trash_decode, trash_encode};

    #[test]
    fn trash_encode_decode_roundtrip() {
        let cases = &[
            "Notes/simple/uuid.eml",
            "Notes/subA/subB/uuid.eml",
            "Notes/folder%with%percents/uuid.eml",
            "Notes/a%2Fb/uuid.eml", // already contains the escape sequence
            "Notes.eml",            // no slash
        ];
        for &original in cases {
            let encoded = trash_encode(original);
            // Encoded form must not contain unescaped slashes.
            assert!(
                !encoded.contains('/'),
                "encoded '{}' still contains '/'",
                encoded
            );
            // Round-trip must be lossless.
            assert_eq!(
                trash_decode(&encoded),
                original,
                "round-trip failed for '{}'",
                original
            );
        }
    }

    #[test]
    fn write_atomic_leaves_a_complete_file_and_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("ABC.eml");
        super::write_atomic(&p, b"first").unwrap();
        super::write_atomic(&p, b"second, longer content").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second, longer content");
        let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("ABC.eml")], "no temp file left behind");
    }

    #[test]
    fn each_write_gets_its_own_temp_name() {
        // Two writers of the same note (`tauri dev` + the installed app, or
        // the app + jodd-mcp) must not share one temp file.
        let p = std::path::Path::new("/v/Notes/ABC.eml");
        let a = super::temp_path_for(p).unwrap();
        let b = super::temp_path_for(p).unwrap();
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert_eq!(t.parent(), p.parent(), "same directory, so the rename is atomic");
            let n = t.file_name().unwrap().to_string_lossy().to_string();
            assert!(n.starts_with(".ABC.eml.") && n.ends_with(".tmp"), "{n}");
        }
    }

    #[test]
    fn concurrent_writers_of_one_note_never_fail_or_strand_a_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = std::sync::Arc::new(d.path().join("ABC.eml"));
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for i in 0..200 {
                        super::write_atomic(&p, format!("writer {t} round {i}").as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("a writer failed");
        }
        let body = String::from_utf8(std::fs::read(&*p).unwrap()).unwrap();
        assert!(body.starts_with("writer ") && body.ends_with(" round 199"), "{body}");
        let names: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("ABC.eml")]);
    }

    #[test]
    fn the_temp_name_is_never_listed_as_a_note() {
        // A crash between write and rename leaves `.ABC.eml.<uuid>.tmp` (or the
        // pre-uuid `.ABC.eml.tmp`): the walk must ignore both.
        let d = tempfile::tempdir().unwrap();
        let v = super::super::LocalFsVertical::new(d.path().to_path_buf(), "localfs:t".into());
        std::fs::create_dir_all(v.notes_dir()).unwrap();
        std::fs::write(v.notes_dir().join(".ABC.eml.tmp"), b"partial").unwrap();
        std::fs::write(super::temp_path_for(&v.notes_dir().join("ABC.eml")).unwrap(), b"partial").unwrap();
        assert!(v.all_eml().is_empty(), "{:?}", v.all_eml());
    }
}

impl LocalFsVertical {
    /// Map a Notes-rooted folder label ("Notes" or "Notes/play5") to an on-disk
    /// directory under `root`. The label ALWAYS starts with "Notes".
    pub(crate) fn folder_path(&self, folder: &str) -> std::path::PathBuf {
        let rel = folder
            .strip_prefix("Notes")
            .unwrap_or(folder)
            .trim_start_matches('/');
        if rel.is_empty() {
            self.notes_dir()
        } else {
            self.notes_dir().join(rel)
        }
    }

    /// Read a single .eml file at `path` and return a Note. The `id` field is
    /// set to the path relative to `root` (forward-slash normalized).
    pub(crate) fn read_note_at(&self, path: &std::path::Path) -> Option<Note> {
        let bytes = std::fs::read(path).ok()?;
        let rel_dir = path.parent()?.strip_prefix(&self.root).ok()?;
        let label = {
            let s = rel_dir.to_string_lossy().replace('\\', "/");
            if s.is_empty() {
                "Notes".to_string()
            } else {
                s
            }
        };
        let mut note = super::decode::decode_eml(&bytes, &label).ok()?;
        note.id = path
            .strip_prefix(&self.root)
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        Some(note)
    }

    /// Walk the Notes directory and collect all .eml paths.
    pub(crate) fn all_eml(&self) -> Vec<std::path::PathBuf> {
        walkdir::WalkDir::new(self.notes_dir())
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .map(|x| x == "eml")
                        .unwrap_or(false)
            })
            .map(|e| e.path().to_path_buf())
            .collect()
    }
}

// ── Transport ─────────────────────────────────────────────────────────────────

#[async_trait]
impl Transport for LocalFsVertical {
    /// LocalFS uses a full-scan model (NoteStore::list_all_notes is the driver).
    /// This returns an empty ChangeSet with an inert cursor to satisfy the trait.
    async fn changes_since(
        &self,
        _cursor: Option<&SyncCursor>,
    ) -> Result<ChangeSet, TransportError> {
        Ok(ChangeSet {
            changes: vec![],
            next_cursor: SyncCursor(Vec::new()),
            more: false,
        })
    }

    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = NoteStore::save_note_full(self, &op, &[]).await?;
        Ok(SaveOutcome {
            remote_id: saved.id,
            cursor_hint: None,
        })
    }

    /// Move a note file into the `.trash/` flat directory, preserving the
    /// original relative path in the trash filename via `trash_encode`.
    ///
    /// `remote_id` is the path of the note relative to `root` (e.g.
    /// `Notes/subA/subB/<uuid>.eml`).  The trash filename becomes
    /// `Notes%2FsubA%2FsubB%2F<uuid>.eml` so that `untrash` can recover the
    /// full original path without any extra metadata.
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        let src = self.root.join(remote_id);
        if !src.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(self.trash_dir()).map_err(perm)?;
        let encoded = trash_encode(remote_id);
        std::fs::rename(&src, self.trash_dir().join(encoded)).map_err(perm)
    }

    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        let mut out = vec![RemoteFolder {
            id: "Notes".into(),
            path: "Notes".into(),
        }];
        if !self.notes_dir().exists() {
            return Ok(out);
        }
        for entry in walkdir::WalkDir::new(self.notes_dir())
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() && entry.path() != self.notes_dir() {
                if let Ok(rel) = entry.path().strip_prefix(&self.root) {
                    let path = rel.to_string_lossy().replace('\\', "/");
                    out.push(RemoteFolder {
                        id: path.clone(),
                        path,
                    });
                }
            }
        }
        Ok(out)
    }

    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        std::fs::create_dir_all(self.folder_path(path)).map_err(perm)?;
        Ok(RemoteFolder {
            id: path.to_string(),
            path: path.to_string(),
        })
    }

    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> {
        self.ensure_folder(name).await
    }

    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        let src = self.folder_path(id);
        let dst = self.folder_path(new_name);
        if src == dst {
            return Ok(());
        }
        if !src.exists() {
            // Source already gone. If destination is there (e.g. user renamed in
            // Finder before the worker ran), the rename is effectively done.
            // If both are absent the row is stale — signal NotFound so the caller
            // can drop it rather than retrying forever.
            return if dst.exists() { Ok(()) } else { Err(TransportError::NotFound) };
        }
        std::fs::rename(src, dst).map_err(perm)
    }

    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        let dir = self.folder_path(id);
        // Never delete the notes root itself — an empty or malformed id resolves
        // to notes_dir() and would wipe the entire vault.
        if dir == self.notes_dir() {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!("refusing to delete vault notes root"),
            });
        }
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(perm)?;
        }
        Ok(())
    }

    async fn move_note(
        &self,
        remote_id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<Option<crate::backend::RemoteNoteVersion>, TransportError> {
        let Some(dest_folder) = add.first() else {
            return Ok(None);
        };
        let src = self.root.join(remote_id);
        let fname = src.file_name().ok_or(TransportError::NotFound)?.to_owned();
        let dest_dir = self.folder_path(dest_folder);
        std::fs::create_dir_all(&dest_dir).map_err(perm)?;
        std::fs::rename(&src, dest_dir.join(fname)).map_err(perm)?;
        let _ = remove;
        // This backend's version is the Date header (see `Note::version`'s
        // doc comment), which a bare filesystem rename never touches.
        Ok(None)
    }
}

// ── NoteStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl NoteStore for LocalFsVertical {
    async fn list_all_notes(
        &self,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        ensure_readable(&self.notes_dir())?;
        let notes = self
            .all_eml()
            .iter()
            .filter_map(|p| self.read_note_at(p))
            .collect();
        Ok((notes, DedupSummary::default()))
    }

    async fn list_notes_in_folder(
        &self,
        folder: &str,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<Vec<Note>, TransportError> {
        let dir = self.folder_path(folder);
        ensure_readable(&dir)?;
        if !dir.exists() {
            return Ok(vec![]);
        }
        let notes = walkdir::WalkDir::new(&dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .map(|x| x == "eml")
                        .unwrap_or(false)
            })
            .filter_map(|e| self.read_note_at(e.path()))
            .collect();
        Ok(notes)
    }

    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        Ok(self
            .all_eml()
            .iter()
            .filter_map(|p| {
                let n = self.read_note_at(p)?;
                Some(MessageIndex {
                    id: n.id,
                    label: n.label,
                })
            })
            .collect())
    }

    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        self.read_note_at(&self.root.join(remote_id))
            .ok_or(TransportError::NotFound)
    }

    async fn save_note_full(
        &self,
        op: &SaveOp<'_>,
        attachments: &[Attachment],
    ) -> Result<SavedNote, TransportError> {
        let uuid = op
            .existing_uuid
            .filter(|s| !s.is_empty())
            .and_then(crate::mime822::canonicalize_uuid)
            .unwrap_or_else(|| {
                crate::mime822::format_apple_uuid(uuid::Uuid::new_v4())
            });

        let now = crate::mime822::format_apple_date(chrono::Local::now());
        let created = op
            .existing_created_date
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| now.clone());

        let body_with_title =
            crate::mime822::inject_title_into_body(op.body_html, op.title);
        let cids = crate::mime822::referenced_cids(&body_with_title);
        let used: Vec<crate::mime822::MimeAttachment<'_>> = attachments
            .iter()
            .filter(|a| cids.iter().any(|c| *c == a.content_id))
            .map(|a| crate::mime822::MimeAttachment {
                content_id: &a.content_id,
                mime_type: &a.mime_type,
                filename: a.filename.as_deref(),
                x_apple_part_url: a.x_apple_part_url.as_deref(),
                data: &a.data,
            })
            .collect();

        let raw = crate::mime822::build_note_mime(
            op.title,
            op.body_html,
            &uuid,
            &now,
            &created,
            "local@jodd",
            &used,
        );

        let dir = self.folder_path(op.label);
        std::fs::create_dir_all(&dir).map_err(perm)?;

        // Write the new file FIRST so a write failure can never lose the note.
        let path = dir.join(format!("{}.eml", uuid));
        write_atomic(&path, raw.as_bytes()).map_err(perm)?;

        // Only AFTER the new file is safely on disk, remove the old one (folder change).
        // A failed remove leaves a recoverable orphan, not a lost note (Gmail doctrine).
        if let Some(old_id) = op.existing_remote_id.filter(|s| !s.is_empty()) {
            let old = self.root.join(old_id);
            if old.exists() && old != path {
                let _ = std::fs::remove_file(&old);
            }
        }

        let rel = path
            .strip_prefix(&self.root)
            .map_err(|e| TransportError::Permanent { source: e.into() })?
            .to_string_lossy()
            .replace('\\', "/");

        Ok(SavedNote {
            id: rel,
            version: now.clone(), // same Date header as `date` below — LocalFs's Note::version
            uuid,
            date: now,
            body_html: op.body_html.to_string(),
            local_version: 0,
        })
    }

    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        Ok(self
            .all_eml()
            .iter()
            .filter_map(|p| {
                let n = self.read_note_at(p)?;
                (n.uuid == uuid).then_some(n.id)
            })
            .collect())
    }

    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        let dir = self.trash_dir();
        if !dir.exists() {
            return Ok(vec![]);
        }
        Ok(walkdir::WalkDir::new(&dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_type().is_file()
                    && e.path()
                        .extension()
                        .map(|x| x == "eml")
                        .unwrap_or(false)
            })
            .filter_map(|e| {
                // The trash filename encodes the original relpath via trash_encode.
                // Decode it to recover the original folder.
                let encoded_name = e.file_name().to_string_lossy().into_owned();
                let original_relpath = trash_decode(&encoded_name);

                // Derive label = parent directory of the original relpath.
                let label = std::path::Path::new(&original_relpath)
                    .parent()
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Notes".into());

                let bytes = std::fs::read(e.path()).ok()?;
                // Pass the decoded label so decode_eml sets the note's label field.
                let n = super::decode::decode_eml(&bytes, &label).ok()?;

                // id = path of the trashed file relative to root (.trash/<encoded>)
                let id = e
                    .path()
                    .strip_prefix(&self.root)
                    .ok()?
                    .to_string_lossy()
                    .replace('\\', "/");

                // The trash filename encodes the original relpath, so the
                // folder below is recovered rather than guessed.
                Some(TrashedNote {
                    original_known: true,
                    id,
                    uuid: n.uuid,
                    title: n.title,
                    date: n.date,
                    label,
                })
            })
            .collect())
    }

    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError> {
        let src = self.root.join(remote_id);

        // The trash filename is the percent-encoded original relpath.
        // Decode it to find out where the note originally lived.
        let encoded_name = src
            .file_name()
            .ok_or(TransportError::NotFound)?
            .to_string_lossy()
            .into_owned();
        let original_relpath = trash_decode(&encoded_name);

        let dest = self.root.join(&original_relpath);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(perm)?;
        }
        std::fs::rename(&src, &dest).map_err(perm)
    }
}

// ── MetadataSidecar ──────────────────────────────────────────────────────────

impl LocalFsVertical {
    fn pin_path(&self, uuid: &str) -> std::path::PathBuf {
        self.meta_dir().join(format!("{}.pin", uuid))
    }
}

#[async_trait]
impl MetadataSidecar for LocalFsVertical {
    /// `Ok(None)` = `.meta/` dir does not exist → caller must NOT prune.
    /// `Ok(Some(v))` = enumerated (possibly empty) → safe to prune.
    async fn list_sidecars(
        &self,
        kind: SidecarKind,
    ) -> Result<Option<Vec<SidecarRecord>>, TransportError> {
        let SidecarKind::Pin = kind;
        let dir = self.meta_dir();
        if !dir.exists() {
            return Ok(None); // store not initialized
        }
        let suffix = ".pin";
        let mut out = vec![];
        for e in walkdir::WalkDir::new(&dir)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !e.file_type().is_file() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(uuid) = name.strip_suffix(suffix) {
                let id = e
                    .path()
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push(SidecarRecord {
                    id,
                    note_uuid: uuid.to_string(),
                    kind: SidecarKind::Pin,
                    body: None,
                });
            }
        }
        Ok(Some(out))
    }

    async fn put_sidecar(
        &self,
        note_uuid: &str,
        kind: SidecarKind,
        body: Option<&[u8]>,
        _replace: Option<&str>,
    ) -> Result<(String, Option<crate::backend::RemoteNoteVersion>), TransportError> {
        let SidecarKind::Pin = kind;
        std::fs::create_dir_all(self.meta_dir()).map_err(perm)?;
        let path = self.pin_path(note_uuid);
        write_atomic(&path, body.unwrap_or(b"")).map_err(perm)?;
        let id = path
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        // The `.pin` file is a separate sidecar file — this write never
        // touches the note's own `.eml`, so its remote_version is unchanged.
        Ok((id, None))
    }

    async fn remove_sidecar(&self, id: &str) -> Result<Option<crate::backend::RemoteNoteVersion>, TransportError> {
        let p = self.root.join(id);
        if p.exists() {
            std::fs::remove_file(p).map_err(perm)?;
        }
        Ok(None)
    }
}
