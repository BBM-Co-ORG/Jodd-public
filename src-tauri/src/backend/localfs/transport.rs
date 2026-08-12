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

pub(crate) fn perm(e: std::io::Error) -> TransportError {
    TransportError::Permanent { source: e.into() }
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
    ) -> Result<(), TransportError> {
        let Some(dest_folder) = add.first() else {
            return Ok(());
        };
        let src = self.root.join(remote_id);
        let fname = src.file_name().ok_or(TransportError::NotFound)?.to_owned();
        let dest_dir = self.folder_path(dest_folder);
        std::fs::create_dir_all(&dest_dir).map_err(perm)?;
        std::fs::rename(&src, dest_dir.join(fname)).map_err(perm)?;
        let _ = remove;
        Ok(())
    }
}

// ── NoteStore ────────────────────────────────────────────────────────────────

#[async_trait]
impl NoteStore for LocalFsVertical {
    async fn list_all_notes(
        &self,
        _cache_by_id: &HashMap<String, Note>,
    ) -> Result<(Vec<Note>, DedupSummary), TransportError> {
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
        std::fs::write(&path, raw.as_bytes()).map_err(perm)?;

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

                Some(TrashedNote {
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
    fn tags_path(&self, uuid: &str) -> std::path::PathBuf {
        self.meta_dir().join(format!("{}.tags.json", uuid))
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
        let dir = self.meta_dir();
        if !dir.exists() {
            return Ok(None); // store not initialized
        }
        let suffix = match kind {
            SidecarKind::Pin => ".pin",
            SidecarKind::Tags => ".tags.json",
        };
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
                let body = match kind {
                    SidecarKind::Tags => std::fs::read(e.path()).ok(),
                    SidecarKind::Pin => None,
                };
                let id = e
                    .path()
                    .strip_prefix(&self.root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                out.push(SidecarRecord {
                    id,
                    note_uuid: uuid.to_string(),
                    kind,
                    body,
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
    ) -> Result<String, TransportError> {
        std::fs::create_dir_all(self.meta_dir()).map_err(perm)?;
        let path = match kind {
            SidecarKind::Pin => self.pin_path(note_uuid),
            SidecarKind::Tags => self.tags_path(note_uuid),
        };
        std::fs::write(&path, body.unwrap_or(b"")).map_err(perm)?;
        let id = path
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        Ok(id)
    }

    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError> {
        let p = self.root.join(id);
        if p.exists() {
            std::fs::remove_file(p).map_err(perm)?;
        }
        Ok(())
    }
}
