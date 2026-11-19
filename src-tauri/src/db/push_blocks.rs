//! Permanently-refused pushes (gotcha #14): marking, listing and clearing
//! the blocks on notes, pins and folders.
//! Split out of `db.rs` 2026-11-16.

use super::*;

impl Db {
    // ─── Permanently-refused pushes ───────────────────────────────────────

    /// Record that this note's push was refused permanently, and stop the
    /// worker retrying it.
    ///
    /// `sync_state` is left alone on purpose. The row IS still dirty — it
    /// holds local content no remote has — and saying otherwise is the lie
    /// this whole path exists to remove. What changes is only whether
    /// `list_dirty` hands it to the worker again. See migration #17.
    ///
    /// Idempotent, and it deliberately overwrites an existing reason: the
    /// second failure's message is the current one, and a stale reason shown
    /// to the user is worse than none.
    pub fn mark_push_blocked(&self, uuid: &str, account_id: &str, reason: &str) -> SqlResult<()> {
        self.mark_push_blocked_as(uuid, account_id, reason, false)
    }

    /// `mark_push_blocked` for a refusal about the remote's own document
    /// (`TransportError::is_blocked_by_remote_document`): no local edit can
    /// get past it, so the editor locks the note until a change on the remote
    /// replaces that document (`upsert_from_remote` clears it). Migration #20.
    pub fn mark_push_blocked_by_remote(&self, uuid: &str, account_id: &str, reason: &str) -> SqlResult<()> {
        self.mark_push_blocked_as(uuid, account_id, reason, true)
    }

