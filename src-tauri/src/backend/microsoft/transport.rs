//! Microsoft `Transport` + `MetadataSidecar`. Milestone 1 implements exactly one
//! method for real — `list_folders` — because it is the only route to a folder
//! list Exchange offers. Everything else is a write and waits for Milestone 2.

use super::{milestone_2, wire, MicrosoftVertical};
use crate::backend::{
    ChangeSet, MetadataSidecar, NoteStore, RemoteFolder, SaveOp, SaveOutcome, SidecarKind,
    SidecarRecord, SyncCursor, Transport, TransportError,
};
use async_trait::async_trait;

#[async_trait]
impl Transport for MicrosoftVertical {
    /// No cursor on this backend yet. Gmail returns an inert full-scan
    /// ChangeSet; here the method has no caller at all (the worker is not driven
    /// by it), so an honest error beats a fabricated change list that a future
    /// caller would trust.
    async fn changes_since(&self, _cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError> {
        Err(milestone_2())
    }

    /// Delegates to `NoteStore::save_note_full` with no attachments — the
    /// caller that actually pushes dirty notes (`push_one_dirty`, lib.rs)
    /// calls `save_note_full` directly so it can pass the note's real
    /// attachment list; this trait method has no attachments parameter to
    /// pass one through, so it can only ever offer the empty-list path.
    ///
    /// **`Transport::save` has no caller in the production sync path**
    /// (checked 2026-08-15, whole-branch review) — `push_one_dirty` (lib.rs)
    /// calls `NoteStore::save_note_full` directly, for every backend, so this
    /// method never fires on a real sync tick. But it is NOT dead code: step
    /// [10] of `examples/ms_write_probe.rs` — the M2 spec's own "run the
    /// shipped code against the real API" live-verification harness — calls
    /// `Transport::save` directly, specifically to prove the trait-level
    /// entry point works end to end (create via `save`, confirm via
    /// `fetch_note`, delete via `Transport::delete`), not just the
    /// `save_note_full` path the sync worker actually takes. Stubbing this
    /// with `Err(milestone_2())` the way `changes_since`/`find_ids_for_uuid`
    /// stay stubbed below would break that probe's step [10] — those two
    /// methods have no real caller ANYWHERE (production or probe), which is
    /// the situation their stub reasoning actually describes; this one does,
    /// so it keeps its real delegation. `gmail::transport::save` and
    /// `localfs::transport::save` are equally unreachable from the sync
    /// worker and implement it for real too, so this is also the choice that
    /// keeps Microsoft consistent with both siblings rather than being the
    /// one stub in an otherwise-live trio.
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError> {
        let saved = self.save_note_full(&op, &[]).await?;
        Ok(SaveOutcome { remote_id: saved.id, cursor_hint: None })
    }

    /// Delete a note. **Hard delete with no undo path** — measured
    /// 2026-08-14: after an Apple-side delete, Deleted Items held zero items
    /// and the note was absent from every scan. That is why
    /// `Capabilities::has_trash` is false, and why the UI confirms first
    /// (Task 10) rather than leaning on a trash that does not exist.
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError> {
        wire::delete_message(&self.token, remote_id).await
    }

    /// The Exchange folder list, reconstructed from messages — PLUS any
    /// already-known folder that still exists but currently holds no note.
    ///
    /// `/me/mailFolders` omits the Notes tree entirely and `GET
    /// /mailFolders/{id}` 404s, so the ONLY route to a folder id is
    /// `parentFolderId` on a message and the only route to its name is
    /// `PR_PARENT_DISPLAY` on that message (gotcha #12). One consequence is
    /// permanent and accepted: the tree is flat (no parent/child shape is
    /// recoverable). The other used to be a bug, fixed here (Task 9,
    /// 2026-08-14): deriving the folder list purely from the scan meant an
    /// EMPTIED folder and a DELETED one both looked like "absent from the
    /// listing", so `prune_clean_folders` (lib.rs) could not tell them apart
    /// and dropped both — including a folder Jodd itself just created, the
    /// moment its push succeeded (row goes `clean`, next scan can't see an
    /// empty folder, clean rows are prunable).
    ///
    /// The fix: for every path this instance already has an Exchange id for
    /// (`folder_ids`, from the local `folders` table) that the scan did NOT
    /// report, ask `wire::folder_still_exists` — the 200-vs-404 check on
    /// `/messages` that is the only way to tell emptied from deleted on this
    /// backend — and keep it if it still answers. A folder Jodd has never
    /// seen (no cached id — e.g. created directly on the iPhone with nothing
    /// yet filed into it) is unaffected by this and stays undiscoverable,
    /// same as before: there is no id to probe.
    ///
    /// Costs one extra `/messages?$top=1` request per stale-but-cached folder
    /// id per pull — zero in the common case where every known folder showed
    /// up in the scan.
    ///
    /// Shares the vertical's ONE cached scan with `list_all_notes`. `list_notes`
    /// (lib.rs) calls both on the same instance, so this used to be a second
    /// complete `/me/messages` pagination; now it is free. It also means the
    /// folder list and the notes' labels come from literally the same
    /// `folder_paths` map rather than two that could disagree.
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError> {
        let scan = self.scan().await?;
        let mut folders = wire::to_folders(scan);
        let seen: std::collections::HashSet<&str> = folders.iter().map(|f| f.id.as_str()).collect();

        // Folders Jodd knows about that hold no note right now. Without this
        // check they are simply absent from the scan, so prune_clean_folders
        // reads them as deleted — see the method doc above for the sequence
        // that vanishes a folder created in Jodd the moment its push succeeds.
        let empty_candidates: Vec<(String, String)> = self
            .folder_ids
            .iter()
            .filter(|(_, id)| !seen.contains(id.as_str()))
            .map(|(path, id)| (path.clone(), id.clone()))
            .collect();

        for (path, id) in empty_candidates {
            if wire::folder_still_exists(&self.token, &id).await {
                folders.push(RemoteFolder { id, path });
            }
        }
        Ok(folders)
    }

    /// Returns the cached Exchange id for `path` if the local `folders` table
    /// already knows one; otherwise delegates to [`create_folder`](Self::create_folder).
    ///
    /// `folder_ids` is populated from an earlier scan (see the field's doc
    /// comment on `MicrosoftVertical`), so a hit here costs no request at all
    /// — the common case, since most callers ask about a folder Jodd has
    /// already seen. A miss means either the folder was never scanned or it
    /// does not exist yet; `create_folder` is the only way to find out which,
    /// because this backend cannot list folders to check (gotcha #12).
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError> {
        if let Some(id) = self.folder_ids.get(path) {
            return Ok(RemoteFolder { id: id.clone(), path: path.to_string() });
        }
        self.create_folder(path).await
    }

    /// Create a folder. **Always lands directly under the `Notes` root** —
    /// this backend cannot express any other parent, because Graph cannot
    /// enumerate the Notes tree (gotcha #12): the only folder id Jodd can ever
    /// name coherently as "the parent" is the root's, and even that is
    /// reachable only through a note filed directly in it (`wire::
    /// notes_root_id`). A folder the user thinks of as "inside L1" cannot be
    /// created here — Jodd cannot see L1's nesting under Notes, so it cannot
    /// offer to nest anything under L1 either. That is a permanent limitation
    /// of this backend, not a gap to fill later.
    ///
    /// **Known limitation, not fixed here: sub-folder create silently lands
    /// at the Notes root.** `Sidebar.svelte`'s "New sub-folder" action offers
    /// itself on any folder, and `create_folder` (the Tauri command) accepts
    /// a path like `"Notes/L1/Sub"` as a valid parent without refusing it —
    /// but this vertical, per the paragraph above, always creates directly
    /// under the root regardless of what parent was asked for. So the local
    /// row records `"Notes/L1/Sub"` while the server actually holds a
    /// root-level folder, and the two disagree until the first scan that
    /// sees a note filed in it, at which point the folder relocates to the
    /// root out from under the user with no error. Before this branch the
    /// user got a loud `Invalid parent path` refusal instead; that was
    /// correct behavior this silent-wrong outcome should eventually restore,
    /// but doing so is out of scope here.
    ///
    /// `name` may be a full Jodd path, not a leaf — `push_one_folder` (lib.rs)
    /// passes `CachedFolder.path` verbatim, and gotcha #9 means that is always
    /// `"Notes/<segment>"` for a folder the user just created. Exchange's
    /// `displayName` must be the LEAF only: `wire::folder_paths()` sanitizes
    /// `/` to `-` (a real path separator has no meaning to Exchange), so
    /// sending the whole path as `displayName` would create a folder Graph
    /// names literally `"Notes/Ideas"`, and the next scan would report it
    /// back as `"Notes-Ideas"` — never matching this `"Notes/Ideas"` row
    /// again. The returned `RemoteFolder.path` stays the caller's ORIGINAL
    /// path (not the leaf), so `mark_folder_created` keys the local row
    /// correctly.
    ///
    /// Once a note is actually filed into the new folder, the next scan
    /// reports its Exchange leaf name — but as of the whole-branch review fix
    /// (2026-08-15), `wire::folder_paths` presents that leaf ALREADY rooted
    /// under `"Notes/"` (see `present_under_notes_root`'s doc comment), so the
    /// re-scanned path agrees with this row's `"Notes/Ideas"` instead of
    /// diverging to a bare `"Ideas"`. Before that fix this was a documented,
    /// deliberate asymmetry — the bare-leaf re-scan silently dropped the
    /// prefix, `prune_clean_folders` read the mismatch as "deleted remotely"
    /// and dropped the row, and the next `ensure_workflow_folder` call
    /// re-inserted it and minted a SECOND Exchange folder. Fixing the leaf
    /// vs. path mismatch at its source (this comment's prior text argued
    /// against exactly that, as "a larger, riskier change than this
    /// milestone should carry") is what the whole-branch review ruled for
    /// instead of loosening the four `lib.rs` folder commands.
    ///
    /// If no note is filed directly in `Notes`, the root's id is unreachable
    /// and this refuses outright — a `Permanent` error naming the problem —
    /// rather than guessing a parent. Silent-wrong is the worse failure here:
    /// a folder filed somewhere the user did not ask for is invisible to
    /// them, since Jodd cannot display the nesting to reveal the mistake.
    /// The root id is looked up in `folder_ids` first — the same local-table
    /// cache `ensure_folder` and `move_note` consult, keyed `"Notes"` by
    /// `reconcile_folders_from_vertical` (lib.rs) — before falling back to a
    /// full mailbox scan. Without that check, EVERY create would pay a whole
    /// `/me/messages` pagination (with bodies) purely to re-derive an id
    /// already sitting in SQLite, and the refusal below would fire spuriously
    /// whenever the user has since moved their last root-level note into a
    /// subfolder, even though Jodd is still holding the root's id in memory.
    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError> {
        let root_id = match self.folder_ids.get("Notes") {
            Some(id) => id.clone(),
            None => {
                let scan = self.scan().await?;
                wire::notes_root_id(scan).ok_or_else(|| TransportError::Permanent {
                    source: anyhow::anyhow!(
                        "cannot create folder '{name}': the Notes root has no reachable \
                         Exchange id — no note is filed directly in it, and that is the only \
                         way this backend can discover a folder id (gotcha #12). File a note \
                         directly in Notes, then retry."
                    ),
                })?
            }
        };
        let leaf = name.rsplit('/').next().unwrap_or(name);
        let mut folder = wire::create_child_folder(&self.token, &root_id, leaf).await?;
        folder.path = name.to_string();
        Ok(folder)
    }

    /// `new_name` arrives as a full Jodd folder PATH (e.g. `"Notes/Renamed"`)
    /// — same shape `create_folder` above receives — so the leaf must be
    /// taken here too. Without it, `displayName` would be set to the literal
    /// path string, which Apple displays as-is and the next scan sanitises
    /// into `Notes/Notes-Renamed` (`/` is not valid in a single folder name).
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError> {
        let leaf = new_name.rsplit('/').next().unwrap_or(new_name);
        wire::rename_folder_req(&self.token, id, leaf).await
    }

    async fn delete_folder(&self, id: &str) -> Result<(), TransportError> {
        wire::delete_folder_req(&self.token, id).await
    }

    /// Move a note to the folder named by `add.first()`. `remove` is ignored:
    /// Exchange folder membership is exclusive (a message has exactly one
    /// `parentFolderId`), so filing a note into the destination already
    /// implies removing it from wherever it was — there is no "also remove
    /// from X" step the way Gmail's label-add/label-remove model needs one.
    ///
    /// `add.first()` is a Jodd folder PATH (e.g. `"Notes/Ideas"`), resolved to
    /// an Exchange folder id through `folder_ids` — the same cache
    /// `list_notes_in_folder` and `save_note_full` use, populated from the
    /// local `folders` table. An unresolvable path is a `Permanent` error
    /// naming it, on the same reasoning as `save_note_full`'s folder-id miss:
    /// the caller needs to know WHICH destination Jodd could not place.
    ///
    /// **Returns the resulting `RemoteNoteVersion`, not discarding it.** A
    /// move is `POST /messages/{id}/move`, a real mutation of the note's own
    /// object — `push_one_dirty` (lib.rs) may call this right after a content
    /// PATCH in the same tick, and the move's own response is the only
    /// trustworthy source for the note's version afterward. Discarding it (as
    /// this did before) left the content push's now-stale version cached,
    /// which could read a later untouched poll as a remote conflict.
    async fn move_note(&self, remote_id: &str, add: &[String], _remove: &[String]) -> Result<Option<crate::backend::RemoteNoteVersion>, TransportError> {
        let dest_path = add.first().ok_or_else(|| TransportError::Permanent {
            source: anyhow::anyhow!("move_note: no destination folder given for note '{remote_id}'"),
        })?;
        let folder_id = self.folder_ids.get(dest_path).ok_or_else(|| TransportError::Permanent {
            source: anyhow::anyhow!(
                "no Exchange folder id for '{dest_path}' — the folder must exist before a note \
                 can be moved into it"
            ),
        })?;
        let msg = wire::move_note_to_folder(&self.token, remote_id, folder_id).await?;
        Ok(Some(wire::graph_message_to_remote_version(&msg)))
    }
}

#[async_trait]
impl MetadataSidecar for MicrosoftVertical {
    /// A real paginated scan, per the M4 design spec's Component E —
    /// `Ok(Some(vec))`, always, never `Ok(None)`. Unlike Gmail's meta-label
    /// sidecar there is no separate store that can be "not initialized yet";
    /// the property either exists on a note or it doesn't, so an account
    /// with zero pinned notes correctly reports `Ok(Some(vec![]))`, which
    /// `sync_pin_state` (lib.rs) already treats as "prune every local pin".
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Option<Vec<SidecarRecord>>, TransportError> {
        let SidecarKind::Pin = kind;
        wire::list_pinned(&self.token).await.map(Some)
    }

