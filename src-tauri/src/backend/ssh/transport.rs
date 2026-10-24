//! Trait impls for `SshVertical` — thin delegation to read.rs / write.rs.

use std::collections::HashMap;

use async_trait::async_trait;

use super::{dir_of_label, label_of, SshVertical};
use crate::backend::{
    Attachment, Capabilities, ChangeSet, DedupSummary, MessageIndex, MetadataSidecar, Note, NoteStore, RemoteFolder,
    RemoteNoteVersion, SaveOp, SaveOutcome, SavedNote, SidecarKind, SidecarRecord, SyncCursor, Transport,
    TransportError, TrashedNote, Vertical,
};

#[async_trait]
impl Transport for SshVertical {
    /// No incremental feed (spec A8): full listing is the driver, as LocalFs.
    async fn changes_since(&self, _cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        Ok(ChangeSet { changes: vec![], next_cursor: SyncCursor(Vec::new()), more: false })
    }

    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = self.save_full(&op).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }

    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        self.trash_note(remote_id).await
    }

    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        let mut dirs = self.listing(None).await?.dirs;
        dirs.retain(|d| d != "Notes");
        dirs.sort();
        Ok(std::iter::once("Notes".to_string())
            .chain(dirs)
            .map(|p| RemoteFolder { id: p.clone(), path: p })
            .collect())
    }

    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        self.make_dir(path).await?;
        Ok(RemoteFolder { id: path.to_string(), path: path.to_string() })
    }

    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> {
        self.ensure_folder(name).await
    }

    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        self.rename_folder_dir(id, new_name).await
    }

    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        self.delete_folder_dir(id).await
    }

    /// A bare `mv` changes no content, so there is no new version to report
    /// (the LocalFs contract). The next listing re-points the cached id.
    async fn move_note(&self, remote_id: &str, add: &[String], _remove: &[String]) -> Result<Option<RemoteNoteVersion>, TransportError> {
        if let Some(dest) = add.first() {
            self.move_file(remote_id, dest).await?;
        }
        Ok(None)
    }
}

#[async_trait]
impl NoteStore for SshVertical {
    async fn list_all_notes(&self, cache_by_id: &HashMap<String, Note>) -> Result<(Vec<Note>, DedupSummary), TransportError> {
        let listing = self.listing(None).await?;
        self.notes_from(&listing, cache_by_id).await
    }

    async fn list_notes_in_folder(&self, folder: &str, cache_by_id: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError> {
        let listing = self.listing(Some(&dir_of_label(folder))).await?;
        Ok(self.notes_from(&listing, cache_by_id).await?.0)
    }

    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError> {
        Ok(self
            .listing(None)
            .await?
            .files
            .into_keys()
            .map(|id| MessageIndex { label: label_of(&id), id })
            .collect())
    }

    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError> {
        self.fetch_one(remote_id).await
    }

    async fn save_note_full(&self, op: &SaveOp<'_>, _attachments: &[Attachment]) -> Result<SavedNote, TransportError> {
        self.save_full(op).await
    }

    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError> {
        self.ids_for_uuid(uuid).await
    }

    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError> {
        self.trashed().await
    }

    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError> {
        self.untrash_note(remote_id).await
    }
}

/// The pin is on the note's own file (spec A4, the Microsoft shape). The
/// "sidecar id" is the note's uuid, not its path: a path changes when the
/// note moves (here, on another device, or by a program on the server), and
/// a stored path would then unpin nothing. Every write returns the file's
/// new sha.
#[async_trait]
impl MetadataSidecar for SshVertical {
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Option<Vec<SidecarRecord>>, TransportError> {
        let SidecarKind::Pin = kind;
        self.pinned_records().await.map(Some)
    }

    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, _body: Option<&[u8]>, _replace: Option<&str>) -> Result<(String, Option<RemoteNoteVersion>), TransportError> {
        let SidecarKind::Pin = kind;
        let uuid = super::canon_uuid(note_uuid).ok_or(TransportError::NotFound)?;
        let path = self.ids_for_uuid(&uuid).await?.into_iter().next().ok_or(TransportError::NotFound)?;
        let version = self.set_pin(&path, true).await?;
        Ok((uuid, Some(version)))
    }

    /// Unpins every file carrying the uuid, so a leftover copy cannot keep
    /// the pin alive.
    async fn remove_sidecar(&self, id: &str) -> Result<Option<RemoteNoteVersion>, TransportError> {
        let paths = self.ids_for_uuid(id).await?;
        if paths.is_empty() {
            return Err(TransportError::NotFound);
        }
        let mut version = None;
        for path in paths {
            version = Some(self.set_pin(&path, false).await?);
        }
        Ok(version)
    }
}

impl Vertical for SshVertical {
    fn backend_id(&self) -> &str {
        "ssh"
    }
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::read::tests::vault;
    use crate::backend::{SaveOp, SidecarKind, Vertical};
    use std::collections::HashMap;

    /// Through the trait objects the core actually holds.
    #[tokio::test]
    async fn the_vertical_works_through_its_traits() {
        let (d, v) = vault();
        let v: Box<dyn Vertical> = Box::new(v);
        assert_eq!(v.backend_id(), "ssh");
        let op = SaveOp { title: "T", body_html: "<div>x</div>", existing_remote_id: None, existing_uuid: None, existing_created_date: None, label: "Notes/Work", base_version: None };
        let saved = v.save_note_full(&op, &[]).await.unwrap();
        let (notes, _) = v.list_all_notes(&HashMap::new()).await.unwrap();
        assert_eq!(notes[0].uuid, saved.uuid);
        assert_eq!(v.list_notes_in_folder("Notes/Work", &HashMap::new()).await.unwrap().len(), 1);
        assert!(v.list_notes_in_folder("Notes", &HashMap::new()).await.unwrap().is_empty());
        let folders: Vec<String> = v.list_folders().await.unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(folders, vec!["Notes", "Notes/Work"]);
        assert_eq!(v.list_index().await.unwrap()[0].label, "Notes/Work");
        assert!(v.changes_since(None).await.unwrap().changes.is_empty());

        let (sid, ver) = v.put_sidecar(&saved.uuid, SidecarKind::Pin, None, None).await.unwrap();
        assert_eq!(sid, saved.uuid, "I-6: the sidecar id is the uuid, which survives a move");
        assert!(ver.is_some());
        let listed = v.list_sidecars(SidecarKind::Pin).await.unwrap().unwrap();
        assert_eq!((listed.len(), listed[0].id.as_str()), (1, saved.uuid.as_str()));
        // Moved on the server (or by another device) after it was pinned.
        std::fs::create_dir_all(d.path().join("Notes/Moved")).unwrap();
        std::fs::rename(d.path().join(&saved.id), d.path().join("Notes/Moved/t.md")).unwrap();
        v.remove_sidecar(&sid).await.unwrap();
        assert!(std::fs::read_to_string(d.path().join("Notes/Moved/t.md")).unwrap().contains("pinned: false"));
        assert_eq!(v.list_sidecars(SidecarKind::Pin).await.unwrap().unwrap().len(), 0);
        assert!(matches!(v.remove_sidecar("99999999-2222-4333-8444-555555555555").await, Err(crate::backend::TransportError::NotFound)));
        std::fs::rename(d.path().join("Notes/Moved/t.md"), d.path().join(&saved.id)).unwrap();

        v.delete(&saved.id).await.unwrap();
        let trashed = v.list_trashed().await.unwrap();
        v.untrash(&trashed[0].id).await.unwrap();
        assert_eq!(v.fetch_note(&saved.id).await.unwrap().uuid, saved.uuid);
    }
}