    pub(crate) fn mark_push_blocked_as(&self, uuid: &str, account_id: &str, reason: &str, by_remote: bool) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE notes SET push_blocked_reason = ?1, push_blocked_by_remote = ?2
             WHERE uuid = ?3 AND account_id = ?4",
            params![reason, by_remote as i64, uuid, account_id],
        )?;
        Ok(())
    }

    /// Re-arm every blocked note in one account, returning how many were
    /// cleared. The editor's "Try again" used to call this; since 2026-09-30
    /// it re-arms only the note on screen (`clear_push_block`).
    ///
    /// There is no automatic re-arm, by decision. The two permanent refusals
    /// this backend actually produces — an attachment on Exchange, and a
    /// subfolder with no reachable Exchange id (gotcha #12) — have no cheap
    /// signal that says "the cause is gone now", and a timer-based retry
    /// would just be the 5-second loop with extra steps. The user editing the
    /// note (`apply_local_edit` clears the reason) or pressing the button is
    /// the signal. If a future backend gains a real "blocker cleared" event,
    /// that event should call this — not a poll.
    pub fn clear_push_blocks(&self, account_id: &str) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE notes SET push_blocked_reason = NULL, push_blocked_by_remote = 0
             WHERE account_id = ?1 AND push_blocked_reason IS NOT NULL",
            params![account_id],
        )?;
        Ok(n)
    }

    /// Re-arm one note's blocked push, returning whether it was blocked.
    /// Backs the editor's "Try again", which is about the note on screen:
    /// retrying every blocked note in the account also unlocked notes the
    /// user was not looking at, only for the worker to refuse them again.
    /// Follows a rekeyed uuid (gotcha #16) — the editor may still hold the old one.
    pub fn clear_push_block(&self, uuid: &str, account_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let uuid = Self::resolve_note_uuid_conn(&conn, uuid, account_id)?;
        let n = conn.execute(
            "UPDATE notes SET push_blocked_reason = NULL, push_blocked_by_remote = 0
             WHERE uuid = ?1 AND account_id = ?2 AND push_blocked_reason IS NOT NULL",
            params![uuid, account_id],
        )?;
        Ok(n > 0)
    }

    /// Stop pushing this note's pin: the backend refused the sidecar write
    /// permanently (gotcha #14). `pin_dirty` stays 1 — the pin was never
    /// sent, and keeping it also keeps `apply_remote_pin` / `clear_pins_not_in`
    /// from undoing it on the next pull. Guarded on `pinned = pushed_pinned`
    /// like `mark_pin_pushed`: a toggle made mid-push is a new request, not
    /// the refused one. Re-armed by toggling the pin (`set_pin`).
    pub fn mark_pin_push_blocked(
        &self,
        uuid: &str,
        account_id: &str,
        pushed_pinned: bool,
        reason: &str,
    ) -> SqlResult<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE notes SET pin_push_blocked_reason = ?1
             WHERE uuid = ?2 AND account_id = ?3 AND pin_dirty = 1 AND pinned = ?4",
            params![reason, uuid, account_id, pushed_pinned as i64],
        )?;
        Ok(())
    }

    /// Stop pushing this folder row: the backend refused its create or rename
    /// permanently. The row keeps its
    /// state and stays listed — dropping it made a folder the user had just
    /// made vanish — but leaves `list_dirty_folders` and `has_pending_pushes`.
    /// Re-armed by renaming the folder, or deleting it while no other row shares its label id, and settled by a pull
    /// that lists the exact name (`upsert_folder_from_remote`).
    pub fn mark_folder_push_blocked(
        &self,
        account_id: &str,
        path: &str,
        pushed_state: FolderSyncState,
        reason: &str,
    ) -> SqlResult<()> {
        // Guarded on the state that was pushed, like `mark_pin_push_blocked`:
        // a delete made during the refused push is a new request.
        self.conn.lock().unwrap().execute(
            "UPDATE folders SET push_blocked_reason = ?1
             WHERE account_id = ?2 AND path = ?3 AND sync_state = ?4",
            params![reason, account_id, path, pushed_state.as_str()],
        )?;
        Ok(())
    }

    /// Every blocked pin and folder in one account, with its reason — the
    /// read side of gotcha #14's "show it as text" for the two blocks that
    /// have no field on the note the frontend holds. A reason on a row that
    /// is no longer pending (`pin_dirty = 0`, a clean folder) is not a block,
    /// matching `count_pending_pushes`.
    pub fn list_sync_blocks(&self, account_id: &str) -> SqlResult<SyncBlocks> {
        let conn = self.conn.lock().unwrap();
        let pins = conn
            .prepare(
                "SELECT uuid, pin_push_blocked_reason FROM notes
                 WHERE account_id = ?1 AND pin_dirty = 1
                   AND pin_push_blocked_reason IS NOT NULL
                 ORDER BY uuid",
            )?
            .query_map(params![account_id], |r| Ok(PinBlock { uuid: r.get(0)?, reason: r.get(1)? }))?
            .collect::<SqlResult<Vec<_>>>()?;
        let folders = conn
            .prepare(
                "SELECT path, push_blocked_reason FROM folders
                 WHERE account_id = ?1 AND sync_state != 'clean'
                   AND push_blocked_reason IS NOT NULL
                 ORDER BY path",
            )?
            .query_map(params![account_id], |r| Ok(FolderBlock { path: r.get(0)?, reason: r.get(1)? }))?
            .collect::<SqlResult<Vec<_>>>()?;
        Ok(SyncBlocks { pins, folders })
    }

    /// The explicit "Try again" for one blocked pin. Follows a rekeyed uuid
    /// like `clear_push_block`. `true` if a block was cleared.
    pub fn clear_pin_push_block(&self, uuid: &str, account_id: &str) -> SqlResult<bool> {
        let conn = self.conn.lock().unwrap();
        let uuid = Self::resolve_note_uuid_conn(&conn, uuid, account_id)?;
        let n = conn.execute(
            "UPDATE notes SET pin_push_blocked_reason = NULL
             WHERE uuid = ?1 AND account_id = ?2 AND pin_push_blocked_reason IS NOT NULL",
            params![uuid, account_id],
        )?;
        Ok(n > 0)
    }

    /// The explicit "Try again" for one blocked folder row. `true` if a
    /// block was cleared.
    pub fn clear_folder_push_block(&self, account_id: &str, path: &str) -> SqlResult<bool> {
        let n = self.conn.lock().unwrap().execute(
            "UPDATE folders SET push_blocked_reason = NULL
             WHERE account_id = ?1 AND path = ?2 AND push_blocked_reason IS NOT NULL",
            params![account_id, path],
        )?;
        Ok(n > 0)
    }

    /// Every note in one account whose push was permanently refused.
    ///
    /// The read side of "the UI must be able to see this". It goes through
    /// SQLite rather than any list path that talks to the backend, because a
    /// blocked note has **no remote object at all** in the case that matters
    /// (a create that never landed) — `list_notes` and `list_notes_in_folder`
    /// both build their result from what the remote returned, so a blocked
    /// note is absent from both by construction. That is precisely how a note
    /// the UI called "Saved" left the list 30 seconds later.
    pub fn list_push_blocked(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, push_blocked_reason, push_blocked_by_remote
             FROM notes
             WHERE account_id = ?1 AND push_blocked_reason IS NOT NULL
               AND sync_state != 'deleted_pending'
             ORDER BY last_local_modified_at DESC",
        )?;
        let rows = stmt.query_map(params![account_id], row_to_note)?;
        rows.collect()
    }

    /// Every row holding local state the remote has not seen: the blocked
    /// rows above plus every `dirty` one still queued — a note created, edited
    /// or MOVED here and not pushed yet. A listing is built from what the
    /// remote returned, so until the push lands such a note is missing from
    /// the folder SQLite files it in (a move: moved, shown, gone, back —
    /// 2026-11-18). Never a clean row: a clean row a listing lacks is one the
    /// remote deleted, and the prune owns that.
    pub fn list_unpushed(&self, account_id: &str) -> SqlResult<Vec<CachedNote>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT uuid, account_id, id, title, body_html, date, x_mail_created_date,
                    label, local_version, remote_version, sync_state,
                    last_synced_at, last_local_modified_at, last_remote_modified_at,
                    pinned, meta_msg_id, pin_dirty, push_blocked_reason, push_blocked_by_remote
             FROM notes
             WHERE account_id = ?1
               AND (push_blocked_reason IS NOT NULL OR sync_state = 'dirty')
               AND sync_state != 'deleted_pending'
             ORDER BY last_local_modified_at DESC",
        )?;
        let rows = stmt.query_map(params![account_id], row_to_note)?;
        rows.collect()
    }
}

