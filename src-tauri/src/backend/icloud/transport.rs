//! iCloud `Transport` + `MetadataSidecar`.
//!
//! Notes are writable as of M2 — `save`, `delete` and `move_note` all go
//! through `records/modify` with `recordChangeTag` as a real optimistic lock.
//! Folders and sidecars are not, and [`unsupported`] says why at each one
//! rather than naming a milestone: neither is a gap waiting on code (M2 spec,
//! Component O).

use super::{unsupported, wire, ICloudVertical};
use crate::backend::{
    ChangeKind, ChangeSet, Identity, MetadataSidecar, RemoteChange, RemoteFolder,
    RemoteNoteVersion, SaveOp, SaveOutcome, SidecarKind, SidecarRecord, SyncCursor, Transport,
    TransportError,
};
use async_trait::async_trait;

#[async_trait]
impl Transport for ICloudVertical {
    /// One page of `changes/zone`, reported honestly.
    ///
    /// **The sync worker does not drive this** (design decision 7): refresh on
    /// this backend is the ⟳ button and sign-in indexing, the same slice
    /// Microsoft's M1 shipped. It is implemented anyway rather than stubbed,
    /// because `changes/zone` *is* the incremental endpoint and writing it now
    /// is what makes the `syncToken` semantics — including the rejection below
    /// — visible and tested before anything depends on them.
    ///
    /// **A rejected token means refetch, not failure**, and that rule is in M1
    /// on correctness grounds: the rejection arrives inside an HTTP 200
    /// (`wire::ZoneReply::TokenRejected`), a merely old token still syncs, and
    /// an implementation that treats a rejection as an error fails rarely and
    /// confusingly. Here that means returning a from-scratch page with a fresh
    /// cursor — the caller sees a complete change list, not an error it would
    /// have to know how to recover from.
    ///
    /// The cursor is opaque bytes to the core, which is exactly what
    /// `SyncCursor` is. Persisting it to `accounts.sync_cursor` is M2's,
    /// alongside the worker integration that would consume it.
    async fn changes_since(&self, cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        let http = reqwest::Client::new();
        let host = self.ck_hostname();
        let mut token: Option<String> = cursor
            .and_then(|c| String::from_utf8(c.0.clone()).ok())
            .filter(|t| !t.is_empty());

        // At most two attempts: the token we were given, then none. A
        // from-scratch read that is also refused is a real failure.
        for attempt in 0..2 {
            let header = self.cookie_header(&host, "/database/1").await?;
            let reply = wire::fetch_zone_page(
                &http,
                &self.session.ck_host,
                &self.session.dsid,
                &self.session.client,
                &header,
                token.as_deref(),
            )
            .await?;

            let page = match reply {
                wire::ZoneReply::Page(p) => p,
                wire::ZoneReply::TokenRejected(reason) => {
                    if attempt == 1 || token.is_none() {
                        return Err(TransportError::Permanent {
                            source: anyhow::anyhow!(
                                "CloudKit refused a from-scratch zone read ({reason})"
                            ),
                        });
                    }
                    crate::log!(
                        "icloud: sync token rejected ({reason}) — reporting a full change set instead"
                    );
                    token = None;
                    continue;
                }
            };

            let folder_paths = wire::build_folder_paths(&wire::folder_records(&page.records));
            let changes = page
                .records
                .iter()
                .filter(|r| r["recordType"] == serde_json::json!("Note"))
                .filter_map(|r| {
                    let id = r["recordName"].as_str()?.to_string();
                    // A tombstone and a live note arrive in the same array and
                    // are told apart only by the field. Reading everything as
                    // Upserted would resurrect deleted notes on every sync.
                    let (kind, folder_hint) = match wire::decode_note(r, &folder_paths) {
                        wire::Decoded::Note(n) => (ChangeKind::Upserted, Some(n.label.clone())),
                        // A note moved to Recently Deleted has left every
                        // listing, which is what a change feed's consumer needs
                        // to know. That it is recoverable matters to the trash
                        // view, not to a detector deciding whether the cached
                        // read is stale.
                        wire::Decoded::Trashed(_)
                        | wire::Decoded::Skipped { reason: wire::SkipReason::Deleted, .. } => {
                            (ChangeKind::Deleted, None)
                        }
                        // Undecodable or structurally broken: the record
                        // exists and changed, so it is an upsert the caller
                        // will fail to read — not a deletion. Claiming Deleted
                        // here would remove a note that is still in iCloud.
                        wire::Decoded::Skipped { .. } => (ChangeKind::Upserted, None),
                    };
                    Some(RemoteChange { remote_id: id, kind, folder_hint })
                })
                .collect();

            return Ok(ChangeSet {
                changes,
                next_cursor: SyncCursor(page.sync_token.unwrap_or_default().into_bytes()),
                more: page.more_coming,
            });
        }

        unreachable!("the loop returns or errors on both attempts")
    }

