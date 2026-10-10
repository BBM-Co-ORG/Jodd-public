//! The SSH vertical's write path. Every content write is compare-and-swap
//! against the file's sha (spec §3); a folder change is part of the content
//! push, LocalFs-style (spec A5).

use super::file::{self, Ext};
use super::frontmatter::Frontmatter;
use super::scripts::{self, sha256_hex};
use super::{apple_now, apple_to_iso, canon_uuid, dir_of_label, iso_now, parent_dir, slugify, SshVertical, ATTACHMENTS_REFUSAL};
use crate::backend::{RemoteNoteVersion, SaveOp, SavedNote, SidecarKind, SidecarRecord, TransportError};

/// The file currently holding a note.
struct Located {
    path: String,
    sha: String,
    fm: Frontmatter,
}

impl SshVertical {
    async fn read_located(&self, path: &str, uuid: &str) -> Result<Option<Located>, TransportError> {
        let files = self.read_files(&[path.to_string()]).await?;
        let Some(Some(bytes)) = files.get(path) else { return Ok(None) };
        let text = String::from_utf8_lossy(bytes);
        let (fm, _) = super::frontmatter::split(&text);
        let Some(fm) = fm else { return Ok(None) };
        if fm.uuid.as_deref().and_then(canon_uuid).as_deref() != Some(uuid) {
            return Ok(None);
        }
        Ok(Some(Located { path: path.to_string(), sha: sha256_hex(bytes), fm }))
    }

    /// The path is a hint; the uuid is the identity.
    async fn locate(&self, uuid: &str, hint: Option<&str>) -> Result<Option<Located>, TransportError> {
        if let Some(h) = hint.filter(|h| !h.is_empty()) {
            if let Some(l) = self.read_located(h, uuid).await? {
                return Ok(Some(l));
            }
        }
        for path in self.ids_for_uuid(uuid).await? {
            if let Some(l) = self.read_located(&path, uuid).await? {
                return Ok(Some(l));
            }
        }
        Ok(None)
    }

    async fn create_file(&self, dir: &str, stem: &str, ext: Ext, content: &[u8]) -> Result<(String, String), TransportError> {
        let f = self.flavor().await?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let out = self.exec(&scripts::create(&self.root, f, dir, stem, ext.as_str(), content, &sha256_hex(content), &nonce)).await?;
        scripts::parse_create(&out).ok_or_else(|| TransportError::Transient {
            source: anyhow::anyhow!("create printed no path: {out:?}"),
        })
    }

    async fn remove_if(&self, path: &str, expected: &str) -> Result<(), TransportError> {
        let f = self.flavor().await?;
        self.exec(&scripts::remove_if(&self.root, f, path, expected)).await.map(|_| ())
    }

    pub(crate) async fn save_full(&self, op: &SaveOp<'_>) -> Result<SavedNote, TransportError> {
        if !crate::mime822::referenced_cids(op.body_html).is_empty() {
            return Err(TransportError::Permanent { source: anyhow::anyhow!(ATTACHMENTS_REFUSAL) });
        }
        let uuid = op.existing_uuid.and_then(canon_uuid).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let located = self.locate(&uuid, op.existing_remote_id).await?;

        let mut fm = located.as_ref().map(|l| l.fm.clone()).unwrap_or_default();
        fm.uuid = Some(uuid.clone());
        fm.title = Some(op.title.to_string());
        if fm.created.is_none() {
            fm.created = Some(op.existing_created_date.and_then(apple_to_iso).unwrap_or_else(iso_now));
        }
        let (ext, content) = file::encode(&fm, op.title, op.body_html);
        let dir = dir_of_label(op.label);
        let in_target = |cur: &Located| parent_dir(&cur.path) == dir && Ext::of_path(&cur.path) == Some(ext);

        // Before the conflict check: a retry whose previous attempt landed
        // but whose reply was lost finds exactly this content already in
        // place, and must succeed rather than conflict with itself.
        if let Some(cur) = located.as_ref().filter(|c| in_target(c) && c.sha == sha256_hex(content.as_bytes())) {
            return Ok(SavedNote { id: cur.path.clone(), version: cur.sha.clone(), uuid, date: apple_now(), body_html: op.body_html.to_string(), local_version: 0 });
        }
        if let (Some(cur), Some(base)) = (&located, op.base_version.filter(|b| !b.is_empty())) {
            if cur.sha != base {
                return Err(TransportError::Conflict { remote_etag: Some(cur.sha.clone()) });
            }
        }

        let (path, sha) = match located {
            Some(cur) if in_target(&cur) => {
                let sha = self.cas_write(&cur.path, &cur.sha, content.as_bytes()).await?;
                (cur.path, sha)
            }
            Some(cur) => {
                // New file first, then retire the old one: a failure between
                // the two leaves a dup that dedup collapses, never a lost note.
                // Once the new file exists the save HAS happened, so no
                // failure to retire the old one may fail it — a retry would
                // create yet another new file. `trash_note` sweeps every file
                // carrying the uuid, so the leftover cannot outlive a delete.
                let created = self.create_file(&dir, &file::file_stem(&cur.path), ext, content.as_bytes()).await?;
                if let Err(e) = self.remove_if(&cur.path, &cur.sha).await {
                    crate::log!("ssh: left {} in place after moving note {uuid}: {e}", cur.path);
                }
                created
            }
            None => self.create_file(&dir, &slugify(op.title), ext, content.as_bytes()).await?,
        };
        Ok(SavedNote { id: path, version: sha, uuid, date: apple_now(), body_html: op.body_html.to_string(), local_version: 0 })
    }