/// Gotcha #14's obligations for the two queues that had none: pin sidecars
/// and folder rows. A row the backend refused permanently stops retrying,
/// stays what it is (pinned, listed), leaves the queue that gates account
/// removal (gotcha #2), and still counts as unsent.
#[cfg(test)]
mod blocked_pin_and_folder_tests {
    use super::*;
    use crate::test_support::{note, temp_db};

    const A: &str = "acct";

    fn dirty_names(db: &Db) -> Vec<String> {
        db.list_dirty_folders().unwrap().into_iter().map(|f| f.path).collect()
    }

    #[test]
    fn a_blocked_pin_leaves_the_pending_queue_but_stays_pinned_and_unsent() {
        let db = temp_db();
        note(A, "u1").insert(&db);
        db.set_pin("u1", A, true).unwrap();
        assert!(db.has_pending_pushes(A).unwrap());

        db.mark_pin_push_blocked("u1", A, true, "Gmail refused to create label 'Notes-Meta'").unwrap();

        assert!(db.list_pin_dirty().unwrap().is_empty(), "a blocked pin must not retry every tick");
        assert!(!db.has_pending_pushes(A).unwrap(), "must not hold a Draining account (gotcha #2)");
        let c = db.count_pending_pushes(A).unwrap();
        assert_eq!((c.pins, c.blocked), (0, 1), "not queued work, but still unsent");
        let n = db.get("u1", A).unwrap().unwrap();
        assert!(n.pinned && n.pin_dirty, "the pin is neither undone nor claimed as pushed");
        // A pull whose sidecar listing lacks the note must not unpin it.
        db.clear_pins_not_in(A, &[], prune_horizon_ms()).unwrap();
        assert!(db.get("u1", A).unwrap().unwrap().pinned);
    }

    #[test]
    fn toggling_the_pin_again_rearms_a_blocked_pin() {
        let db = temp_db();
        note(A, "u1").insert(&db);
        note(A, "u2").insert(&db);
        db.set_pin("u1", A, true).unwrap();
        db.set_pin("u2", A, true).unwrap();
        db.mark_pin_push_blocked("u1", A, true, "refused").unwrap();
        db.mark_pin_push_blocked("u2", A, true, "refused").unwrap();

        db.set_pin("u1", A, false).unwrap();
        db.set_pin_batch(A, &["u2".to_string()], false).unwrap();

        let mut armed: Vec<String> = db.list_pin_dirty().unwrap().into_iter().map(|n| n.uuid).collect();
        armed.sort();
        assert_eq!(armed, vec!["u1".to_string(), "u2".to_string()]);
        assert_eq!(db.count_pending_pushes(A).unwrap().blocked, 0);
    }