    /// Resolves `note_uuid` to a message id and `PATCH`es
    /// [`wire::JODD_PIN_PROP`] — see `wire::put_pin`. `body` is forwarded
    /// as-is (the trait's caller, `push_one_pin` in lib.rs, always sends
    /// `{"pinned":true}` when this is reached — the `pinned:false` case goes
    /// through `remove_sidecar` instead); `None` falls back to the same
    /// value so a direct trait call with no body still writes something
    /// sensible rather than an empty property.
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, _replace: Option<&str>) -> Result<(String, Option<crate::backend::RemoteNoteVersion>), TransportError> {
        let SidecarKind::Pin = kind;
        let value = match body {
            Some(b) => String::from_utf8_lossy(b).into_owned(),
            None => wire::pin_property_value(true),
        };
        let (id, version) = wire::put_pin(&self.token, note_uuid, &value).await?;
        Ok((id, Some(version)))
    }

    /// Writes `pinned:false` rather than deleting the property — see
    /// `wire::remove_pin`'s doc comment for why that is the safer
    /// equivalent, not a shortcut.
    async fn remove_sidecar(&self, id: &str) -> Result<Option<crate::backend::RemoteNoteVersion>, TransportError> {
        wire::remove_pin(&self.token, id).await.map(Some)
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn v() -> MicrosoftVertical {
        MicrosoftVertical::new("tok".into(), "acct".into(), Default::default())
    }

    /// Proves the two-request SEQUENCE, not just that each half compiles —
    /// the M4 design spec's Verification table calls this out explicitly,
    /// on the precedent of
    /// `save_note_full_dispatches_create_and_patch_to_the_right_endpoint_and_method`.
    ///
    /// Also proves the version-staleness fix: the PATCH response must decode
    /// to a real `GraphMessage` (not the resolve step's list shape), and its
    /// `lastModifiedDateTime` must come back as `Some(RemoteNoteVersion)`
    /// rather than being discarded — see `wire::patch_pin_property`'s doc
    /// comment for why silently dropping it desyncs `notes.remote_version`.
    #[tokio::test]
    async fn put_sidecar_pin_resolves_the_uuid_then_patches_the_named_property() {
        let (addr, seen) = server_routing_with_body(&[
            ("/me/messages?", 200, r#"{"value":[{"id":"MSG-ID"}]}"#),
            (
                "/me/messages/MSG-ID",
                200,
                r#"{"id":"MSG-ID","internetMessageId":"<note-uuid>","lastModifiedDateTime":"2026-08-16T10:00:00Z"}"#,
            ),
        ])
        .await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let (id, version) = v()
            .put_sidecar("note-uuid", SidecarKind::Pin, Some(br#"{"pinned":true}"#), None)
            .await
            .expect("put_sidecar(Pin) must succeed");
        assert_eq!(id, "note-uuid", "the returned id IS the uuid — no second object exists on this backend");
        let version = version.expect("a property PATCH on the note's own message must report its new version");
        assert_eq!(
            version.version, "2026-08-16T10:00:00Z",
            "must come from the PATCH response, not be silently unchanged"
        );

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "must resolve THEN patch, one request each; saw {calls:?}");

        let (resolve_line, _) = &calls[0];
        assert!(resolve_line.starts_with("GET /me/messages?"), "got: {resolve_line}");
        assert!(
            resolve_line.contains("internetMessageId"),
            "must resolve via a $filter on internetMessageId: {resolve_line}"
        );

        let (patch_line, patch_body) = &calls[1];
        assert!(patch_line.starts_with("PATCH /me/messages/MSG-ID"), "got: {patch_line}");
        let sent: serde_json::Value = serde_json::from_str(patch_body).unwrap();
        let props = sent["singleValueExtendedProperties"].as_array().unwrap();
        assert_eq!(props[0]["id"], wire::JODD_PIN_PROP);
        assert_eq!(props[0]["value"], r#"{"pinned":true}"#, "the caller's body is forwarded as-is: {patch_body}");
    }

    /// Component C: unpin must WRITE `pinned:false`, not delete the property
    /// and not silently no-op — `list_sidecars(Pin)` reads whatever value is
    /// stored, so anything else would let an unpinned note re-pin itself on
    /// the next cold start. Also proves unpin reports its new version too —
    /// the same PATCH-to-the-note's-own-object shape as pin.
    #[tokio::test]
    async fn remove_sidecar_writes_pinned_false_not_a_delete_or_a_noop() {
        let (addr, seen) = server_routing_with_body(&[
            ("/me/messages?", 200, r#"{"value":[{"id":"MSG-ID"}]}"#),
            (
                "/me/messages/MSG-ID",
                200,
                r#"{"id":"MSG-ID","internetMessageId":"<note-uuid>","lastModifiedDateTime":"2026-08-16T10:05:00Z"}"#,
            ),
        ])
        .await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let version = v().remove_sidecar("note-uuid").await.expect("remove_sidecar must succeed");
        assert!(version.is_some(), "unpinning also PATCHes the note's own message, so its version changes too");

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "must resolve THEN patch; saw {calls:?}");
        let (patch_line, patch_body) = &calls[1];
        assert!(patch_line.starts_with("PATCH /me/messages/MSG-ID"), "got: {patch_line}");
        let sent: serde_json::Value = serde_json::from_str(patch_body).unwrap();
        let props = sent["singleValueExtendedProperties"].as_array().unwrap();
        assert_eq!(props[0]["id"], wire::JODD_PIN_PROP);
        assert_eq!(
            props[0]["value"], r#"{"pinned":false}"#,
            "unpin must WRITE false, not delete or no-op — sent: {patch_body}"
        );
    }

    #[tokio::test]
    async fn pin_sidecar_store_now_reports_some_even_when_empty() {
        // M4: Pin is a real scan. Ok(Some(vec![])) — never Ok(None) — because
        // there is no separate store that can be "not initialized"; zero
        // pinned notes is a real, enumerable answer, and sync_pin_state
        // (lib.rs) is meant to prune local pins against exactly this.
        let (addr, _) = server_returning(r#"{"value":[]}"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let sidecars = v().list_sidecars(SidecarKind::Pin).await.unwrap();
        assert!(sidecars.is_some_and(|v| v.is_empty()));
    }

    #[tokio::test]
    async fn delete_addresses_the_message_and_reports_success_on_204() {
        let (addr, seen) = recording_server_status(204).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());

        v.delete("MSG-ID").await.expect("204 is success");

        let calls = seen.lock().unwrap().clone();
        assert!(calls[0].0.starts_with("DELETE /me/messages/MSG-ID"), "got: {}", calls[0].0);
    }

    /// Fix round: `rename_folder` must send the LEAF as `displayName`, not
    /// the full Jodd path `push_one_folder` (lib.rs) hands it — the same
    /// requirement `create_folder_sends_the_leaf_as_display_name_and_
    /// returns_the_full_path` above proves for `create_folder`. Before this
    /// fix, renaming "Notes/L1" sent the literal string "Notes/Renamed" as
    /// the Exchange folder's `displayName`, which Apple displays as-is and
    /// the next scan sanitises into "Notes/Notes-Renamed". This test would
    /// fail if the full path were sent instead of the leaf.
    #[tokio::test]
    async fn rename_folder_sends_the_leaf_as_display_name() {
        let (addr, seen) = recording_server_status(204).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        v().rename_folder("F-ID", "Notes/Renamed").await.expect("204 is success");

        let calls = seen.lock().unwrap().clone();
        let (line, body) = &calls[0];
        assert!(line.starts_with("PATCH /me/mailFolders/F-ID"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            sent["displayName"], "Renamed",
            "the Exchange folder name must be the LEAF, not the full path — sent: {body}"
        );
    }

    /// A local server that responds with a specific HTTP status code.
    /// Records each request's line and body as `(line, body)` tuples.
    async fn recording_server_status(
        status_code: u16,
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (line, body) = if let Some(sep_pos) = req.find("\r\n\r\n") {
                    let hdr_part = &req[..sep_pos];
                    let line = hdr_part.lines().next().unwrap_or("").to_string();
                    let body = req[sep_pos + 4..].to_string();
                    (line, body)
                } else {
                    let line = req.lines().next().unwrap_or("").to_string();
                    (line, String::new())
                };
                recorder.lock().unwrap().push((line, body));

                let status_text = match status_code {
                    204 => "No Content",
                    200 => "OK",
                    _ => "Unknown",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    status_code, status_text
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    #[tokio::test]
    async fn methods_with_no_home_yet_name_the_milestone_rather_than_failing_vaguely() {
        // delete() (Task 7), the folder ops + move_note (Task 8), and the Pin
        // sidecar (M4) are now implemented, so this covers only what still
        // has no home: no cursor exists on this backend yet.
        let err = v().changes_since(None).await.unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().contains("Milestone 2"), "got: {err}");
    }

    #[tokio::test]
    async fn create_folder_refuses_when_the_root_cannot_be_found() {
        // Silent-wrong is the worse failure: guessing a parent would file the
        // folder somewhere the user did not ask for and cannot see.
        let (addr, _) = server_returning(r#"{"value":[]}"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());

        let err = v.create_folder("Ideas").await.unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().contains("Notes"), "must name what is missing: {err}");
    }

    #[tokio::test]
    async fn ensure_folder_uses_the_cached_id_without_a_network_call() {
        // No server is wired up at all — if this fell through to create_folder
        // (a scan + a POST), the test would hang or fail on a real connection
        // rather than merely fail an assertion.
        let folder_ids: std::collections::HashMap<String, String> =
            [("Notes/Ideas".to_string(), "IDEAS-ID".to_string())].into_iter().collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);

        let f = v.ensure_folder("Notes/Ideas").await.expect("cached id must resolve with no request");
        assert_eq!(f.id, "IDEAS-ID");
        assert_eq!(f.path, "Notes/Ideas");
    }

    #[tokio::test]
    async fn ensure_folder_falls_through_to_create_folder_when_the_path_is_uncached() {
        // No entry for "Ideas" in folder_ids, so this must actually attempt
        // create_folder rather than returning some other default — proven by
        // inheriting create_folder's own root-unreachable refusal rather than
        // a generic "not found" (which a short-circuit could also produce).
        let (addr, _) = server_returning(r#"{"value":[]}"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), Default::default());

        let err = v.ensure_folder("Ideas").await.unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().contains("Notes"), "must fall through to create_folder: {err}");
    }

    #[tokio::test]
    async fn move_note_resolves_the_destination_path_and_ignores_remove() {
        // A well-formed single-message response, matching what a real Graph
        // /move returns — move_note_to_folder decodes it via decode_or_refetch,
        // so a body-less 200 (recording_server_status) would misread as the
        // unidentifiable-response case and fail this test for the wrong reason.
        let (addr, seen) = server_returning(
            r#"{"id":"MSG-ID","subject":"s","internetMessageId":"<a@b>","parentFolderId":"IDEAS-ID",
                "createdDateTime":"2026-08-13T17:08:23Z","lastModifiedDateTime":"2026-08-13T19:48:36Z",
                "body":{"contentType":"html","content":"<html><body>s</body></html>"}}"#,
        )
        .await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));
        let folder_ids: std::collections::HashMap<String, String> =
            [("Notes/Ideas".to_string(), "IDEAS-ID".to_string())].into_iter().collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);

        v.move_note("MSG-ID", &["Notes/Ideas".to_string()], &["Notes".to_string()])
            .await
            .expect("a known destination path must resolve");

        // The line alone cannot tell path resolution apart from a bug that
        // sends the unresolved PATH ("Notes/Ideas") as destinationId instead
        // of the resolved id — the body is what actually proves the lookup
        // happened, not merely that a request went out.
        let (line, body) = seen.lock().unwrap().first().cloned().unwrap();
        assert!(line.starts_with("POST /me/messages/MSG-ID/move"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            sent["destinationId"], "IDEAS-ID",
            "must send the RESOLVED folder id, not the path — got: {body}"
        );
    }

    #[tokio::test]
    async fn move_note_names_the_path_it_could_not_resolve() {
        let v = v();
        let err = v
            .move_note("MSG-ID", &["Notes/Unknown".to_string()], &[])
            .await
            .unwrap_err();
        assert!(matches!(err, TransportError::Permanent { .. }));
        assert!(err.to_string().contains("Notes/Unknown"), "must name the offending path: {err}");
    }

    /// Fix round 1 regression: `create_folder` must send the LEAF as
    /// `displayName` even when handed a full Jodd path, and must return the
    /// caller's ORIGINAL path unchanged — not the leaf — so
    /// `mark_folder_created` keys the local row correctly. `folder_ids`
    /// already carries `"Notes"` → `"ROOT-ID"`, so this also proves the root
    /// lookup consults that cache instead of paying for a mailbox scan:
    /// exactly one request is recorded, and it is the create itself.
    #[tokio::test]
    async fn create_folder_sends_the_leaf_as_display_name_and_returns_the_full_path() {
        let (addr, seen) = server_returning(r#"{"id":"NEW-ID","displayName":"Ideas"}"#).await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));
        let folder_ids: std::collections::HashMap<String, String> =
            [("Notes".to_string(), "ROOT-ID".to_string())].into_iter().collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);

        // push_one_folder (lib.rs) passes CachedFolder.path verbatim, always
        // "Notes/<segment>" for a UI-created folder (gotcha #9).
        let f = v.create_folder("Notes/Ideas").await.expect("cached root id must resolve with no scan");

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "a cached root id must skip the mailbox scan entirely; saw {calls:?}");
        let (line, body) = &calls[0];
        assert!(line.starts_with("POST /me/mailFolders/ROOT-ID/childFolders"), "got: {line}");
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            sent["displayName"], "Ideas",
            "the Exchange folder name must be the LEAF, not the full path — sent: {body}"
        );

        assert_eq!(
            f.path, "Notes/Ideas",
            "the returned path must stay the caller's ORIGINAL path so mark_folder_created \
             keys the local row correctly"
        );
    }