    /// Trashes `remote_id` and every other file carrying its uuid: a copy
    /// left behind by an interrupted relocation (or by a program) would
    /// otherwise bring the deleted note back on the next listing.
    pub(crate) async fn trash_note(&self, remote_id: &str) -> Result<(), TransportError> {
        let headers = self.headers().await?;
        let uuid = headers.iter().find(|h| h.path == remote_id).and_then(|h| h.uuid.as_deref()).and_then(canon_uuid);
        let mut paths: Vec<&str> = match uuid.as_deref() {
            Some(u) => headers.iter().filter(|h| h.uuid.as_deref().and_then(canon_uuid).as_deref() == Some(u)).map(|h| h.path.as_str()).collect(),
            None => Vec::new(),
        };
        if !paths.contains(&remote_id) {
            paths.push(remote_id);
        }
        for p in paths {
            self.exec(&scripts::trash(&self.root, p, &super::trash_name(p))).await?;
        }
        Ok(())
    }

    pub(crate) async fn untrash_note(&self, trash_id: &str) -> Result<(), TransportError> {
        let name = trash_id.strip_prefix(".jodd/trash/").ok_or(TransportError::NotFound)?;
        let dest = super::trash_original(name);
        self.exec(&scripts::untrash(&self.root, name, &dest)).await.map(|_| ())
    }

    pub(crate) async fn move_file(&self, remote_id: &str, dest_label: &str) -> Result<String, TransportError> {
        Ok(self.exec(&scripts::move_to(&self.root, remote_id, &dir_of_label(dest_label))).await?.trim().to_string())
    }

    pub(crate) async fn make_dir(&self, label: &str) -> Result<(), TransportError> {
        self.exec(&scripts::mkdir_p(&self.root, &dir_of_label(label))).await.map(|_| ())
    }

    pub(crate) async fn rename_folder_dir(&self, from: &str, to: &str) -> Result<(), TransportError> {
        let (src, dst) = (dir_of_label(from), dir_of_label(to));
        if src == dst {
            return Ok(());
        }
        self.exec(&scripts::rename_dir(&self.root, &src, &dst)).await.map(|_| ())
    }

    /// Moves the directory into `.jodd/trash-dirs/`, never deletes it (see
    /// `scripts::trash_dir`). Never the vault's Notes root — an empty or
    /// malformed id resolves there.
    pub(crate) async fn delete_folder_dir(&self, label: &str) -> Result<(), TransportError> {
        let dir = dir_of_label(label);
        if dir == "Notes" {
            return Err(TransportError::Permanent { source: anyhow::anyhow!("refusing to delete vault notes root") });
        }
        self.exec(&scripts::trash_dir(&self.root, &dir, &super::trash_name(&dir))).await.map(|_| ())
    }

    /// Rewrites only the frontmatter, under CAS against the bytes just read.
    pub(crate) async fn set_pin(&self, path: &str, pinned: bool) -> Result<RemoteNoteVersion, TransportError> {
        let files = self.read_files(&[path.to_string()]).await?;
        let Some(Some(bytes)) = files.get(path) else { return Err(TransportError::NotFound) };
        let content = file::with_frontmatter(bytes, |fm| fm.pinned = pinned);
        let sha = self.cas_write(path, &sha256_hex(bytes), &content).await?;
        Ok(RemoteNoteVersion { version: sha, date: apple_now() })
    }