    #[test]
    fn a_note_blocked_for_its_content_and_its_pin_is_one_unsent_note() {
        // `blocked` counts notes, not blocks: the count is two disjoint
        // sums (content, then pin-only) so each reads an index, and the
        // second must leave out a note the first already counted.
        let db = temp_db();
        note(A, "u1").insert(&db);
        note(A, "u2").insert(&db);
        db.set_pin("u1", A, true).unwrap();
        db.set_pin("u2", A, true).unwrap();
        db.mark_pin_push_blocked("u1", A, true, "refused").unwrap();
        db.mark_pin_push_blocked("u2", A, true, "refused").unwrap();
        db.mark_push_blocked("u1", A, "refused too").unwrap();

        assert_eq!(db.count_pending_pushes(A).unwrap().blocked, 2);
    }

    #[test]
    fn a_blocked_folder_stays_listed_and_leaves_the_pending_queue() {
        let db = temp_db();
        db.create_folder_local_new(A, "Notes/Work").unwrap();

        db.mark_folder_push_blocked(A, "Notes/Work", FolderSyncState::DirtyNew, "Gmail refused to create label 'Notes/Work'").unwrap();

        assert!(dirty_names(&db).is_empty(), "a blocked folder must not retry every tick");
        assert!(!db.has_pending_pushes(A).unwrap(), "must not hold a Draining account (gotcha #2)");
        let c = db.count_pending_pushes(A).unwrap();
        assert_eq!((c.folders, c.blocked), (0, 1));
        let listed: Vec<String> = db.list_folders(A).unwrap().into_iter().map(|f| f.path).collect();
        assert!(listed.contains(&"Notes/Work".to_string()), "the folder the user made does not vanish");
    }

    #[test]
    fn renaming_a_blocked_folder_rearms_it() {
        let db = temp_db();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        db.create_folder_local_new(A, "Notes/Play").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Work", FolderSyncState::DirtyNew, "refused").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Play", FolderSyncState::DirtyNew, "refused").unwrap();

        db.rename_subtree(A, "Notes/Work", "Notes/Jobs").unwrap();
        db.rename_folder(A, "Notes/Play", "Notes/Fun").unwrap();