    /// The thin `Transport` face of the note write; `NoteStore::save_note_full`
    /// is where the work is, and is what `push_one_dirty` actually calls.
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = crate::backend::NoteStore::save_note_full(self, &op, &[]).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }

    /// Files the note in Apple's own Recently Deleted rather than tombstoning
    /// it — see [`wire::delete_note_body`] for why that is a choice and not the
    /// only mechanism.
    ///
    /// The `recordChangeTag` goes along: a delete is a write like any other,
    /// and racing it against an edit made on the phone should be refused by the
    /// server rather than silently winning.
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        let base = self.scan().await?.bases.get(remote_id).cloned();
        // **A password-protected note cannot be deleted through this path,
        // and saying so up front is the whole fix.** It is a
        // `PasswordProtectedNote` record, not a `Note`, and
        // `wire::delete_note_body` builds a `Note`-shaped update —
        // CloudKit answers HTTP 400, every time, forever. Measured live
        // 2026-08-27: two locked notes, 491 identical requests over 3.5
        // hours before the worker learned to stop.
        //
        // The refusal keys on the record TYPE, exactly as the content write's
        // does (Component H3's amendment): a guard that instead tried to send
        // `"recordType": "PasswordProtectedNote"` would be an unmeasured
        // write against a record whose body Jodd cannot even read, which is
        // precisely the guess this backend's doctrine forbids. If deleting a
        // locked note is ever wanted, it needs a capture of what Apple's own
        // client sends — not a shape inferred from this one.
        if base.as_ref().is_some_and(|b| b.locked) {
            return Err(TransportError::Permanent {
                source: anyhow::anyhow!(
                    "this note is password-protected in Apple Notes — Jodd cannot delete it; \
                     delete it in Apple Notes instead"
                ),
            });
        }
        let tag = base.map(|b| b.change_tag);
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.modify(&wire::delete_note_body(remote_id, tag.as_deref(), now_ms)).await?;
        // Folded, not dropped: the note is in the Trash now, so it must leave
        // every listing — or the 2500 ms folder sweep renders a note the user
        // just deleted — AND appear in the trash view, which `has_trash: true`
        // means the user can actually open.
        self.trash_note_in_cache(remote_id).await;
        Ok(())
    }

    /// The real folder tree, from the instance scan.
    ///
    /// Unlike Microsoft (gotcha #12, where the tree is unenumerable and nesting
    /// is unrecoverable), `Folder` records carry a `ParentFolder` reference —
    /// so this is a genuine `Notes/A/B` tree and every existing subtree query
    /// works untouched. Trash is excluded, consistent with `has_trash: false`.
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        Ok(self.scan().await?.folders.clone())
    }

    /// `ensure_folder` stays unsupported — nothing on this backend calls it.
    /// `restore_note`'s `UntrashThenMove` branch is the one caller (lib.rs),
    /// and `restore_kind(ICloud)` is `MoveOutOfTrash`, which never reaches
    /// it. Leaving it refused, rather than duplicating `create_folder`'s
    /// logic behind a second entry point nothing exercises, keeps there
    /// being one tested path to a folder create.
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        Err(unsupported(&format!(
            "creating folder {path:?} is not available — make it in Apple Notes"
        )))
    }

    /// Folder create — measured live 2026-08-24, confirmed on the Mac,
    /// icloud.com and the iPhone: a folder created this way is a folder
    /// Apple Notes displays, in the right place.
    ///
    /// `push_one_folder` (lib.rs) calls this with the FULL PATH, not a bare
    /// leaf — Gmail's labels are paths too, so this is the existing
    /// contract, not something added here. The parent is derived by
    /// [`wire::parent_for_path`], on the convention Apple's own client keeps
    /// (Notes.app will not create a folder inside `Notes`): CloudKit itself
    /// enforces none of it, which is why this is a convention and not a
    /// server-side guarantee.
    async fn create_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        let scan = self.scan().await?;
        let parent = wire::parent_for_path(path, &scan.folders).map_err(|m| unsupported(&m))?;
        drop(scan);
        let title = path.rsplit('/').next().unwrap_or(path);
        let record = self.mint();
        self.modify(&wire::create_folder_body(&record, title, parent.as_deref())).await?;
        // The tree changed; the next read has to see it. `Scan` has no
        // surgical "add one folder" update — `folder_paths` is derived from
        // the WHOLE set, and every cached note's `.label` depends on it — so
        // this drops the cache rather than risk it disagreeing with the
        // account it just wrote to. Folder writes are rare; a re-walk is
        // cheap for what it buys.
        self.scans.invalidate().await;
        Ok(RemoteFolder { id: record, path: path.to_string() })
    }

    /// Folder rename — same-parent only, and it refuses rather than guesses
    /// when that is not what it was asked to do.
    ///
    /// `push_one_folder` (lib.rs) reaches this for BOTH a same-parent rename
    /// (`rename_folder` command) and a reparent (`move_folder` command) —
    /// the only signal here is the full new path. A same-parent rename is
    /// measured live: it writes `TitleEncrypted` alone, never `ParentFolder`,
    /// matching what `rename_folder_body` sends. A reparent would need to
    /// write `ParentFolder` on a record that already exists — nothing here
    /// has ever sent that, live or in a unit test — so `move_folder` (lib.rs)
    /// already refuses it before a dirty row can reach this at all. The
    /// check below is the second line of defense: if a future caller ever
    /// slips a reparent through anyway, this refuses it explicitly rather
    /// than mis-place a folder relative to what its `ParentFolder` actually
    /// says, the same discipline gotcha #12/#18 apply per-backend.
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        let (records, _, _) = self.fetch_all_records().await?;
        let Some(record) = records
            .iter()
            .find(|r| r["recordType"] == serde_json::json!("Folder") && r["recordName"] == serde_json::json!(id))
        else {
            return Err(TransportError::NotFound);
        };
        let tag = record["recordChangeTag"].as_str().unwrap_or_default();
        let current_parent = wire::parent_folder_of(record);

        let scan = self.scan().await?;
        let intended_parent =
            wire::parent_for_path(new_name, &scan.folders).map_err(|m| unsupported(&m))?;
        drop(scan);

        if intended_parent != current_parent {
            return Err(unsupported(
                "moving this folder to a different parent isn't available on iCloud yet — \
                 do it in Apple Notes",
            ));
        }

        let title = new_name.rsplit('/').next().unwrap_or(new_name);
        self.modify(&wire::rename_folder_body(id, tag, title)).await?;
        self.scans.invalidate().await;
        Ok(())
    }

    /// Folder delete — measured live 2026-08-24 as the cleanup step of
    /// `icloud_relocation_selftest`'s own scratch folder. `delete_folder`
    /// (lib.rs) already refuses a non-empty folder before this is ever
    /// called, so there is no "what about the notes inside it" question
    /// here — CloudKit's own delete needs none either; a `Folder` record
    /// carries no reference to what is filed under it.
    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        let (records, _, _) = self.fetch_all_records().await?;
        let Some(record) = records
            .iter()
            .find(|r| r["recordType"] == serde_json::json!("Folder") && r["recordName"] == serde_json::json!(id))
        else {
            // Already gone. Deleting something absent is success, not a
            // failure to surface — mirrors push_one_folder's own handling of
            // a NotFound elsewhere in the dirty-folder lifecycle.
            return Ok(());
        };
        let tag = record["recordChangeTag"].as_str().unwrap_or_default();
        self.modify(&wire::delete_folder_body(id, tag)).await?;
        self.scans.invalidate().await;
        Ok(())
    }

    /// Relocates a note by writing its `Folder` reference.
    ///
    /// **The sync worker does not call this** — `save_note_full` carries the
    /// folder in the same `records/modify`, which is what
    /// `SaveSemantics::InPlaceUpdateIncludingMove` declares. It is here for the
    /// callers that relocate a note without rewriting it, and because a
    /// `Transport` that refused to move would be lying about a backend where a
    /// move is one field.
    ///
    /// A move writes the note's own record, so it reports the note's new
    /// version — a `None` would leave `notes.remote_version` holding the tag
    /// from before the move, and the next poll would read a changed record as
    /// someone else's edit and manufacture a conflict copy out of Jodd's own
    /// write (Microsoft fix 5, arriving here for the same reason).
    ///
    /// `add` carries Jodd PATHS, not ids — the same shape `push_one_dirty`
    /// hands Microsoft — so it is resolved against the scan's own tree.
    /// `remove` is meaningless on a single-exclusive folder model and is
    /// ignored, exactly as the destination alone decides where the note lives.
    async fn move_note(
        &self,
        remote_id: &str,
        add: &[String],
        _remove: &[String],
    ) -> Result<Option<RemoteNoteVersion>, TransportError> {
        let Some(label) = add.first() else {
            return Err(unsupported("a move with no destination folder"));
        };
        let scan = self.scan().await?;
        let folder = self.folder_id_for(&scan, label)?;
        let tag = scan.bases.get(remote_id).map(|b| b.change_tag.clone());
        // Searched in both: a restore is a move OUT of the Trash, so the note
        // this is relocating may well be a trashed one.
        let moved = scan
            .notes
            .iter()
            .chain(scan.trashed.iter())
            .find(|n| n.uuid == remote_id)
            .cloned();
        let base = scan.bases.get(remote_id).cloned();
        drop(scan);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let saved = self.modify(&wire::move_note_body(remote_id, tag.as_deref(), &folder, now_ms)).await?;
        let date = saved.modified_ms.and_then(wire::apple_date_from_ms).unwrap_or_default();
        // The note kept its body and its pin and changed folder — fold exactly
        // that, so the cached scan agrees with where the note now is.
        if let (Some(mut note), Some(mut base)) = (moved, base) {
            note.label = label.clone();
            note.version = saved.change_tag.clone();
            if !date.is_empty() {
                note.date = date.clone();
            }
            base.folder_id = folder;
            base.change_tag = saved.change_tag.clone();
            self.cache_note(note, base).await;
        }
        Ok(Some(RemoteNoteVersion { version: saved.change_tag, date }))
    }
}