    /// A local server that answers every request with the same fixed body,
    /// recording each request's `(line, body)`. Serves three shapes above: an
    /// empty-mailbox scan (`{"value":[]}"` ends Pass A's walk with no folders,
    /// so `notes_root_id` finds nothing — what the `create_folder`/
    /// `ensure_folder` refusal tests need), a single Graph resource (what
    /// `move_note`'s success test needs `move_note_to_folder`'s
    /// `decode_or_refetch` to parse), and a folder resource (what
    /// `create_folder`'s own success test needs `decode_folder_id` to parse).
    async fn server_returning(body: &str) -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let recorder = Arc::clone(&seen);
        let body = body.to_string();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (line, req_body) = if let Some(sep_pos) = req.find("\r\n\r\n") {
                    let hdr_part = &req[..sep_pos];
                    (hdr_part.lines().next().unwrap_or("").to_string(), req[sep_pos + 4..].to_string())
                } else {
                    (req.lines().next().unwrap_or("").to_string(), String::new())
                };
                recorder.lock().unwrap().push((line, req_body));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    /// A local server that answers by REQUEST PATH rather than by a single
    /// fixed body. `server_returning` above cannot express this test: the
    /// union logic in `list_folders` needs TWO different folder ids to get
    /// TWO different answers (one still exists, one is gone) inside the same
    /// scan.
    async fn server_routing(
        routes: &[(&str, u16, &str)],
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let routes: Vec<(String, u16, String)> =
            routes.iter().map(|(p, s, b)| (p.to_string(), *s, b.to_string())).collect();

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                recorder.lock().unwrap().push(line.clone());
                let (status, body) = routes
                    .iter()
                    .find(|(path, _, _)| line.contains(path.as_str()))
                    .map(|(_, s, b)| (*s, b.as_str()))
                    .unwrap_or((404, "{}"));
                let status_text = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Unknown",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    status_text,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    /// Like `server_routing` but also records each request's body — needed
    /// when a test must both route by path (e.g. a resolve-by-uuid GET vs. a
    /// PATCH to the resolved id, which land on different paths) AND assert
    /// on what was sent to the second one.
    async fn server_routing_with_body(
        routes: &[(&str, u16, &str)],
    ) -> (std::net::SocketAddr, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let routes: Vec<(String, u16, String)> =
            routes.iter().map(|(p, s, b)| (p.to_string(), *s, b.to_string())).collect();

        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 16 * 1024];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (line, req_body) = if let Some(sep_pos) = req.find("\r\n\r\n") {
                    let hdr_part = &req[..sep_pos];
                    (hdr_part.lines().next().unwrap_or("").to_string(), req[sep_pos + 4..].to_string())
                } else {
                    (req.lines().next().unwrap_or("").to_string(), String::new())
                };
                recorder.lock().unwrap().push((line.clone(), req_body));
                let (status, body) = routes
                    .iter()
                    .find(|(path, _, _)| line.contains(path.as_str()))
                    .map(|(_, s, b)| (*s, b.as_str()))
                    .unwrap_or((404, "{}"));
                let status_text = match status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Unknown",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    status_text,
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (addr, seen)
    }