        let mut armed = dirty_names(&db);
        armed.sort();
        assert_eq!(armed, vec!["Notes/Fun".to_string(), "Notes/Jobs".to_string()]);
    }

    #[test]
    fn deleting_a_blocked_renamed_folder_pushes_the_delete() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Work", "Label_7").unwrap();
        db.rename_folder(A, "Notes/Work", "Notes/Jobs").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Jobs", FolderSyncState::DirtyRenamed, "refused").unwrap();

        db.mark_folder_deleted(A, "Notes/Jobs").unwrap();

        assert_eq!(db.folder_sync_state(A, "Notes/Jobs").unwrap(), Some(FolderSyncState::DeletedPending));
        assert_eq!(dirty_names(&db), vec!["Notes/Jobs".to_string()]);
    }

    /// A delete made while the refused push was in flight is a new request;
    /// the block belongs to the push that was refused, not to it.
    #[test]
    fn a_block_for_an_earlier_state_does_not_stick_to_a_delete() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Work", "Label_7").unwrap();
        db.rename_folder(A, "Notes/Work", "Notes/Jobs").unwrap();
        db.mark_folder_deleted(A, "Notes/Jobs").unwrap();

        db.mark_folder_push_blocked(A, "Notes/Jobs", FolderSyncState::DirtyRenamed, "refused").unwrap();

        assert_eq!(dirty_names(&db), vec!["Notes/Jobs".to_string()], "the delete still pushes");
    }

    /// The read side of "show it as text": every blocked pin and folder of
    /// one account, with its reason. Nothing from another account, nothing
    /// unblocked, and a block on a row no longer pending is not a block.
    #[test]
    fn list_sync_blocks_names_each_blocked_pin_and_folder() {
        let db = temp_db();
        note(A, "u1").insert(&db);
        note(A, "u2").insert(&db);
        note("other", "u3").insert(&db);
        db.set_pin("u1", A, true).unwrap();
        db.set_pin("u2", A, true).unwrap();
        db.set_pin("u3", "other", true).unwrap();
        db.mark_pin_push_blocked("u1", A, true, "meta refused").unwrap();
        db.mark_pin_push_blocked("u3", "other", true, "elsewhere").unwrap();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        db.create_folder_local_new(A, "Notes/Play").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Work", FolderSyncState::DirtyNew, "label refused").unwrap();

        let b = db.list_sync_blocks(A).unwrap();

        assert_eq!(b.pins, vec![PinBlock { uuid: "u1".into(), reason: "meta refused".into() }]);
        assert_eq!(b.folders, vec![FolderBlock { path: "Notes/Work".into(), reason: "label refused".into() }]);
    }

    /// The explicit "Try again": one pin or one folder, back in the queue.
    #[test]
    fn retrying_one_block_rearms_only_that_row() {
        let db = temp_db();
        note(A, "u1").insert(&db);
        note(A, "u2").insert(&db);
        db.set_pin("u1", A, true).unwrap();
        db.set_pin("u2", A, true).unwrap();
        db.mark_pin_push_blocked("u1", A, true, "r").unwrap();
        db.mark_pin_push_blocked("u2", A, true, "r").unwrap();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Work", FolderSyncState::DirtyNew, "r").unwrap();

        assert!(db.clear_pin_push_block("u1", A).unwrap());
        assert!(db.clear_folder_push_block(A, "Notes/Work").unwrap());
        assert!(!db.clear_folder_push_block(A, "Notes/Work").unwrap(), "nothing left to clear");

        let pins: Vec<String> = db.list_pin_dirty().unwrap().into_iter().map(|n| n.uuid).collect();
        assert_eq!(pins, vec!["u1".to_string()]);
        assert_eq!(dirty_names(&db), vec!["Notes/Work".to_string()]);
        assert_eq!(db.list_sync_blocks(A).unwrap().pins.len(), 1, "u2 is still blocked");
    }

    /// While a rename is blocked, a pull writes a clean mirror at the
    /// remote's old name with the SAME label id. The mirror looks empty when
    /// its notes are dirty under the blocked name, so `delete_folder` allowed
    /// it — and the worker deleted that label: the user's real folder. Its
    /// children's mirrors too. A different folder is still deletable.
    #[test]
    fn a_pull_mirror_of_a_blocked_rename_cannot_be_deleted() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/A", "Label_1").unwrap();
        db.upsert_folder_from_remote(A, "Notes/A/x", "Label_2").unwrap();
        db.upsert_folder_from_remote(A, "Notes/C", "Label_3").unwrap();
        db.rename_subtree(A, "Notes/A", "Notes/B").unwrap();
        db.mark_folder_push_blocked(A, "Notes/B", FolderSyncState::DirtyRenamed, "refused").unwrap();
        db.upsert_folder_from_remote(A, "Notes/A", "Label_1").unwrap(); // the pull's mirror
        db.upsert_folder_from_remote(A, "Notes/A/x", "Label_2").unwrap();

        let x = db.folder_delete_refusal(A, "Notes/A/x").unwrap().expect("a child's mirror is refused");
        assert!(x.contains("'Notes/B/x'"), "names the folder it mirrors: {x}");
        let top = db.folder_delete_refusal(A, "Notes/A").unwrap().expect("the mirror is refused");
        assert!(top.contains("'Notes/B'"), "{top}");
        assert_eq!(db.folder_delete_refusal(A, "Notes/C").unwrap(), None, "another folder is deletable");

        // Post-merge review of #151: the blocked row shares Label_1 too, and
        // after the pull it LOOKS empty — its clean notes were moved into the
        // mirror. Deleting it deleted Label_1: the real folder and its notes.
        note(A, "n1").label("Notes/A").insert(&db); // the pull filed it in the mirror
        assert_eq!(db.count_notes_in_label(A, "Notes/B").unwrap(), 0, "precondition: B looks empty");
        let b = db.folder_delete_refusal(A, "Notes/B").unwrap().expect("the blocked row is refused too");
        assert!(b.contains("'Notes/A'") && b.contains("Try again"), "names the real folder and the remedy: {b}");
        let bx = db.folder_delete_refusal(A, "Notes/B/x").unwrap().expect("and its child");
        assert!(bx.contains("'Notes/A/x'"), "{bx}");
        // A child cannot be renamed "back" by itself (one segment at a time)
        // and its own Try again re-blocks it: the remedy names the parent.
        assert!(bx.contains("Rename 'Notes/B' back"), "names the parent that can act: {bx}");
        assert!(!bx.contains("Rename 'Notes/B/x'"), "{bx}");
        let ax = db.folder_delete_refusal(A, "Notes/A/x").unwrap().unwrap();
        assert!(ax.contains("Rename 'Notes/B' back"), "the mirror side names it too: {ax}");
        assert_eq!(db.shared_label_partner(A, "Notes/B").unwrap().as_deref(), Some("Notes/A"), "what the worker re-checks");
        assert_eq!(db.shared_label_partner(A, "Notes/C").unwrap(), None);
    }

    /// A delete that was refused (blocked) and since gained a sibling sharing
    /// its label id — another device renamed that folder, a pull listed the
    /// new name. It is not a rename, and "Try again" is the button that led
    /// here: the remedy is renaming it to the sibling's name to keep it.
    #[test]
    fn a_refused_delete_sharing_a_label_says_how_to_keep_it() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/X", "Label_1").unwrap();
        db.mark_folder_deleted(A, "Notes/X").unwrap();
        db.mark_folder_push_blocked(A, "Notes/X", FolderSyncState::DeletedPending, "refused").unwrap();
        db.upsert_folder_from_remote(A, "Notes/Y", "Label_1").unwrap();

        let m = db.folder_delete_refusal(A, "Notes/X").unwrap().expect("refused");
        assert!(m.contains("delete of 'Notes/X' was refused") && m.contains("rename 'Notes/X' to 'Notes/Y'"), "{m}");
        assert!(!m.contains("Try again"), "{m}");
        let y = db.folder_delete_refusal(A, "Notes/Y").unwrap().expect("refused");
        assert!(y.contains("rename 'Notes/X' to 'Notes/Y'"), "{y}");
    }

    /// Another device renamed a folder; until the next pull prunes it, the
    /// old name is a clean row with the same label id as the new one. Either
    /// delete would delete the one real folder, so both are refused, with a
    /// message that says to wait for the sync.
    #[test]
    fn two_clean_rows_of_one_folder_are_both_refused() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Old", "Label_1").unwrap();
        db.upsert_folder_from_remote(A, "Notes/New", "Label_1").unwrap();
        let m = db.folder_delete_refusal(A, "Notes/New").unwrap().expect("refused");
        assert!(m.contains("few minutes") && m.contains("'Notes/Old'"), "{m}");
        assert!(db.folder_delete_refusal(A, "Notes/Old").unwrap().is_some());
    }

    /// Try again lands the refused rename. The mirror used to linger, clean,
    /// until a full pull pruned it (sparing anything synced in the last 10
    /// min) — an empty duplicate that was still the same label, so deleting
    /// it deleted the folder just renamed (review of this PR). Landing the
    /// rename retires its mirror and moves the notes the pull filed there.
    #[test]
    fn landing_a_refused_rename_retires_its_mirror() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/A", "Label_1").unwrap();
        db.upsert_folder_from_remote(A, "Notes/Z", "Label_3").unwrap();
        db.rename_subtree(A, "Notes/A", "Notes/B").unwrap();
        db.mark_folder_push_blocked(A, "Notes/B", FolderSyncState::DirtyRenamed, "refused").unwrap();
        db.upsert_folder_from_remote(A, "Notes/A", "Label_1").unwrap(); // the pull's mirror
        note(A, "u1").label("Notes/A").insert(&db); // a clean note the pull relabelled to it
        note(A, "u2").label("Notes/Z").insert(&db);

        db.clear_folder_push_block(A, "Notes/B").unwrap();
        db.mark_folder_renamed(A, "Notes/B").unwrap();

        assert!(db.get_folder(A, "Notes/A").unwrap().is_none(), "the mirror is gone");
        assert_eq!(db.get_folder(A, "Notes/B").unwrap().unwrap().sync_state, FolderSyncState::Clean);
        assert_eq!(db.get("u1", A).unwrap().unwrap().label, "Notes/B", "its notes follow the folder");
        assert_eq!(db.get("u2", A).unwrap().unwrap().label, "Notes/Z", "another folder's notes stay");
        assert!(db.get_folder(A, "Notes/Z").unwrap().is_some());
        assert_eq!(db.folder_delete_refusal(A, "Notes/B").unwrap().unwrap_or_default().contains("same folder"), false,
            "the real folder is deletable");
    }

    /// Defensive: if two rows ever share a label id, a clean one is refused
    /// whatever the other's state — deleting either deletes both.
    #[test]
    fn a_clean_row_sharing_a_label_id_with_any_row_is_refused() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/A", "Label_1").unwrap();
        db.upsert_folder_from_remote(A, "Notes/B", "Label_1").unwrap();

        let r = db.folder_delete_refusal(A, "Notes/A").unwrap().expect("refused");
        assert!(r.contains("'Notes/B'"), "{r}");
    }

    /// A refused delete is blocked, not dropped — the folder still exists
    /// remotely. Hidden, it could not show its `!` or Try again, and the
    /// sidebar would claim a delete that never happened (gotcha #14).
    #[test]
    fn a_folder_whose_delete_was_refused_is_listed_again() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Old", "Label_5").unwrap();
        db.mark_folder_deleted(A, "Notes/Old").unwrap();
        let listed = |db: &Db| db.list_folders(A).unwrap().into_iter().map(|f| f.path).collect::<Vec<_>>();
        assert!(!listed(&db).contains(&"Notes/Old".to_string()), "a pending delete is hidden");

        db.mark_folder_push_blocked(A, "Notes/Old", FolderSyncState::DeletedPending, "refused").unwrap();

        assert!(listed(&db).contains(&"Notes/Old".to_string()));
        assert_eq!(db.folder_delete_refusal(A, "Notes/Old").unwrap(), None, "deleting it again is allowed");
        db.mark_folder_deleted(A, "Notes/Old").unwrap();
        assert_eq!(dirty_names(&db), vec!["Notes/Old".to_string()], "and re-arms the delete");
    }

    /// Renaming a folder whose delete was refused means keeping it: a
    /// rename, not the delete retried under a new name.
    #[test]
    fn renaming_a_folder_whose_delete_was_refused_keeps_it() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Old", "Label_5").unwrap();
        db.mark_folder_deleted(A, "Notes/Old").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Old", FolderSyncState::DeletedPending, "refused").unwrap();

        db.rename_subtree(A, "Notes/Old", "Notes/Kept").unwrap();

        assert_eq!(db.folder_sync_state(A, "Notes/Kept").unwrap(), Some(FolderSyncState::DirtyRenamed));
        assert_eq!(dirty_names(&db), vec!["Notes/Kept".to_string()]);
    }

    /// A note filed into a blocked-delete folder makes "Try again" unsafe:
    /// LocalFs deletes a directory with everything in it.
    #[test]
    fn a_blocked_delete_that_gained_a_note_refuses_another_delete() {
        let db = temp_db();
        db.upsert_folder_from_remote(A, "Notes/Old", "Label_5").unwrap();
        db.mark_folder_deleted(A, "Notes/Old").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Old", FolderSyncState::DeletedPending, "refused").unwrap();
        note(A, "u1").label("Notes/Old").insert(&db);

        assert!(db.folder_delete_refusal(A, "Notes/Old").unwrap().unwrap().contains("not empty"));
    }

    /// The blocker is gone when the remote lists a label of exactly this
    /// name — the same adoption a create's 409 performs.
    #[test]
    fn a_blocked_new_folder_is_adopted_when_the_remote_lists_its_exact_name() {
        let db = temp_db();
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        db.mark_folder_push_blocked(A, "Notes/Work", FolderSyncState::DirtyNew, "refused").unwrap();

        db.upsert_folder_from_remote(A, "Notes/Work", "Label_9").unwrap();

        let f = db.get_folder(A, "Notes/Work").unwrap().unwrap();
        assert_eq!((f.sync_state, f.label_id.as_deref()), (FolderSyncState::Clean, Some("Label_9")));
        assert_eq!(db.count_pending_pushes(A).unwrap().blocked, 0);
    }
}