#[async_trait]
impl MetadataSidecar for ICloudVertical {
    /// **`Ok(None)`, never `Ok(Some(vec![]))`** — and the difference is data
    /// loss.
    ///
    /// The trait's contract is explicit: `None` means the sidecar store is not
    /// initialized, so the caller MUST NOT prune local state. `Some(vec![])`
    /// means "enumerated, and it is empty", which tells the core to prune every
    /// local pin to nothing. iCloud has no sidecar store at all, so `None` is
    /// the truthful answer and the empty vec would silently wipe the user's
    /// Jodd-local pins on the first refresh.
    ///
    /// Apple's own pin is a different thing and is read: `IsPinned` is a real
    /// field on the record, mapped in `wire::decode_note`.
    async fn list_sidecars(
        &self,
        _kind: SidecarKind,
    ) -> Result<Option<Vec<SidecarRecord>>, TransportError> {
        Ok(None)
    }

    /// **Not a gap — a design answer.** The pin on this backend is Apple's
    /// own, on a `Note_UserSpecific` record, and `db::remote_pin_policy(ICloud)`
    /// is `RemoteWins` (I1b): a Jodd-written sidecar pin would be overwritten
    /// by the next pull, i.e. a control that visibly does nothing. The right
    /// implementation writes the per-user record itself, which is a different
    /// record type with its own tombstone hazard (gotcha #23) and its own
    /// measurement, and it is not a sidecar.
    async fn put_sidecar(
        &self,
        _note_uuid: &str,
        _kind: SidecarKind,
        _body: Option<&[u8]>,
        _replace: Option<&str>,
    ) -> Result<(String, Option<RemoteNoteVersion>), TransportError> {
        Err(unsupported("pinning from Jodd is not available — pin the note in Apple Notes"))
    }

    async fn remove_sidecar(
        &self,
        _id: &str,
    ) -> Result<Option<RemoteNoteVersion>, TransportError> {
        Err(unsupported("unpinning from Jodd is not available — unpin the note in Apple Notes"))
    }
}