    /// End-to-end proof of the union logic, not just `wire::folder_still_exists`
    /// in isolation: an empty scan means `to_folders` alone reports nothing, so
    /// EVERY folder below is a candidate probed via `folder_ids` — exactly the
    /// path a folder Jodd just created (and which now holds no note) takes.
    #[tokio::test]
    async fn list_folders_keeps_an_emptied_cached_folder_and_drops_a_deleted_one() {
        let (addr, _) = server_routing(&[
            ("/me/messages", 200, r#"{"value":[]}"#),
            ("/me/mailFolders/LIVE-ID/messages", 200, r#"{"value":[]}"#),
            ("/me/mailFolders/GONE-ID/messages", 404, r#"{"error":{"code":"ErrorItemNotFound"}}"#),
        ])
        .await;
        wire::set_graph_base_for_test(&format!("http://{addr}"));

        let folder_ids: std::collections::HashMap<String, String> = [
            ("Notes/Live".to_string(), "LIVE-ID".to_string()),
            ("Notes/Gone".to_string(), "GONE-ID".to_string()),
        ]
        .into_iter()
        .collect();
        let v = MicrosoftVertical::new("tok".into(), "acct".into(), folder_ids);

        let folders = v.list_folders().await.expect("a 404 probe must not fail the whole read");
        let paths: Vec<&str> = folders.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"Notes/Live"),
            "an emptied-but-still-existing folder must survive: {paths:?}"
        );
        assert!(
            !paths.contains(&"Notes/Gone"),
            "a folder that 404s must not be reported as existing: {paths:?}"
        );
    }
}