    pub(crate) async fn pinned_records(&self) -> Result<Vec<SidecarRecord>, TransportError> {
        Ok(self
            .headers()
            .await?
            .into_iter()
            .filter(|h| h.pinned)
            .filter_map(|h| {
                let uuid = h.uuid.as_deref().and_then(canon_uuid)?;
                Some(SidecarRecord { id: uuid.clone(), note_uuid: uuid, kind: SidecarKind::Pin, body: None })
            })
            .collect())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::read::tests::vault;
    use super::super::*;
    use crate::backend::{NoteStore, SaveOp, TransportError};

    fn op<'a>(title: &'a str, body: &'a str, label: &'a str, id: Option<&'a str>, uuid: Option<&'a str>, base: Option<&'a str>) -> SaveOp<'a> {
        SaveOp { title, body_html: body, existing_remote_id: id, existing_uuid: uuid, existing_created_date: None, label, base_version: base }
    }

    const TODO: &str = "<div class=\"jodd-task\"><input type=\"checkbox\" contenteditable=\"false\">&nbsp;a</div>";

    #[tokio::test]
    async fn create_then_update_in_place() {
        let (d, v) = vault();
        let a = v.save_full(&op("Meeting notes", TODO, "Notes/Work", None, None, None)).await.unwrap();
        assert_eq!(a.id, "Notes/Work/meeting-notes.md");
        assert_eq!(a.uuid, a.uuid.to_lowercase());
        let text = std::fs::read_to_string(d.path().join(&a.id)).unwrap();
        assert!(text.contains("# Meeting notes\n\n- [ ] a\n"), "{text}");
        assert_eq!(a.version, scripts::sha256_hex(text.as_bytes()));

        let b = v.save_full(&op("Renamed title", "<div>b</div>", "Notes/Work", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!((b.id.as_str(), b.uuid.as_str()), (a.id.as_str(), a.uuid.as_str()), "a title change never renames the file");
        assert!(std::fs::read_to_string(d.path().join(&b.id)).unwrap().contains("# Renamed title"));
    }

    /// The accepted-race case the spec builds on: somebody else wrote the
    /// file after Jodd last synced it.
    #[tokio::test]
    async fn a_stale_base_version_is_a_conflict_and_writes_nothing() {
        let (d, v) = vault();
        let a = v.save_full(&op("T", "<div>x</div>", "Notes", None, None, None)).await.unwrap();
        let path = d.path().join(&a.id);
        let edited = std::fs::read_to_string(&path).unwrap().replace("x", "edited on the server");
        std::fs::write(&path, &edited).unwrap();
        let err = v.save_full(&op("T", "<div>mine</div>", "Notes", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap_err();
        assert!(matches!(err, TransportError::Conflict { .. }), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    }

    #[tokio::test]
    async fn same_title_twice_gets_a_numbered_file() {
        let (_d, v) = vault();
        let a = v.save_full(&op("Same", "<div>1</div>", "Notes", None, None, None)).await.unwrap();
        let b = v.save_full(&op("Same", "<div>2</div>", "Notes", None, None, None)).await.unwrap();
        assert_eq!((a.id.as_str(), b.id.as_str()), ("Notes/same.md", "Notes/same-2.md"));
    }

    #[tokio::test]
    async fn switching_format_writes_new_then_retires_old() {
        let (d, v) = vault();
        let a = v.save_full(&op("Fmt", "<div>plain</div>", "Notes", None, None, None)).await.unwrap();
        let b = v.save_full(&op("Fmt", "<div><u>under</u></div>", "Notes", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!(b.id, "Notes/fmt.html");
        assert!(!d.path().join("Notes/fmt.md").exists());
        let c = v.save_full(&op("Fmt", "<div>plain again</div>", "Notes", Some(&b.id), Some(&b.uuid), Some(&b.version))).await.unwrap();
        assert_eq!(c.id, "Notes/fmt.md");
        assert!(!d.path().join("Notes/fmt.html").exists());
    }

    /// A folder change is a content push on this backend (A5).
    #[tokio::test]
    async fn a_label_change_moves_the_file_and_keeps_its_name() {
        let (d, v) = vault();
        let a = v.save_full(&op("Mv", "<div>x</div>", "Notes/A", None, None, None)).await.unwrap();
        let b = v.save_full(&op("Mv", "<div>x</div>", "Notes/B", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!(b.id, "Notes/B/mv.md");
        assert!(!d.path().join("Notes/A/mv.md").exists());
    }

    /// A program on the server moved the file; the uuid still finds it.
    #[tokio::test]
    async fn a_file_moved_by_someone_else_is_found_by_uuid() {
        let (d, v) = vault();
        let a = v.save_full(&op("Found", "<div>x</div>", "Notes", None, None, None)).await.unwrap();
        std::fs::create_dir_all(d.path().join("Notes/Elsewhere")).unwrap();
        std::fs::rename(d.path().join(&a.id), d.path().join("Notes/Elsewhere/renamed.md")).unwrap();
        let b = v.save_full(&op("Found", "<div>y</div>", "Notes/Elsewhere", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!(b.id, "Notes/Elsewhere/renamed.md");
    }

    /// I-2(a): once the new file landed, failing to retire the old one must
    /// not fail the save — a retry would create a second new file.
    #[tokio::test]
    async fn a_relocation_whose_old_file_cannot_be_removed_still_succeeds() {
        let (d, v) = vault();
        let a = v.save_full(&op("Mv", "<div>x</div>", "Notes/A", None, None, None)).await.unwrap();
        let Some(_ro) = super::super::read::tests::ReadOnlyDir::new(&d.path().join("Notes/A")) else { return };
        let b = v.save_full(&op("Mv", "<div>x</div>", "Notes/B", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!(b.id, "Notes/B/mv.md");
        assert!(d.path().join("Notes/B/mv.md").exists());
        assert!(d.path().join("Notes/A/mv.md").exists(), "left for dedup and delete to collapse");
    }

    /// I-2(b): a retry whose first attempt landed (the reply was lost) finds
    /// its own content on disk and succeeds instead of conflicting.
    #[tokio::test]
    async fn a_retry_of_a_save_that_already_landed_is_a_no_op() {
        let (d, v) = vault();
        let a = v.save_full(&op("Same", "<div>x</div>", "Notes", None, None, None)).await.unwrap();
        let before = std::fs::read(d.path().join(&a.id)).unwrap();
        let b = v.save_full(&op("Same", "<div>x</div>", "Notes", Some(&a.id), Some(&a.uuid), Some("stale-version"))).await.unwrap();
        assert_eq!((b.id.as_str(), b.version.as_str(), b.uuid.as_str()), (a.id.as_str(), a.version.as_str(), a.uuid.as_str()));
        assert_eq!(std::fs::read(d.path().join(&a.id)).unwrap(), before);
        assert_eq!(std::fs::read_dir(d.path().join("Notes")).unwrap().count(), 1, "no new file");
        // The relocation case: the move landed, the old path is gone.
        let m = v.save_full(&op("Same", "<div>x</div>", "Notes/B", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        let again = v.save_full(&op("Same", "<div>x</div>", "Notes/B", Some(&a.id), Some(&a.uuid), Some(&a.version))).await.unwrap();
        assert_eq!(again.id, m.id);
        assert_eq!(std::fs::read_dir(d.path().join("Notes/B")).unwrap().count(), 1);
    }

    /// I-3: a leftover copy carrying the same uuid would otherwise bring the
    /// deleted note back on the next listing.
    #[tokio::test]
    async fn deleting_a_note_trashes_every_file_with_its_uuid() {
        let (d, v) = vault();
        let a = v.save_full(&op("Dup", "<div>x</div>", "Notes", None, None, None)).await.unwrap();
        std::fs::create_dir_all(d.path().join("Notes/Other")).unwrap();
        std::fs::copy(d.path().join(&a.id), d.path().join("Notes/Other/copy.md")).unwrap();
        v.trash_note(&a.id).await.unwrap();
        assert_eq!(v.trashed().await.unwrap().len(), 2);
        let (notes, _) = v.list_all_notes(&std::collections::HashMap::new()).await.unwrap();
        assert!(notes.iter().all(|n| n.uuid != a.uuid), "{notes:?}");
    }

    #[tokio::test]
    async fn attachments_are_refused_permanently() {
        let (_d, v) = vault();
        let err = v.save_full(&op("Img", "<div><img src=\"cid:abc\"></div>", "Notes", None, None, None)).await.unwrap_err();
        match err {
            TransportError::Permanent { source } => assert_eq!(source.to_string(), ATTACHMENTS_REFUSAL),
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn pin_rewrites_only_the_frontmatter_and_lists_back() {
        let (d, v) = vault();
        let a = v.save_full(&op("Pin", TODO, "Notes", None, None, None)).await.unwrap();
        let before = std::fs::read_to_string(d.path().join(&a.id)).unwrap();
        let ver = v.set_pin(&a.id, true).await.unwrap();
        let after = std::fs::read_to_string(d.path().join(&a.id)).unwrap();
        assert!(after.contains("pinned: true"));
        assert_eq!(after.split("---\n").nth(2), before.split("---\n").nth(2), "body untouched");
        assert_eq!(ver.version, scripts::sha256_hex(after.as_bytes()));
        let recs = v.pinned_records().await.unwrap();
        assert_eq!((recs.len(), recs[0].note_uuid.as_str(), recs[0].id.as_str()), (1, a.uuid.as_str(), a.uuid.as_str()));
        v.set_pin(&a.id, false).await.unwrap();
        assert!(v.pinned_records().await.unwrap().is_empty());
    }

    /// C-1: a folder on a shared vault can hold files that are not notes.
    #[tokio::test]
    async fn deleting_a_folder_moves_it_to_trash_dirs_with_everything_in_it() {
        let (d, v) = vault();
        let a = v.save_full(&op("Kept", "<div>x</div>", "Notes/Proj", None, None, None)).await.unwrap();
        std::fs::write(d.path().join("Notes/Proj/keep.txt"), "not a note").unwrap();
        v.delete_folder_dir("Notes/Proj").await.unwrap();
        assert!(!d.path().join("Notes/Proj").exists());
        let trashed: Vec<_> = std::fs::read_dir(d.path().join(".jodd/trash-dirs")).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(trashed.len(), 1);
        assert!(trashed[0].file_name().unwrap().to_str().unwrap().ends_with("-Notes%2FProj"), "{trashed:?}");
        assert_eq!(std::fs::read_to_string(trashed[0].join("keep.txt")).unwrap(), "not a note");
        assert!(trashed[0].join(a.id.rsplit('/').next().unwrap()).exists(), "the note survives too");
        let folders: Vec<String> = crate::backend::Transport::list_folders(&v).await.unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(folders, vec!["Notes"]);
    }

    /// I-4: the second trash of the same path must not `mv` over the first.
    #[tokio::test]
    async fn trashing_one_path_twice_keeps_both() {
        let (d, v) = vault();
        let a = v.save_full(&op("Todo", "<div>first</div>", "Notes", None, None, None)).await.unwrap();
        v.trash_note(&a.id).await.unwrap();
        let b = v.save_full(&op("Todo", "<div>second</div>", "Notes", None, None, None)).await.unwrap();
        assert_eq!((a.id.as_str(), b.id.as_str()), ("Notes/todo.md", "Notes/todo.md"));
        v.trash_note(&b.id).await.unwrap();
        let t = v.trashed().await.unwrap();
        assert_eq!(t.len(), 2, "{t:?}");
        assert_ne!(t[0].id, t[1].id);
        assert!(t.iter().all(|x| x.label == "Notes" && x.id.starts_with(".jodd/trash/")));
        let mut uuids: Vec<&str> = t.iter().map(|x| x.uuid.as_str()).collect();
        uuids.sort();
        let mut want = vec![a.uuid.as_str(), b.uuid.as_str()];
        want.sort();
        assert_eq!(uuids, want);
        v.untrash_note(&t[0].id).await.unwrap();
        assert!(d.path().join("Notes/todo.md").exists());
        assert!(matches!(v.untrash_note(&t[1].id).await, Err(TransportError::Conflict { .. })), "A10: restore onto an occupied path");
    }

    #[tokio::test]
    async fn trash_restore_move_and_folders() {
        let (d, v) = vault();
        let a = v.save_full(&op("Bin", "<div>x</div>", "Notes/A", None, None, None)).await.unwrap();
        v.trash_note(&a.id).await.unwrap();
        v.trash_note(&a.id).await.unwrap(); // idempotent
        let t = v.trashed().await.unwrap();
        assert_eq!((t[0].title.as_str(), t[0].label.as_str(), t[0].uuid.as_str()), ("Bin", "Notes/A", a.uuid.as_str()));
        v.untrash_note(&t[0].id).await.unwrap();
        assert!(d.path().join("Notes/A/bin.md").exists());
        assert_eq!(v.move_file("Notes/A/bin.md", "Notes/B").await.unwrap(), "Notes/B/bin.md");

        v.make_dir("Notes/New").await.unwrap();
        v.rename_folder_dir("Notes/New", "Notes/Renamed").await.unwrap();
        v.rename_folder_dir("Notes/New", "Notes/Renamed").await.unwrap(); // already done
        assert!(matches!(v.rename_folder_dir("Notes/Gone", "Notes/Nowhere").await, Err(TransportError::NotFound)));
        v.delete_folder_dir("Notes/Renamed").await.unwrap();
        assert!(!d.path().join("Notes/Renamed").exists());
        assert!(matches!(v.delete_folder_dir("Notes").await, Err(TransportError::Permanent { .. })));
    }
}
