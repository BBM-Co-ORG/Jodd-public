//! Per-account operational commands: listing notes and folders (live and
//! cached), delete / move / restore, trash, attachments, search, note links
//! and citations, `index_account`, the pin/tag sync triggers and `flush_sync`.
//! Moved out of `lib.rs` 2026-11-16.

use super::*;
// Build the "by Gmail message id" cache map that gmail::list_notes uses to
// skip messages.get for already-hydrated notes. Filters out rows that don't
// have a remote id yet (local-new pending push) — we have nothing to match
// them against in the Gmail response.
pub(crate) fn cache_by_msg_id(state: &State<'_, AppState>, account_id: &str) -> HashMap<String, gmail::Note> {
    match state.db.list_notes(account_id) {
        Ok(rows) => rows
            .into_iter()
            .filter(|c| !c.id.is_empty())
            .map(|c| (c.id.clone(), c.to_frontend_note()))
            .collect(),
        Err(e) => {
            log!("cache_by_msg_id failed for {}: {}", account_id, e);
            HashMap::new()
        }
    }
}

/// Bring a remote-derived listing in line with what SQLite files where: append
/// the account's notes the remote has not seen yet, skipping any the remote
/// already returned, and — folder-scoped — leave out a note SQLite files in
/// another folder.
///
/// Both list paths build their result out of what the backend handed back, so
/// a note whose CREATE was refused is missing from both by construction — it
/// has no remote object to be returned. The frontend then treats "absent from
/// the fetch" as "deleted elsewhere" and drops it from the store after a 30 s
/// grace window (`loadFolderNotes`, App.svelte), so a note the toolbar had
/// just called "Saved" silently left the list while its row sat in SQLite
/// being retried forever. Measured 2026-08-17: `list_cached_notes` returned 2
/// for the account while `list_notes` returned 0, three polls in a row.
///
/// A note merely not pushed YET is the same case for as long as the push
/// takes (`Db::list_unpushed`): a note moved here was in the new folder,
/// gone from it when the next folder read came back without it, and back once
/// the worker's push reached the remote (2026-11-18).
///
/// Filtered by `label` when the caller is folder-scoped, because
/// `prune_clean_in_label` and the frontend's per-folder replace both key off
/// the folder — handing a folder view a note from a different folder would put
/// it in the wrong place, not merely show it twice. The same goes for a note
/// the REMOTE still has here and SQLite has moved away: the folder it left
/// lists the old message until the push lands. That filter reads the label
/// `overlay_cached_row` restamped from the row, so it runs after the restamp.
///
/// Deliberately additive and last: reconciliation, pruning and local_version
/// restamping have all already run over the remote rows by the time this is
/// called, and an unpushed row must not participate in any of them (it may
/// have no remote counterpart to reconcile against).
///
/// Takes `&Db` rather than the Tauri `State` for the same reason
/// `push_one_dirty_db` is split out — it is the whole of the behaviour, and a
/// `State` cannot be built in a unit test.
pub(crate) fn append_unpushed_notes(
    db: &db::Db,
    account_id: &str,
    label: Option<&str>,
    result: &mut Vec<gmail::Note>,
) {
    if let Some(l) = label {
        let before = result.len();
        result.retain(|n| n.label == l);
        if result.len() < before {
            log!(
                "append_unpushed_notes: left out {} note(s) SQLite files outside '{}'",
                before - result.len(), l
            );
        }
    }
    let unpushed = match db.list_unpushed(account_id) {
        Ok(rows) => rows,
        Err(e) => {
            log!("append_unpushed_notes: list_unpushed failed for {}: {}", account_id, e);
            return;
        }
    };
    let present: std::collections::HashSet<String> =
        result.iter().map(|n| n.uuid.clone()).collect();
    let mut added = 0usize;
    for row in unpushed {
        if present.contains(&row.uuid) {
            continue;
        }
        if label.is_some_and(|l| row.label != l) {
            continue;
        }
        result.push(row.to_frontend_note());
        added += 1;
    }
    if added > 0 {
        log!(
            "append_unpushed_notes: surfaced {} note(s) for {} the remote has not seen yet",
            added, account_id
        );
    }
}

/// Whether a listing may run, given the account's kind and — on Android only —
/// the result of asking for All files access. `None` means no check was made
/// (desktop, or not a Local Folder account).
///
/// On Android 11+ without All files access, MediaProvider's FUSE layer may let
/// `read_dir` SUCCEED but filter it to files Jodd itself created, so
/// `ensure_readable` passes, the listing is silently partial, and the prune
/// that follows drops every clean cached note that came from elsewhere.
/// EACCES cannot be relied on, so the listing is refused before any read.
/// A failed check is its own error, never folded into "access is off".
pub(crate) fn localfs_listing_gate(is_localfs: bool, access: Option<Result<bool, String>>) -> Result<(), String> {
    if !is_localfs {
        return Ok(());
    }
    match access {
        None | Some(Ok(true)) => Ok(()),
        Some(Ok(false)) => Err(crate::backend::localfs::transport::ACCESS_LOST.to_string()),
        Some(Err(e)) => Err(e),
    }
}

/// Runs `localfs_listing_gate` for a listing command. Asks the plugin only on
/// Android and only for a Local Folder account; desktop is unchanged.
pub(crate) async fn check_localfs_listing_access(
    state: &AppState,
    backend_kind: accounts::BackendKind,
) -> Result<(), String> {
    let is_localfs = backend_kind == accounts::BackendKind::LocalFs;
    #[cfg(target_os = "android")]
    let access = if is_localfs {
        Some(crate::android_storage::has_all_files_access(&state.app_handle).await)
    } else {
        None
    };
    #[cfg(not(target_os = "android"))]
    let access = {
        let _ = state;
        None
    };
    localfs_listing_gate(is_localfs, access)
}

#[tauri::command]
pub async fn list_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    log!("list_notes: invoked for account {}", account_id);

    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    // Before any read or prune: a Local Folder listing on Android without All
    // files access may be silently partial (see `localfs_listing_gate`).
    check_localfs_listing_access(&state, backend_kind).await?;

    // **This command IS the refresh** — the ⟳ button, the 10-minute full poll,
    // and the authoritative pull that prunes clean rows. On iCloud the zone
    // read is shared across vertical instances so a folder sweep does not walk
    // the whole account once per folder; that sharing has to stop here, or the
    // one action whose entire purpose is "go and look again" would be answered
    // out of a cache. Every other backend re-fetches by construction.
    if backend_kind == accounts::BackendKind::ICloud {
        icloud_scan_cache(&state, &account_id).invalidate().await;
    }

    // ── Backend-specific: obtain notes + vertical ─────────────────────
    // Gmail: ensure token, get label_map, build vertical via parts, run
    //        list_all_notes, then self-heal if empty (stale label cache).
    // Microsoft: vertical_for handles the token; there is no label map and no
    //        enumerable folder tree, so folders come back through the vertical.
    // LocalFs: no token/keychain — vertical_for resolves root_dir and
    //          the filesystem vertical handles list_all_notes directly.
    //
    // Dispatched on `folder_source_kind(backend_kind)` rather than on
    // `backend_kind` directly, so the mapping from backend to reconciliation
    // helper lives in exactly one testable place — and so adding a fourth
    // backend fails to compile here until that mapping is stated.
    let (result, dedup, v, folder_source) = match folder_source_kind(backend_kind) {
        FolderSourceKind::Labels => {
            let token = ensure_token(&state, &account_id).await?;
            let label_map = cached_label_map(&state, &account_id, &token).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let v = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
            let (mut result, mut dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;

            // Self-heal: stale label cache after Apple Notes recreates labels.
            if result.is_empty() {
                log!("list_notes: zero results — checking if label cache is stale");
                if let Ok(fresh) = gmail::get_label_map(&token).await {
                    let changed = fresh != label_map;
                    {
                        let mut states = state.account_states.lock().unwrap();
                        let entry = states.entry(account_id.clone()).or_default();
                        entry.label_map_cache = Some((fresh.clone(), std::time::Instant::now()));
                    }
                    if changed {
                        log!("list_notes: label map changed, retrying");
                        let v2 = vertical_from_parts(&state, &account_id, token.clone(), fresh.clone())?;
                        let (notes, fresh_dedup) = v2.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
                        result = notes;
                        dedup = fresh_dedup;
                    }
                }
            }

            // Build the vertical again (v was consumed above) for the common tail.
            // vertical_from_parts is cheap — no network, just wraps the parts.
            let v_tail = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
            // `reconcile_folders_from_labels` re-derives the Notes/* paths from
            // the label map itself, so only the map travels to the tail.
            (result, dedup, v_tail, FolderSource::Labels(label_map))
        }
        FolderSourceKind::Vertical => {
            // Exchange: `vertical_for` does the token work, there is no label
            // map, and the folder list is only obtainable from the vertical — so
            // the tail reconciles through it rather than through a path list,
            // which would throw away the ids (gotcha #12).
            let v = vertical_for(&state, &account_id).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let (result, dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
            (result, dedup, v, FolderSource::Vertical)
        }
        FolderSourceKind::Paths => {
            // LocalFs: no token or label_map needed.
            let v = vertical_for(&state, &account_id).await?;
            let cache_map = cache_by_msg_id(&state, &account_id);
            let (result, dedup) = v.list_all_notes(&cache_map).await.map_err(|e| e.to_string())?;
            // Collect folder paths from the filesystem for reconciliation.
            let fs_folders: Vec<String> = v.list_folders().await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path)
                .collect();
            (result, dedup, v, FolderSource::Paths(fs_folders))
        }
    };
    // The branch that fetched the notes must have built the payload the chosen
    // helper consumes — a mismatch would silently reconcile from the wrong
    // source. Cheap enough to assert on every call rather than only in tests.
    debug_assert_eq!(folder_source.kind(), folder_source_kind(backend_kind));

    // Surface the dedup summary so the sidebar can show a passive "N dup"
    // indicator. Replace (not accumulate) — each list_notes call is a
    // complete observation; after cleanup_orphans runs the next call will
    // report fewer duplicates. The divergent duplicates travel on to be
    // preserved below, not into the stored summary.
    let mut dedup = dedup;
    let incomplete = dedup.incomplete;
    // Before reconcile moves the rows: see `divergent_worth_keeping`.
    let divergent = divergent_worth_keeping(&state.db, &account_id, std::mem::take(&mut dedup.divergent));
    // A complete listing saw every duplicate the account has, so it replaces
    // the set the next sign-in index leaves out of the counts (migration 31).
    if let Err(e) = state.db.record_duplicate_ids(&account_id, &std::mem::take(&mut dedup.dropped_ids), !incomplete) {
        log!("list_notes: record_duplicate_ids failed: {}", e);
    }
    state.dup_stats.lock().unwrap().insert(account_id.clone(), dedup);

    // Tag each note with its account so the frontend can scope folder views.
    let mut result = result;
    for n in &mut result {
        n.account_id = Some(account_id.clone());
    }

    // Reconcile each fetched note against the cache. reconcile_one handles
    // the full state machine: insert fresh on unknown uuid, refresh clean,
    // detect conflicts when both sides changed, leave deletion-pending
    // alone, etc. See reconcile_one comments for the full decision table.
    //
    // After the per-row pass, prune clean cache rows whose uuid didn't
    // come back from remote — those notes are gone. Only safe here
    // (full sweep), not in list_notes_in_folder (scoped fetch).
    {
        for n in &result {
            reconcile_one(&state, &account_id, n);
        }
        // Duplicates the dedupe dropped although they hold text the kept copy
        // lacks become notes of their own (`reconcile::preserve_divergent_db`).
        preserve_divergent(&state, &account_id, &divergent);
        // The other half of the pin trace. `list_cached_notes` reports what the
        // cache holds when the list paints; this reports what the fetch carried
        // and what survived reconcile. Silence from this line means list_notes
        // did not run at all, which is itself the answer.
        log!(
            "list_notes: {} reconciled {} note(s), {} pinned on the wire",
            account_id,
            result.len(),
            result.iter().filter(|n| n.pinned).count()
        );
        // ── Pin sidecar pull reconciliation ────────────────────────────────
        //
        // List every pin sidecar via the vertical (which resolves meta_label
        // internally, returning an empty vec if the label/dir isn't present
        // yet), apply each to the cache via apply_remote_pin, then clear pin
        // on any locally-pinned row whose uuid didn't appear in the listing.
        //
        // Skipped silently on errors so a transient backend glitch doesn't
        // break the entire list_notes path — pin sync is UX-only.
        //
        // BEFORE the restamp below, which copies `pinned` from the row into what
        // this command returns: run after it, a pin this pass applied reached
        // SQLite but not the screen (2026-11-12, S23 FE: pinned on the Mac, ⟳ on
        // the phone, still unpinned).
        {
            match v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string()) {
                Ok(Some(sidecars)) => {
                    let (_, cleared) = apply_pin_sidecars_db(&state.db, &account_id, &sidecars);
                    if cleared > 0 {
                        log!("list_notes: cleared {} pin(s) absent from meta_label", cleared);
                    }
                }
                Ok(None) => { /* meta store not present yet — skip; do NOT clear pins */ }
                Err(e) => log!("list_notes: list_meta_sidecars failed: {}", e),
            }
        }

        // Restamp local_version from the post-reconcile DB row. Every note
        // in `result` was parsed off the wire (Gmail/LocalFs) with
        // local_version: 0 baked in — not the row's real value, since
        // upsert_from_remote's ON CONFLICT DO UPDATE never touches
        // local_version. A cache MISS (remote id rotated — another device
        // pushed, or this device's own worker pushed mid-listing) means a
        // note with a real nonzero local_version still gets fresh-parsed
        // with 0 here; without this restamp that 0 reaches the frontend and
        // every subsequent save for that note CASes against a stale
        // expected value, producing a permanent false conflict.
        for n in &mut result {
            if let Ok(Some(cached)) = state.db.get(&n.uuid, &account_id) {
                overlay_cached_row(n, &cached);
            }
        }
        let keep: Vec<String> = result.iter().map(|n| n.uuid.clone()).collect();
        // The count that separates "the fetch lost notes" from "the cache did".
        // A live iCloud account fetched 776 and cached 188; without this line
        // the difference is invisible from the log.
        match state.db.list_notes(&account_id) {
            Ok(rows) => log!(
                "list_notes: {} note(s) fetched, {} row(s) in the cache after reconcile",
                result.len(),
                rows.len()
            ),
            Err(e) => log!("list_notes: could not count cached rows: {e}"),
        }
        prune_after_listing_db(&state.db, &account_id, &keep, incomplete);
        if incomplete {
            carry_unread_cached_notes_db(&state.db, &account_id, None, &mut result);
        }
        // Tags are keyed by uuid; once a note is pruned its tag rows would
        // be orphans. Pre-tombstone behaviour was to hard-delete them, which
        // races with Gmail's eventual consistency: a transient omission in
        // one listing would silently destroy the user's tags. Now we move
        // them to `tag_tombstones` so a note that reappears on the next
        // sweep gets its tags restored automatically (via the restore step
        // inside upsert_from_remote). Tombstones older than TOMBSTONE_TTL_MS
        // are dropped here too — at that age the disappearance is real.
        match state.db.tombstone_orphan_tags(&account_id) {
            Ok(n) if n > 0 => log!("list_notes: tombstoned {} tag row(s) for pruned notes", n),
            Ok(_) => {}
            Err(e) => log!("list_notes: tombstone orphan tags failed: {}", e),
        }
        match state.db.sweep_old_tombstones(&account_id, orphan_commands::TOMBSTONE_TTL_MS) {
            Ok(n) if n > 0 => log!("list_notes: swept {} expired tag tombstone(s)", n),
            Ok(_) => {}
            Err(e) => log!("list_notes: sweep tombstones failed: {}", e),
        }
    }

    // ── Folder pull reconciliation ─────────────────────────────────────
    // Gmail: label_map is authoritative — upsert + prune via label helper.
    // LocalFs: filesystem folder list is authoritative — upsert + prune
    //          via path helper (filesystem is strongly-consistent, so
    //          pruning is safe unlike Gmail cold-start).
    // Microsoft: the vertical's message-derived folder list is the only one
    //          there is, and it is the only carrier of the folder ids.
    match folder_source {
        FolderSource::Labels(label_map) => {
            reconcile_folders_from_labels(&state.db, &account_id, &label_map, true);
        }
        FolderSource::Paths(folder_paths) => {
            reconcile_folders_from_paths(&state.db, &account_id, &folder_paths, true);
        }
        FolderSource::Vertical => {
            // `prune = true` here is a deliberate ruling, not an oversight, and
            // it has a user-visible cost worth stating plainly: **a Microsoft
            // folder row is deleted the moment its last note leaves it, so a
            // folder the user emptied vanishes from their sidebar** — which
            // reads like a sync bug.
            //
            // Do not "fix" this by flipping it to false. On this backend a
            // folder is observable ONLY through a note that points at it
            // (gotcha #12: `/me/mailFolders` omits the Notes tree, `GET
            // /mailFolders/{id}` 404s, and `childFolders` always returns
            // empty). Graph therefore cannot distinguish "folder emptied" from
            // "folder deleted" — both look identical from here. Pruning matches
            // exactly what is observable, and the emptied-folder case
            // self-corrects the instant a note is filed there again. A retained
            // stale row would never self-correct, because nothing would ever
            // arrive to contradict it.
            //
            // Best-effort, like the two above: a folder-scan failure must not
            // fail the whole pull, whose notes have already been reconciled.
            if let Err(e) = reconcile_folders_from_vertical(&state.db, &account_id, &*v, true).await {
                log!("list_notes: folder reconcile failed for {}: {}", account_id, e);
            }
        }
    }

    // D8 fix: drop any uuid the cache says is deleted_pending. Gmail's
    // search index can lag the worker's trash calls by a few seconds; in
    // that window the user just told us to delete a note but Gmail still
    // returns it. Without this filter the frontend merge re-introduces
    // it as a "ghost" entry in $notes — SQLite says gone, UI shows it.
    //
    // Cheap: one indexed SELECT against the partial sync_state index.
    // Must run AFTER reconcile_one + prune_clean, because those operate
    // on the raw fetch result. The filter only shapes what we return to
    // the frontend.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if !deleted.is_empty() {
            let drop: std::collections::HashSet<String> = deleted.into_iter().collect();
            let before = result.len();
            result.retain(|n| !drop.contains(&n.uuid));
            let dropped = before - result.len();
            if dropped > 0 {
                log!("list_notes: filtered {} ghost(s) from Gmail fetch", dropped);
            }
        }
    }

    // AFTER the ghost filter, not before: that filter drops rows the user has
    // locally deleted, and a blocked note is the opposite case — present
    // locally, absent remotely, and wanted.
    append_unpushed_notes(&state.db, &account_id, None, &mut result);

    log!(
        "list_notes: returning {} notes for {}",
        result.len(),
        account_id
    );
    Ok(result)
}

/// A local, account-scoped snapshot; a successful SQLite save is not a push.
#[derive(serde::Serialize)]
pub struct NotePersistence {
    pub(crate) account_id: String,
    pub(crate) uuid: String,
    pub(crate) local_version: i64,
    pub(crate) sync_state: db::SyncState,
    pub(crate) push_blocked_reason: Option<String>,
    pub(crate) push_blocked_by_remote: bool,
}

pub(crate) fn note_persistence_db(db: &db::Db, account_id: &str, uuid: &str) -> Result<Option<NotePersistence>, String> {
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| e.to_string())?;
    Ok(db.get(&uuid, account_id).map_err(|e| e.to_string())?.map(|n| NotePersistence {
        account_id: n.account_id, uuid: n.uuid, local_version: n.local_version,
        // Pin sidecars have a separate dirty bit and do not bump the content
        // version. A clean body alone must not claim the entire note synced.
        sync_state: if n.pin_dirty && n.sync_state == db::SyncState::Clean { db::SyncState::Dirty } else { n.sync_state },
        push_blocked_reason: n.push_blocked_reason,
        push_blocked_by_remote: n.push_blocked_by_remote,
    }))
}

/// Bring a fetched note in line with its post-reconcile cache row before it
/// reaches the frontend. `local_version` always comes from the row (the wire
/// carries 0 — see the restamp comment in `list_notes`), and so do the pin and
/// the folder (see below). A `dirty` row also
/// lends its title and body: its unpushed content is newer than the remote's
/// by construction (a remote change to a dirty row becomes a conflict), and a
/// listing that paired the remote's OLD body with the row's NEW version lied
/// to both version checks — a save made over it passed the compare-and-swap
/// and overwrote an agent's unpushed append, and Mark reviewed certified the
/// append while the editor showed the old body (PR #139 review, 2026-10-08).
pub(crate) fn overlay_cached_row(n: &mut gmail::Note, cached: &db::CachedNote) {
    n.local_version = cached.local_version;
    // The folder is the row's, never the wire's: a listing read before the
    // worker pushed a move still finds the message in the label it LEFT, and
    // after it `is_older_copy` keeps a clean row against that stale copy
    // (gotcha #47). Taking the wire's put the note back in its old folder until
    // the next read — moved, shown, gone, back (2026-11-18).
    n.label = cached.label.clone();
    // The pin is the row's, never the wire's: a Gmail message carries none
    // (a new message id parses as `pinned: false`), a cache hit carries the
    // value from before this pull's pin pass, and on iCloud
    // `upsert_from_remote` has already written the remote's pin to the row.
    n.pinned = cached.pinned;
    if cached.sync_state == db::SyncState::Dirty {
        n.title = cached.title.clone();
        n.body_html = cached.body_html.clone();
    }
}

/// The open note as SQLite holds it right now, through a pre-rekey alias
/// (gotcha #16); `None` for a missing or pending-delete row. Pure local read.
/// The editor re-reads through this when the trust chip advances past the body
/// it rendered, so Mark reviewed never certifies text that is not on screen:
/// the full listing that would otherwise bring the body waits on the remote.
pub(crate) fn cached_note_db(db: &db::Db, account_id: &str, uuid: &str) -> Result<Option<gmail::Note>, String> {
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| e.to_string())?;
    Ok(db.get(&uuid, account_id).map_err(|e| e.to_string())?
        .filter(|n| n.sync_state != db::SyncState::DeletedPending)
        .map(|n| n.to_frontend_note()))
}

/// `merge_onto_row_db`'s answer: the merge, and the row's `local_version` to
/// save it against.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedRowMerge {
    #[serde(flatten)]
    pub(crate) merge: merge::EditorMerge,
    pub(crate) local_version: i64,
}

/// The editor's text merged with the row that moved under it
/// (`merge::merge_onto_moved_row`), or `None` and the editor keeps both. Pure
/// local read: the editor saves the merge itself, through `save_note`'s
/// version check, so a row that moves again in between is refused as before.
/// A title the row changed is not merged (the title is the body's first line
/// on Gmail, gotcha #11, and the editor would overwrite it).
pub(crate) fn merge_onto_row_db(
    db: &db::Db,
    account_id: &str,
    uuid: &str,
    title: &str,
    base_body: &str,
    body_html: &str,
) -> Result<Option<MovedRowMerge>, String> {
    let uuid = db.resolve_note_uuid(uuid, account_id).map_err(|e| e.to_string())?;
    let Some(row) = db.get(&uuid, account_id).map_err(|e| e.to_string())? else { return Ok(None) };
    if row.sync_state == db::SyncState::DeletedPending || row.title != title {
        return Ok(None);
    }
    Ok(merge::merge_onto_moved_row(base_body, body_html, &row.body_html)
        .map(|merge| MovedRowMerge { merge, local_version: row.local_version }))
}

/// Local-first delete. Marks the row `deleted_pending` so the frontend
/// stops showing it, then the background worker handles the remote trash
/// call. If the note was a brand-new local-only note (no remote_version
/// yet), the worker just removes the row — no remote call needed.
///
/// A uuid is required. The old fallback — trash straight on the remote by
/// Gmail message `id`, with no SQLite write first — is gone (CLAUDE.md D7):
/// it broke the local-first doctrine and nothing reached it, because the one
/// caller (`controllers/noteMutations.ts`) invokes this only with a real,
/// non-`tmp:` uuid. `id` is still accepted because that caller still sends
/// it; it is ignored.
#[tauri::command]
pub async fn delete_note(
    account_id: String,
    id: Option<String>,
    uuid: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let _ = id;
    delete_note_db(&state.db, &account_id, uuid.as_deref())
}

/// `delete_note` past its write guard, free of `AppState` so a test can run
/// it on a temp DB. SQLite only — never the remote.
pub(crate) fn delete_note_db(db: &db::Db, account_id: &str, uuid: Option<&str>) -> Result<(), String> {
    let uuid = uuid
        .filter(|s| !s.is_empty() && !s.starts_with("tmp:"))
        .ok_or_else(|| "delete_note: a note uuid is required".to_string())?;
    db.mark_deleted(uuid, account_id).map_err(|e| e.to_string())?;
    log!("delete_note: marked deleted_pending for uuid={}", uuid);
    Ok(())
}

/// Batch move — relabels every uuid in `uuids` to `target_label` in one
/// SQLite transaction. Each touched row goes dirty (or conflict, per the
/// state machine) and the sync worker pushes the moves to Gmail on its
/// next ticks. Returns the count of rows actually updated.
///
/// Why a batch primitive instead of looping save_note N times: the loop
/// shape lets the user see partial states (3 of 7 notes moved) while the
/// IPC awaits queued behind each other, and serializes the SQLite writes
/// one Mutex acquisition per note. The batch is atomic — either every
/// row's label moves or none does — and acquires the connection Mutex
/// once.
#[tauri::command]
pub async fn move_notes_batch(
    account_id: String,
    uuids: Vec<String>,
    target_label: String,
    ai_result_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let _policy_gate = ai_result_id.as_ref().map(|_| state.ai_policy.gate.lock().unwrap());
    if let Some(id) = &ai_result_id { validate_ai_result(&state, &account_id, id)?; }
    let touched = state.db
        .move_notes_batch(&account_id, &uuids, &target_label, &actor_for(&state, ai_result_id.as_deref()))
        .map_err(|e| e.to_string())?;
    log!(
        "move_notes_batch: account={} touched={}/{} target='{}'",
        account_id, touched, uuids.len(), target_label
    );
    Ok(touched)
}

/// Batch delete — marks every uuid in `uuids` as `deleted_pending` in one
/// SQLite transaction. The sync worker trashes them on Gmail in the
/// background. Same atomicity argument as move_notes_batch.
#[tauri::command]
pub async fn delete_notes_batch(
    account_id: String,
    uuids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let touched = state.db
        .delete_notes_batch(&account_id, &uuids)
        .map_err(|e| e.to_string())?;
    log!(
        "delete_notes_batch: account={} touched={}/{}",
        account_id, touched, uuids.len()
    );
    Ok(touched)
}

/// Toggle the pin column on one note. Pure local-first: a single SQLite
/// UPDATE, no Gmail involvement, no sync_state transition. The worker
/// has nothing to push because pin doesn't round-trip through the email
/// backend (Apple stores pin in iCloud metadata Jodd can't reach via
/// Gmail). Returns immediately after the row write commits.
#[derive(serde::Serialize)]
pub struct AttachmentDto {
    pub(crate) content_id: String,
    pub(crate) mime_type: String,
    pub(crate) data_uri: String,
    /// Bytes the cache holds for it, every type (the date tooltip's
    /// "on this device" line); the bytes themselves still cross only for images.
    pub(crate) size: usize,
}

/// Return a note's attachments for the editor. `data_uri` is populated only for
/// IMAGES (what renders inline); other types (PDF/zip/…) come back as cid+mime
/// with an empty data_uri — enough for the editor to (a) leave their <object>
/// placeholder and (b) detect a stale/over-stripped body (attachments exist but
/// the body references none) and self-heal. Non-image bytes never cross IPC
/// (a 33 MB zip has no inline rendering).
#[tauri::command]
pub async fn get_note_attachments(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<Vec<AttachmentDto>, String> {
    let atts = state
        .db
        .list_attachments(&account_id, &uuid)
        .map_err(|e| e.to_string())?;
    Ok(atts
        .into_iter()
        .map(|a| {
            let data_uri = if a.mime_type.starts_with("image/") {
                crate::mime822::data_uri(&a.mime_type, &a.data)
            } else {
                String::new()
            };
            AttachmentDto {
                data_uri,
                size: a.data.len(),
                content_id: a.content_id,
                mime_type: a.mime_type,
            }
        })
        .collect())
}

pub(crate) fn rfc2822_ms(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc2822(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// List notes in Gmail Trash for the "Recently Deleted" view. Filters out
/// edit-revisions (a trashed message whose uuid still has a LIVE cache row is
/// just an old revision from a save's insert-new+trash-old, not a user
/// deletion), then dedups by uuid keeping the newest, newest-first.
#[tauri::command]
pub async fn list_trashed_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::TrashedNote>, String> {
    let v = vertical_for(&state, &account_id).await?;
    let trashed = v.list_trashed().await.map_err(|e| e.to_string())?;

    let mut by_uuid: HashMap<String, gmail::TrashedNote> = HashMap::new();
    for t in trashed {
        // A trashed message is a mere edit-revision ONLY if a DIFFERENT (live)
        // message with this uuid still exists — i.e. the cache's current id for
        // the uuid points at some OTHER message. If the cache id IS this trashed
        // id (the note's live message got trashed and the cache just hasn't
        // pruned yet), it's a GENUINE deletion → show it. Earlier this checked
        // only "row exists", which wrongly hid genuinely-deleted notes whose
        // stale cache row lingered.
        let is_revision = matches!(
            state.db.get(&t.uuid, &account_id),
            Ok(Some(row)) if row.id != t.id
        );
        if is_revision {
            continue;
        }
        match by_uuid.get(&t.uuid) {
            Some(existing) if rfc2822_ms(&existing.date) >= rfc2822_ms(&t.date) => {}
            _ => {
                by_uuid.insert(t.uuid.clone(), t);
            }
        }
    }
    // Secondary key on uuid: `by_uuid.into_values()` walks a HashMap, whose
    // iteration order is randomized per construction, so ties on `date` (e.g.
    // a batch of test notes created in the same second) would otherwise
    // reorder themselves on every refresh even though nothing changed.
    let mut out: Vec<gmail::TrashedNote> = by_uuid.into_values().collect();
    out.sort_by(|a, b| {
        rfc2822_ms(&b.date).cmp(&rfc2822_ms(&a.date)).then_with(|| a.uuid.cmp(&b.uuid))
    });
    Ok(out)
}

/// Restore a trashed note: untrash the Gmail message (its Notes label is
/// retained, so it returns to its folder) and clear any local deleted_pending
/// row so the worker doesn't re-trash it. The next list reconcile re-inserts
/// the untrashed note as clean.
/// How a backend puts a trashed note back where the user wants it.
///
/// `restore_note` used to be `if Gmail { … } else { …LocalFs… }`, which was
/// true while exactly two backends had a trash and became a live defect the
/// moment a third did: the `else` ran LocalFs's filename decode over a
/// CloudKit `recordName`. Stated per backend, exhaustively, so a fifth has to
/// answer rather than inherit somebody else's mechanism.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreKind {
    /// Gmail: trash is a LABEL, so untrash removes it and the note reappears
    /// under the `Notes/*` label it kept the whole time; an explicit target is
    /// a move afterwards.
    UntrashThenMove,
    /// LocalFs: the trash filename encodes the original relpath, so untrash
    /// alone puts the file back exactly where it was.
    UntrashToEncodedPath,
    /// iCloud: a note's folder reference IS its location, so being in the Trash
    /// means that reference was overwritten — there is nothing to un-delete,
    /// only somewhere to move it to. And nothing measured says where it came
    /// from, so that somewhere has to come from the user
    /// (`TrashedNote::original_known` is false here, and the UI offers only
    /// "Restore to…").
    MoveOutOfTrash,
}

/// `None` = this backend has no trash to restore from — `Capabilities::
/// has_trash` is false and the UI shows no view at all.
pub(crate) fn restore_kind(kind: accounts::BackendKind) -> Option<RestoreKind> {
    match kind {
        accounts::BackendKind::Gmail => Some(RestoreKind::UntrashThenMove),
        accounts::BackendKind::LocalFs => Some(RestoreKind::UntrashToEncodedPath),
        accounts::BackendKind::ICloud => Some(RestoreKind::MoveOutOfTrash),
        // Measured: an Apple-side delete leaves nothing in Deleted Items, so
        // there is genuinely nothing to restore from.
        accounts::BackendKind::Microsoft => None,
        // Same shape as LocalFs: the trashed file's name encodes its original
        // relpath, so untrash alone puts it back exactly where it was.
        accounts::BackendKind::Ssh => Some(RestoreKind::UntrashToEncodedPath),
    }
}

#[tauri::command]
pub async fn restore_note(
    account_id: String,
    uuid: String,
    id: String,
    original_label: String,
    target_label: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Relocate)?;
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };

    let Some(how) = restore_kind(backend_kind) else {
        return Err(format!(
            "{account_id} has no Recently Deleted to restore from"
        ));
    };

    if how == RestoreKind::MoveOutOfTrash {
        // One write: the record's `Folder` reference is what says where the
        // note lives, so moving it out of the Trash IS the restore. A target is
        // required rather than defaulted — see `RestoreKind::MoveOutOfTrash`.
        let target = target_label.filter(|t| !t.is_empty()).ok_or_else(|| {
            "this account cannot tell which folder the note came from — choose one to \
             restore it into"
                .to_string()
        })?;
        let v = vertical_for(&state, &account_id).await?;
        v.move_note(&id, std::slice::from_ref(&target), &[])
            .await
            .map_err(|e| e.to_string())?;
        let _ = state.db.delete(&uuid, &account_id);
        log!("restore_note[{account_id}]: RESTORED id={id} uuid={uuid} into {target}");
        return Ok(());
    }

    if how == RestoreKind::UntrashThenMove {
        let token = ensure_token(&state, &account_id).await?;
        let label_map = cached_label_map(&state, &account_id, &token).await?;
        let v = vertical_from_parts(&state, &account_id, token.clone(), label_map.clone())?;
        v.untrash(&id).await.map_err(|e| e.to_string())?;
        let id_of = |name: &str| {
            label_map
                .iter()
                .find(|(_, n)| n.as_str() == name)
                .map(|(lid, _)| lid.clone())
        };
        // Optionally move to a chosen folder (like a restore-as-move): remove the
        // original Notes label, add the target. The picker only offers existing
        // folders for an explicit "restore to X".
        //
        // If the note's folder was deleted while it sat in Trash, untrash alone
        // leaves the message with NO Notes-tree label at all — invisible to
        // every list/index path. `original_label` can't detect this on its own:
        // list_trashed_notes' pick_notes_label already falls back to the
        // synthetic string "Notes" when no real label matched, and "Notes" always
        // resolves in id_of (the root label exists), so comparing against
        // `original_label` here would silently miss every orphaned case. Instead
        // check the message's actual current labels (ground truth, post-untrash).
        // Propagate a fetch failure rather than defaulting has_notes_label to
        // false: a false default would wrongly force even a perfectly-intact
        // restore into the root "Notes" folder instead of leaving it in place.
        let current_labels = gmail::get_message_label_ids(&token, &id).await.map_err(|e| e.to_string())?;
        let has_notes_label = current_labels.iter().any(|lid| {
            label_map
                .get(lid)
                .is_some_and(|n| n == "Notes" || n.starts_with("Notes/"))
        });
        let effective_target = target_label
            .filter(|t| *t != original_label)
            .or_else(|| (!has_notes_label).then(|| "Notes".to_string()));
        if let Some(target) = effective_target {
            let target_id = match id_of(&target) {
                Some(tid) => tid,
                None => v.ensure_folder(&target).await.map_err(|e| e.to_string())?.id,
            };
            let remove: Vec<String> = id_of(&original_label).into_iter().collect();
            v.move_note(&id, &[target_id], &remove).await.map_err(|e| e.to_string())?;
        }
    } else {
        debug_assert_eq!(how, RestoreKind::UntrashToEncodedPath);
        // LocalFs: no token/label_map needed. untrash decodes the trash
        // filename back to the original relpath and restores the file there,
        // recreating the subfolder if needed.
        let v = vertical_for(&state, &account_id).await?;
        v.untrash(&id).await.map_err(|e| e.to_string())?;

        // After untrash the note lives at its original relpath
        // (trash_decode of the trash filename == original relpath).
        let encoded_basename = std::path::Path::new(&id)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let restored_id =
            crate::backend::localfs::transport::trash_decode(&encoded_basename);

        if let Some(target) = target_label.filter(|t| *t != original_label) {
            // User chose a different destination folder: move there after restore.
            v.move_note(&restored_id, &[target], &[original_label.clone()])
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let _ = state.db.delete(&uuid, &account_id);
    log!("restore_note[{}]: UNTRASHED id={} uuid={} (from {})", account_id, id, uuid, original_label);
    Ok(())
}

/// Full-text search (SQLite FTS5, Thai-aware) over title + plain-text body.
/// Scope is caller-controlled: pass `account_id`+`label` to search one folder,
/// `account_id` only for one account, or neither (null) to search EVERY account.
/// Pure SQLite read. Returns full notes so the UI can show + open results that
/// aren't loaded in memory.
#[tauri::command]
pub fn search_notes(
    account_id: Option<String>,
    label: Option<String>,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = state
        .db
        .search_notes(account_id.as_deref(), label.as_deref(), &query, &hidden_account_ids(&state))
        .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

#[derive(serde::Serialize)]
pub struct NoteConnections {
    pub(crate) outgoing: Vec<gmail::Note>,
    pub(crate) backlinks: Vec<gmail::Note>,
}

/// Fact-schema edges consumer: a note's [[wikilink]] connections — `outgoing`
/// (notes it links to, resolved by title) and `backlinks` (notes that link to
/// it). Pure SQLite read over the `edges` table (derived from bodies on write).
/// Read after every save, so off the main thread (`db_blocking`).
#[tauri::command]
pub async fn note_connections(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<NoteConnections, String> {
    crate::db_blocking(&state, move |db| {
        let outgoing = db.outgoing_links(&account_id, &uuid).map_err(|e| e.to_string())?;
        let backlinks = db.backlinks(&account_id, &uuid).map_err(|e| e.to_string())?;
        Ok(NoteConnections {
            outgoing: outgoing.iter().map(|c| c.to_frontend_note()).collect(),
            backlinks: backlinks.iter().map(|c| c.to_frontend_note()).collect(),
        })
    })
    .await
}

/// What a clicked `[[wikilink]]` resolved to: the note, plus the first line
/// of its body for a preview.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteLinkPreview {
    pub(crate) note: gmail::Note,
    pub(crate) first_line: String,
}

const LINK_PREVIEW_MAX_CHARS: usize = 140;

/// First non-empty line of the body's link text, trimmed and cut to
/// `LINK_PREVIEW_MAX_CHARS` on a char boundary (trailing whitespace the cut
/// exposes is trimmed too). Empty when there is none. The cached `body_html`
/// never carries the title line — Gmail and LocalFs strip it on decode and
/// iCloud cuts it (`the_body_never_keeps_the_title_line_it_cut`) — so the
/// first line here is already the line after the title (gotcha #11).
pub(crate) fn link_preview_first_line(body_html: &str) -> String {
    db::link_text(body_html)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| {
            let cut: String = l.chars().take(LINK_PREVIEW_MAX_CHARS).collect();
            cut.trim_end().to_string()
        })
        .unwrap_or_default()
}

/// Resolve a `[[wikilink]]` key to its note. Pure SQLite read — never the remote.
#[tauri::command]
pub fn resolve_note_link(
    account_id: String,
    key: String,
    state: State<'_, AppState>,
) -> Result<Option<NoteLinkPreview>, String> {
    let found = state
        .db
        .resolve_note_link(&account_id, &key)
        .map_err(|e| e.to_string())?;
    Ok(found.map(|c| NoteLinkPreview {
        first_line: link_preview_first_line(&c.body_html),
        note: c.to_frontend_note(),
    }))
}

/// URLs this note cites (`rel='cites'` edges). Pure SQLite read.
#[tauri::command]
pub fn note_citations(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    state.db.citations(&account_id, &uuid).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct DuplicateCitation {
    pub(crate) url: String,
    pub(crate) existing_note_uuid: String,
    pub(crate) existing_note_title: String,
}

/// Pre-flight check for Extract's duplicate-source warning: scan `source_text`
/// for URLs and report every one that's already cited by another note in this
/// account (excluding `exclude_uuid`, the append target if any). Called by the
/// frontend BEFORE `extract_note`/`append_extract_note` — a non-empty
/// result means the modal should show a soft warning with a "Continue
/// anyway" resubmit, never a hard block.
#[tauri::command]
pub fn check_duplicate_citations(
    account_id: String,
    source_text: String,
    exclude_uuid: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DuplicateCitation>, String> {
    let mut out = Vec::new();
    for url in db::extract_urls_decoded(&source_text) {
        if let Some(owner) = state
            .db
            .find_citation_owner(&account_id, &url, exclude_uuid.as_deref())
            .map_err(|e| e.to_string())?
        {
            out.push(DuplicateCitation {
                url,
                existing_note_uuid: owner.uuid,
                existing_note_title: owner.title,
            });
        }
    }
    Ok(out)
}

#[derive(serde::Serialize)]
pub struct LinkCandidate {
    pub(crate) uuid: String,
    pub(crate) title: String,
    pub(crate) label: String,
    pub(crate) slug: String,
}

/// Autocomplete for the `[[` link picker: notes whose title matches `query`,
/// each with its ready-to-insert slug (note_slug = title-slug + uuid8).
/// Called as the user types after `[[`, so off the main thread (`db_blocking`).
#[tauri::command]
pub async fn search_note_links(
    account_id: String,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<LinkCandidate>, String> {
    crate::db_blocking(&state, move |db| {
        let rows = db.search_titles(&account_id, &query, 8).map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(|(uuid, title, label)| {
                let slug = db::note_slug(&title, &uuid);
                LinkCandidate { uuid, title, label, slug }
            })
            .collect())
    })
    .await
}

#[cfg(test)]
mod localfs_listing_gate_tests {
    use super::localfs_listing_gate;
    use crate::backend::localfs::transport::ACCESS_LOST;

    #[test]
    fn a_non_localfs_account_is_never_gated() {
        assert_eq!(localfs_listing_gate(false, None), Ok(()));
        assert_eq!(localfs_listing_gate(false, Some(Ok(false))), Ok(()));
        assert_eq!(localfs_listing_gate(false, Some(Err("jni".into()))), Ok(()));
    }

    #[test]
    fn desktop_localfs_has_no_access_check_and_passes() {
        assert_eq!(localfs_listing_gate(true, None), Ok(()));
    }

    #[test]
    fn android_localfs_with_access_passes() {
        assert_eq!(localfs_listing_gate(true, Some(Ok(true))), Ok(()));
    }

    #[test]
    fn android_localfs_without_access_refuses_with_access_lost() {
        // MediaProvider may let read_dir succeed but filter it to the app's own
        // files, so a listing without All files access would be silently
        // partial and prune_clean would drop clean notes. Refuse before reading.
        assert_eq!(localfs_listing_gate(true, Some(Ok(false))), Err(ACCESS_LOST.to_string()));
    }

    #[test]
    fn a_failed_access_check_is_its_own_error_not_access_lost() {
        assert_eq!(localfs_listing_gate(true, Some(Err("plugin gone".into()))), Err("plugin gone".to_string()));
    }
}

// Scoped fetch: only the notes whose label is exactly `path`. Used by the
// frontend when the user has been focused on one folder long enough to
// warrant a refresh — far cheaper than fetching every Notes sub-label.
#[tauri::command]
pub async fn list_notes_in_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    // Before any read or prune — same gate as `list_notes`.
    check_localfs_listing_access(&state, backend_kind).await?;

    let cache_map = cache_by_msg_id(&state, &account_id);
    let result = if backend_kind == accounts::BackendKind::Gmail {
        let token = ensure_token(&state, &account_id).await?;
        let label_map = cached_label_map(&state, &account_id, &token).await?;
        // Verify the folder exists (or is locally-pending). The vertical's
        // list_notes_in_folder returns an empty vec when the folder isn't in its
        // label_map; we want a real "Folder not found" error for folders that don't
        // exist at all, and an empty list only for locally-pending (not yet pushed).
        if !label_map.values().any(|n| n == &path) {
            if let Ok(Some(_)) = state.db.get_folder(&account_id, &path) {
                log!(
                    "list_notes_in_folder: '{}' exists locally but not on Gmail yet — returning its unpushed notes",
                    path
                );
                // Nothing remote to list, but a note moved or saved into the
                // new folder is in SQLite there already.
                let mut result = Vec::new();
                append_unpushed_notes(&state.db, &account_id, Some(&path), &mut result);
                return Ok(result);
            }
            return Err(format!("Folder not found: {}", path));
        }
        let v = vertical_from_parts(&state, &account_id, token, label_map)?;
        v.list_folder_with_divergent(&path, &cache_map).await.map_err(|e| e.to_string())?
    } else {
        // LocalFs: no token/label_map — vertical handles a missing dir by returning empty.
        let v = vertical_for(&state, &account_id).await?;
        v.list_folder_with_divergent(&path, &cache_map).await.map_err(|e| e.to_string())?
    };
    let (mut result, summary) = result;
    let incomplete = summary.incomplete;
    // Before reconcile moves the rows: see `divergent_worth_keeping`.
    let divergent = divergent_worth_keeping(&state.db, &account_id, summary.divergent);
    // A folder listing saw part of the account: it only adds (migration 31).
    if let Err(e) = state.db.record_duplicate_ids(&account_id, &summary.dropped_ids, false) {
        log!("list_notes_in_folder: record_duplicate_ids failed: {}", e);
    }
    for n in &mut result {
        n.account_id = Some(account_id.clone());
    }
    // Same reconciliation as list_notes. The scoped prune drops clean
    // rows IN this label only (a per-folder fetch isn't authoritative
    // about other folders in the same account).
    {
        for n in &result {
            reconcile_one(&state, &account_id, n);
        }
        preserve_divergent(&state, &account_id, &divergent);
        // Restamp local_version from the post-reconcile DB row — same
        // rationale as list_notes above: a wire-parsed Note always carries
        // local_version: 0, which is not the row's real value.
        for n in &mut result {
            if let Ok(Some(cached)) = state.db.get(&n.uuid, &account_id) {
                overlay_cached_row(n, &cached);
            }
        }
        let keep: Vec<String> = result.iter().map(|n| n.uuid.clone()).collect();
        prune_folder_after_listing_db(&state.db, &account_id, &path, &keep, incomplete);
        if incomplete {
            carry_unread_cached_notes_db(&state.db, &account_id, Some(&path), &mut result);
        }
    }
    // D8 fix: drop ghosts whose local cache row is deleted_pending. See
    // list_notes for the full rationale — same race, same fix shape.
    // This is the path the 10s folder settle uses, so it's the primary
    // exposure surface for the bug after a delete.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if !deleted.is_empty() {
            let drop: std::collections::HashSet<String> = deleted.into_iter().collect();
            let before = result.len();
            result.retain(|n| !drop.contains(&n.uuid));
            let dropped = before - result.len();
            if dropped > 0 {
                log!(
                    "list_notes_in_folder: filtered {} ghost(s) from Gmail fetch for '{}'",
                    dropped, path
                );
            }
        }
    }
    // Scoped to this folder — see `append_unpushed_notes`. This is the path
    // the 2500 ms sweep uses, so it is the one that was dropping the note.
    append_unpushed_notes(&state.db, &account_id, Some(&path), &mut result);
    Ok(result)
}

// Force-refetch a single note's body straight from Gmail, bypassing the
// cache-aware fan-out in list_notes / list_notes_in_label. Use case: the
// user suspects their local cache is stale or corrupted for a specific
// note (long notes edited from multiple places, recovery after a bug fix,
// etc.) and wants to pull the authoritative content without invalidating
// the whole folder. Cheap: one messages.get round-trip + one DB upsert.
//
// Returns the fresh Note (post strip_leading_title, label-mapped) so the
// frontend can replace its in-memory copy without an extra list cycle.
#[tauri::command]
pub async fn refetch_note(
    account_id: String,
    id: String,
    state: State<'_, AppState>,
) -> Result<gmail::Note, String> {
    if id.is_empty() {
        return Err("refetch_note: empty id (note has no remote version yet)".into());
    }
    let v = vertical_for(&state, &account_id).await?;
    let mut note = v.fetch_note(&id).await.map_err(|e| e.to_string())?;
    note.account_id = Some(account_id.clone());
    log!(
        "refetch_note: uuid={} id={} body_len={}",
        note.uuid, note.id, note.body_html.len()
    );
    // D8 fix: don't return ghosts. If the user marked this note for
    // deletion locally, fetching from Gmail would hand back a note that
    // we logically consider gone — same shape as the list_notes_in_folder
    // case, just for a single message. The reconcile_one call below
    // would correctly skip the upsert (DeletedPending branch), but the
    // frontend would still receive the note and show it. Refuse instead
    // so the caller surfaces a meaningful "already deleted" state.
    if let Ok(deleted) = state.db.list_deleted_pending_uuids(&account_id) {
        if deleted.iter().any(|u| u == &note.uuid) {
            return Err(format!(
                "refetch_note: uuid={} is marked deleted locally — refusing to resurrect",
                note.uuid
            ));
        }
    }
    // Upsert through the same reconcile path list_notes uses, so dirty/
    // conflict states are honored consistently (we don't blindly stomp local
    // edits — see reconcile_one for the conflict-copy semantics).
    reconcile_one(&state, &account_id, &note);
    // `note` was parsed straight off the wire and carries local_version: 0
    // (backend/gmail/wire.rs, backend/localfs/decode.rs), which is NOT the
    // row's real local_version — upsert_from_remote's ON CONFLICT DO UPDATE
    // deliberately never touches local_version. If we returned `note` as-is,
    // the frontend would adopt 0 as its "last known" version and every
    // subsequent save would CAS against a stale expected value, producing a
    // permanent false conflict on a note nobody else touched. Restamp from
    // the post-reconcile DB row — the single source of truth for this field.
    if let Ok(Some(cached)) = state.db.get(&note.uuid, &account_id) {
        overlay_cached_row(&mut note, &cached);
    }
    Ok(note)
}

// Read-only fetch of a trashed message's full content, for the "Recently
// Deleted" preview pane. Deliberately does NOT call reconcile_one or touch
// SQLite at all: the note is gone from the cache by design (deleted_pending
// was already pruned), and upserting it back in would resurrect a note the
// user just deleted. This is purely "show me what this used to say" —
// explicit user click, one messages.get round-trip, nothing persisted.
#[tauri::command]
pub async fn get_trashed_note_preview(
    account_id: String,
    id: String,
    state: State<'_, AppState>,
) -> Result<gmail::Note, String> {
    if id.is_empty() {
        return Err("get_trashed_note_preview: empty id".into());
    }
    let v = vertical_for(&state, &account_id).await?;
    let mut note = v.fetch_note(&id).await.map_err(|e| e.to_string())?;
    note.account_id = Some(account_id);
    Ok(note)
}

/// Pull pin state from this account's meta_label and apply to the local
/// cache. The frontend triggers this on cold start (after the index pass
/// completes) so a Jodd instance signed into a Gmail account that another
/// Jodd instance has been pinning notes on sees the pins as soon as
/// possible — without having to wait for the user to click "All" or
/// trigger a full list_notes.
///
/// Same reconciliation as list_notes (`apply_pin_sidecars_db`): pin what
/// the sidecars name, unpin clean rows they don't. A sidecar for a note not
/// pulled here yet is left for that note's pull — never trashed. Errors on the
/// meta_label path are surfaced (not silently swallowed like in list_notes
/// where they'd break the note list) — the frontend can log them.
///
/// Lightweight: meta_label is one Gmail label, sidecar count is bounded
/// by "notes the user has pinned" which is typically <100. Each sidecar
/// only needs a Subject-header fetch, no body. Fast even on a 6k mailbox.
#[tauri::command]
pub async fn sync_pin_state(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    // Account must exist — error if not found (unlike list_notes which silently skips).
    if !state.accounts.lock().unwrap().iter().any(|a| a.id == account_id) {
        return Err(format!("Account not found: {}", account_id));
    }
    let v = vertical_for(&state, &account_id).await?;
    // list_sidecars returns Ok(None) if meta_label isn't on Gmail yet — not
    // an error; first pin push will ensure_label and the next sync_pin_state
    // call will find it. On None we skip pruning so we don't wipe locally-pinned
    // notes when the meta store hasn't been created yet (fresh install, sign-out/in).
    let sidecars_opt = v.list_sidecars(SidecarKind::Pin).await.map_err(|e| e.to_string())?;
    let Some(sidecars) = sidecars_opt else {
        log!("sync_pin_state: meta store absent for {} — skipping (no prune)", account_id);
        return Ok(0);
    };
    let (applied, cleared) = apply_pin_sidecars_db(&state.db, &account_id, &sidecars);
    log!(
        "sync_pin_state: account={} sidecars={} applied={} cleared={}",
        account_id, sidecars.len(), applied, cleared
    );
    Ok(applied + cleared)
}

/// Disabled no-op, kept only because the frontend still calls it on cold
/// start and account reactivation (App.svelte, Sidebar.svelte) and removing
/// the command would turn that into a runtime "command not found" instead
/// of the harmless Ok(0) it returns today. Tags have no sidecar to pull —
/// see the comment inside.
#[tauri::command]
pub async fn sync_tag_state(
    account_id: String,
    _state: State<'_, AppState>,
) -> Result<usize, String> {
    // DISABLED. Tags are now inline #hashtags in the note body (they round-trip
    // with Apple Notes) and are derived into note_tags on every note write
    // (reconcile_tags_from_body_conn). Reading the legacy Notes-Meta tag sidecar
    // would fight that model — a tag the user removed from the body would
    // reappear from a stale sidecar — so this pull is a no-op now.
    log!("sync_tag_state: disabled — tags are inline in the body now ({})", account_id);
    Ok(0)
}

/// Cheap account-wide index — every Notes message's id + label, no body
/// fetch. Returns in seconds even for a 6k mailbox. The frontend uses this
/// to render folder counts and a "loaded X of Y" indicator before bodies
/// arrive. Bodies are hydrated on-demand by `list_notes_in_folder` / full
/// `list_notes` calls — both already cache-aware (Phase B), so this index
/// pass costs nothing the next time around.
///
/// Folder reconciliation runs first, upsert-only, through whichever helper
/// `folder_source_kind` names for this backend — the same mapping `list_notes`
/// uses, so the two passes cannot disagree.
#[tauri::command]
pub async fn index_account(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::MessageIndex>, String> {
    let backend_kind = {
        let list = state.accounts.lock().unwrap();
        list.iter()
            .find(|a| a.id == account_id)
            .map(|a| a.backend_kind)
            .ok_or_else(|| format!("account {} not found", account_id))?
    };
    // Same dispatch as `list_notes`, through the same one-place mapping, so the
    // cold-start pass and the authoritative pull can never disagree about which
    // source a backend's folders come from.
    //
    // prune=false throughout: cold start adds folders but defers removal to the
    // authoritative list_notes pull, which sees a complete view.
    match folder_source_kind(backend_kind) {
        FolderSourceKind::Labels => {
            let token = ensure_token(&state, &account_id).await?;
            let label_map = cached_label_map(&state, &account_id, &token).await?;
            // Populate the folders cache from the full remote label set so EMPTY
            // folders (no notes) show in the sidebar on cold start. list_notes — the
            // only other folder-sync path — is not called on cold start, so without
            // this an empty label like `Notes/play2` stayed invisible until the user
            // navigated.
            reconcile_folders_from_labels(&state.db, &account_id, &label_map, false);
            let v = vertical_from_parts(&state, &account_id, token, label_map)?;
            let idx = v.list_index().await.map_err(|e| e.to_string())?;
            // The index lists message ids, so a Gmail duplicate counts as a
            // note until a listing has seen it (migration 31).
            Ok(drop_known_duplicates_db(&state.db, &account_id, idx))
        }
        FolderSourceKind::Vertical => {
            // Exchange folder ids exist nowhere but this listing — see
            // `reconcile_folders_from_vertical`. Best-effort like the LocalFs
            // arm: a folder-scan failure must not sink the whole index pass.
            let v = vertical_for(&state, &account_id).await?;
            if let Err(e) = reconcile_folders_from_vertical(&state.db, &account_id, &*v, false).await {
                log!("index_account: folder reconcile failed for {}: {}", account_id, e);
            }
            let idx = v.list_index().await.map_err(|e| e.to_string())?;
            // AFTER the reads, never before: `blocked_reason` reports what a
            // completed read learned, and asking a fresh instance returns None
            // by construction (Component H2).
            record_backend_block(&state, &account_id, v.blocked_reason());
            Ok(idx)
        }
        FolderSourceKind::Paths => {
            // LocalFs: no token/label_map needed — `vertical_for` resolves
            // root_dir from the account config, and the filesystem is the
            // source of truth for folders.
            let v = vertical_for(&state, &account_id).await?;
            let fs_folders: Vec<String> = v.list_folders().await
                .unwrap_or_default()
                .into_iter()
                .map(|f| f.path)
                .collect();
            reconcile_folders_from_paths(&state.db, &account_id, &fs_folders, false);
            v.list_index().await.map_err(|e| e.to_string())
        }
    }
}

/// Records a whole-account hard block a read just discovered — Component H2.
///
/// **The sign-in gate is not enough on its own.** Advanced Data Protection can
/// be switched on after the account exists, so an account that worked
/// yesterday can be unreadable today, and nothing about sign-in would ever run
/// again to notice. Every index pass re-stamps this, which also means the field
/// clears itself the moment the account becomes readable again — derived state,
/// re-derived on every pass, exactly as gotcha #4 requires.
///
/// **Writes only on a change.** `index_account` runs on cold start and on every
/// explicit re-index; rewriting `accounts.json` each time would be a file write
/// per refresh for a value that almost never moves.
///
/// The route back to the UI already exists and is deliberately reused rather
/// than invented: the frontend re-reads `list_accounts` on the poll the
/// Inactive group runs, and `Account` serializes whole — so the banner updates
/// with no new event channel. That is gotcha #6's lesson applied ahead of the
/// failure rather than after it.
pub(crate) fn record_backend_block(state: &State<'_, AppState>, account_id: &str, reason: Option<String>) {
    let _gate = state.ai_policy.gate.lock().unwrap();
    let mut list = state.accounts.lock().unwrap();
    let Some(acct) = list.iter_mut().find(|a| a.id == account_id) else { return };
    if acct.blocked_reason == reason {
        return;
    }
    match &reason {
        Some(r) => log!("account {account_id} is blocked: {r}"),
        None => log!("account {account_id} is no longer blocked"),
    }
    invalidate_ai(state, &state.app_handle);
    acct.blocked_reason = reason;
    if let Err(e) = accounts::save_accounts(&list) {
        log!("record_backend_block: save_accounts failed: {e}");
    }
}

/// Whether the local cache was rebuilt after a key-mismatch recovery and
/// the user should be prompted to re-index their Gmail accounts. Backed by
/// a marker file rather than in-memory state so it survives the recovery
/// happening inside `.setup()`, before any frontend command can run.
#[tauri::command]
pub fn needs_reindex_after_recovery() -> bool {
    paths::data_base()
        .map(|d| d.join("jodd").join("NEEDS_REINDEX").exists())
        .unwrap_or(false)
}

/// Clear the recovery marker once the user has re-indexed (or dismissed
/// the prompt).
#[tauri::command]
pub fn clear_reindex_marker() -> Result<(), String> {
    if let Some(dir) = paths::data_base() {
        let marker = dir.join("jodd").join("NEEDS_REINDEX");
        if marker.exists() {
            std::fs::remove_file(&marker).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Cache-first read scoped to one folder. Pure SQLite, no token refresh,
/// no label_map lookup, no Gmail round-trip — returns in sub-ms. This is
/// the doctrine-compliant navigation read: clicking a folder paints
/// immediately from the local replica. Reconciliation against Gmail is
/// the sweep tick's job (it calls `list_notes_in_folder` instead).
///
/// Returns notes whose label exactly equals `path`, excluding rows in
/// `deleted_pending`. A folder the user just created locally that has
/// no notes yet returns an empty vec — no "Folder not found" error,
/// even if the label hasn't been pushed to Gmail yet.
#[tauri::command]
pub async fn list_cached_notes_in_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let cached = state.db.list_notes_by_label(&account_id, &path).map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Read the local replica for one account. Used by the frontend on cold
/// start to paint the UI before the network fetch returns — this is the
/// "instant launch" path. Always succeeds (returns an empty vec if the
/// cache has never been populated). Pure local read, no network.
#[tauri::command]
pub async fn list_cached_notes(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let db = state.db.clone();
    let cached = db.list_notes(&account_id).map_err(|e| e.to_string())?;
    // The pinned count rides along because this is the exact moment the list
    // paints: if the cache holds the pin here and the UI shows no PINNED
    // heading, the bug is in the paint; if it holds none, the pin never
    // reached SQLite and the walk finding it is beside the point.
    log!(
        "list_cached_notes: {} returned {} cached notes ({} pinned)",
        account_id,
        cached.len(),
        cached.iter().filter(|n| n.pinned).count()
    );
    Ok(cached.into_iter().map(|c| c.to_frontend_note()).collect())
}

/// Return folder paths from the LOCAL CACHE only. Sub-ms read. Includes
/// folders in any non-deleted state (clean / dirty_new / dirty_renamed)
/// so newly-created-but-not-yet-pushed folders are visible immediately.
///
/// Reconciliation with Gmail happens inside `list_notes` (which has the
/// authoritative label_map) — no network call needed here.
///
/// Returns the implicit "Notes" root if the cache doesn't have it yet
/// (first-run before any sync). This keeps the Sidebar from being
/// empty on the very first cold start.
#[tauri::command]
pub async fn list_folders(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let cached = state.db.list_folders(&account_id).map_err(|e| e.to_string())?;
    let mut paths: Vec<String> = cached.into_iter().map(|f| f.path).collect();
    // Ensure the "Notes" root is always present, even on a cold first run
    // before any sync has populated the folders table.
    if !paths.iter().any(|p| p == "Notes") {
        paths.push("Notes".to_string());
    }
    paths.sort();
    Ok(paths)
}

/// Folder kinds for one account — `(path, kind)` pairs, where kind is one of
/// `'user' | 'system_workflow' | 'smart_query'`. Companion to `list_folders`
/// for the Sidebar Folders/Workflows split (Task 16). Returning a separate
/// command keeps `list_folders` callers (NoteContextMenu, etc.) unchanged.
/// Paths not present in the cache (e.g. the implicit "Notes" root before any
/// sync) are absent here — the frontend treats absence as 'user'.
#[tauri::command]
pub async fn list_folder_kinds(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<(String, String)>, String> {
    let cached = state.db.list_folders(&account_id).map_err(|e| e.to_string())?;
    Ok(cached.into_iter().map(|f| (f.path, f.kind)).collect())
}

/// Notes with zero incoming [[wikilink]] backlinks. Pure SQLite read.
/// Backs the fully-virtual "Orphaned" Smart Folder — no `folders` table
/// row (see design spec decision 4).
/// `account_id: None` reads every active account ("All accounts").
#[tauri::command]
pub fn list_orphaned_notes(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = read_views(&state, account_id, |id| state.db.list_orphaned_notes(id))?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

/// Notes untouched for 30+ days. Pure SQLite read. Backs the fully-virtual
/// "Stale" Smart Folder.
/// `account_id: None` reads every active account ("All accounts").
#[tauri::command]
pub fn list_stale_notes(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = read_views(&state, account_id, |id| state.db.list_stale_notes(id))?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}

/// Per-note trust for the Unreviewed smart folder's list.
#[derive(serde::Serialize)]
pub struct UnreviewedTrust { pub(crate) uuid: String, pub(crate) trust: provenance::Trust }

/// Notes whose content was last written by an agent, workflow or remote
/// change and not since reviewed. Pure SQLite read. Backs the fully-virtual
/// "Unreviewed" Smart Folder. `async` (R8's reasoning): a sync command runs on
/// the main thread and can wait up to 5 s behind a jodd-mcp write.
/// `account_id: None` reads every active account ("All accounts"); the
/// chips' trust is already loaded per account (`refreshUnreviewed`).
#[tauri::command]
pub async fn list_unreviewed_notes(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = read_views(&state, account_id, |id| state.db.list_unreviewed(id))?;
    Ok(rows.iter().map(|(n, _)| n.to_frontend_note()).collect())
}

/// The same listing, as `(uuid, trust)` pairs, so the list can show each
/// row's chip without a second per-note round trip. Loads no note body.
#[tauri::command]
pub async fn list_unreviewed_trust(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<UnreviewedTrust>, String> {
    let rows = state.db.list_unreviewed_trust(&account_id).map_err(|e| e.to_string())?;
    Ok(rows.into_iter().map(|(uuid, trust)| UnreviewedTrust { uuid, trust }).collect())
}

/// Record that the user reviewed this note, unless something non-human
/// changed it after the `seen_*` state they were looking at (`stale`).
#[tauri::command]
pub async fn verify_note(
    account_id: String,
    uuid: String,
    seen_event_id: Option<i64>,
    seen_local_version: i64,
    state: State<'_, AppState>,
) -> Result<db::VerifyOutcome, String> {
    state.db.verify_note(&account_id, &uuid, seen_event_id, seen_local_version).map_err(|e| e.to_string())
}

/// Notes carrying Extract's Source block, wherever they are filed. Pure
/// SQLite read. Backs the fully-virtual "Extracts" Smart Folder.
/// `account_id: None` reads every active account ("All accounts").
#[tauri::command]
pub fn list_extract_notes(
    account_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<gmail::Note>, String> {
    let rows = read_views(&state, account_id, |id| state.db.list_extract_notes(id))?;
    Ok(rows.iter().map(|n| n.to_frontend_note()).collect())
}


// move_note (label-modify based) was removed 2026-06-09 — dead code with no
// callers anywhere in the frontend. The actual move-folder flow is implemented
// in NoteContextMenu.svelte via save_note (insert + trash), which preserves
// the X-UUID and works cross-folder without separate move logic. If a future
// "fast move that skips body re-upload" is needed, re-add with: validate
// message_id belongs to account_id (cache lookup), check both labels exist,
// and update the local cache row's label inside the same critical section.

/// Run one sync tick immediately instead of waiting out the worker's sleep.
/// Called by the frontend on visibility transitions: on Android the worker
/// loop stops entirely once the OS freezes a backgrounded process, so without
/// this a note edited just before switching apps would sit dirty. See spec §5.
///
/// Shares per-account admission with periodic work and immediate LocalFS writes.
#[tauri::command]
pub async fn flush_sync(app: AppHandle) -> Result<(), String> {
    sync_worker_tick(&app).await;
    Ok(())
}

#[cfg(test)]
mod restore_kind_tests {
    use super::*;

    /// `restore_note` used to be `if Gmail { … } else { …LocalFs… }` — true
    /// while exactly two backends had a trash, and a live defect the moment a
    /// third did: the `else` ran LocalFs's trash-filename decode over a
    /// CloudKit `recordName`.
    #[test]
    fn every_backend_states_how_it_restores() {
        use accounts::BackendKind::*;
        assert_eq!(restore_kind(Gmail), Some(RestoreKind::UntrashThenMove));
        assert_eq!(restore_kind(LocalFs), Some(RestoreKind::UntrashToEncodedPath));
        assert_eq!(restore_kind(ICloud), Some(RestoreKind::MoveOutOfTrash));
        assert_eq!(
            restore_kind(Microsoft),
            None,
            "measured: an Apple-side delete leaves nothing in Deleted Items"
        );
    }

    /// The two facts must agree. A backend that reports a trash the UI shows,
    /// and then has no way to restore from it, is the always-refusing button
    /// the capability was introduced to prevent.
    #[test]
    fn a_backend_with_a_trash_can_restore_from_it_and_one_without_cannot() {
        for kind in accounts::ALL_BACKENDS {
            assert_eq!(
                backend::Capabilities::for_backend(kind).has_trash,
                restore_kind(kind).is_some(),
                "{kind:?} disagrees with itself about having a Recently Deleted"
            );
        }
    }
}

#[cfg(test)]
mod cached_note_tests {
    use super::*;
    // The editor re-reads the open note through this when the trust chip
    // advances past what it rendered, so Mark reviewed never certifies a body
    // the user has not seen (spec 2026-10-07 §5.2).
    #[test]
    fn reads_the_current_row_follows_a_rekey_and_hides_a_pending_delete() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let actor = crate::provenance::Actor::test();
        let a = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some("same"), "A", "first", "Notes", None, None, &actor, None).unwrap();
        let b = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some(&a.uuid), "A", "first and an appended line", "Notes", None, Some(a.local_version), &actor, None).unwrap();
        let n = cached_note_db(&db, "a", &a.uuid).unwrap().unwrap();
        assert!(n.body_html.contains("appended line"), "{}", n.body_html);
        assert_eq!(n.local_version, b.local_version);
        assert!(cached_note_db(&db, "other", &a.uuid).unwrap().is_none(), "account-scoped");
        db.rekey_note_uuid(&a.uuid, "assigned", "a").unwrap();
        assert_eq!(cached_note_db(&db, "a", &a.uuid).unwrap().unwrap().uuid, "assigned", "a pre-rekey uuid follows its alias (gotcha #16)");
        db.mark_deleted("assigned", "a").unwrap();
        assert!(cached_note_db(&db, "a", "assigned").unwrap().is_none(), "a pending delete is not shown");
    }

    /// The editor's refused save (2026-11-05, note 80D6C8E1): its text and the
    /// row both moved from the body it last saved, on different lines. The
    /// merge comes back with the row's version to save against; nothing is
    /// written by asking.
    #[test]
    fn a_refused_editor_save_merges_with_the_row_when_the_lines_differ() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let actor = crate::provenance::Actor::test();
        let base = "<div>one</div><div>two</div>";
        let a = save_note_db(&db, "a", accounts::BackendKind::Gmail, Some("u"), "T", base, "Notes", None, None, &actor, None).unwrap();
        let row = "<div>one</div><div>two</div><div>from elsewhere</div>";
        let b = save_note_db(&db, "a", accounts::BackendKind::Gmail, Some(&a.uuid), "T", row, "Notes", None, Some(a.local_version), &actor, None).unwrap();
        let editor = "<div>one, edited</div><div>two</div>";

        let m = merge_onto_row_db(&db, "a", &a.uuid, "T", base, editor).unwrap().expect("merges");
        assert_eq!(m.merge.body_html, "<div>one, edited</div><div>two</div><div>from elsewhere</div>");
        assert_eq!(m.merge.inserts, vec![(2, "<div>from elsewhere</div>".to_string())]);
        assert_eq!(m.local_version, b.local_version);
        assert_eq!(db.get(&a.uuid, "a").unwrap().unwrap().body_html, row, "asking writes nothing");

        // A title the row changed is not the editor's to overwrite.
        assert!(merge_onto_row_db(&db, "a", &a.uuid, "T elsewhere", base, editor).unwrap().is_none());
        // The same line changed on both sides keeps both.
        let clash = "<div>one</div><div>two, edited</div>";
        let row2 = "<div>one</div><div>two, rewritten</div>";
        save_note_db(&db, "a", accounts::BackendKind::Gmail, Some(&a.uuid), "T", row2, "Notes", None, Some(b.local_version), &actor, None).unwrap();
        assert!(merge_onto_row_db(&db, "a", &a.uuid, "T", base, clash).unwrap().is_none());
        // No row, no merge.
        assert!(merge_onto_row_db(&db, "a", "missing", "T", base, editor).unwrap().is_none());
    }

    /// The listing must never pair a remote body with a newer local version.
    #[test]
    fn a_listing_carries_a_dirty_rows_body_and_a_clean_rows_remote_body() {
        let mut row = crate::test_support::note("a", "u").title("T").body("old<br>agent line").build();
        row.local_version = 4;
        row.sync_state = db::SyncState::Dirty;
        let fetched = |row: &db::CachedNote| { let mut n = row.to_frontend_note(); n.body_html = "old".into(); n.title = "T0".into(); n.local_version = 0; n };

        let mut n = fetched(&row);
        overlay_cached_row(&mut n, &row);
        assert_eq!((n.body_html.as_str(), n.title.as_str(), n.local_version), (row.body_html.as_str(), "T", 4));
        assert!(n.body_html.contains("agent line"));

        row.sync_state = db::SyncState::Clean;
        let mut n = fetched(&row);
        overlay_cached_row(&mut n, &row);
        assert_eq!((n.body_html.as_str(), n.title.as_str(), n.local_version), ("old", "T0", 4), "clean: the remote is the truth");
    }

    /// 2026-11-12: pinned on the Mac, ⟳ on the phone, still unpinned. A Gmail
    /// message carries no pin, so the listing's copy of a note whose message
    /// changed says `false`; the row (after this pull's pin pass) is the truth.
    #[test]
    fn a_listing_carries_the_rows_pin_not_the_wires() {
        let mut row = crate::test_support::note("a", "u").title("T").body("b").build();
        row.pinned = true;
        let mut n = row.to_frontend_note();
        n.pinned = false; // a fresh parse of a new message id
        overlay_cached_row(&mut n, &row);
        assert!(n.pinned);

        row.pinned = false;
        let mut n = row.to_frontend_note();
        n.pinned = true; // a cache hit from before the pin was cleared
        overlay_cached_row(&mut n, &row);
        assert!(!n.pinned);
    }

    /// 2026-11-18: a note moved from one folder to another showed in the new
    /// one, vanished, and came back. A listing read before the worker pushed
    /// the move still finds the message in the OLD label; the row is the truth
    /// — dirty after a move, and clean when `is_older_copy` kept it against a
    /// stale copy in the label it left (gotcha #47).
    #[test]
    fn a_listing_carries_the_rows_folder_not_the_wires() {
        let mut row = crate::test_support::note("a", "u").label("Notes/B").build();
        for state in [db::SyncState::Dirty, db::SyncState::Clean] {
            row.sync_state = state;
            let mut n = row.to_frontend_note();
            n.label = "Notes/A".into(); // the stale message, still in the label it left
            overlay_cached_row(&mut n, &row);
            assert_eq!(n.label, "Notes/B", "{state:?}: the row's folder");
        }
    }
}

#[cfg(test)]
mod delete_note_tests {
    use super::delete_note_db;
    use crate::db::SyncState;
    use crate::test_support::{note, temp_db};

    const ACCT: &str = "gmail:a@x.com";

    #[test]
    fn a_uuid_marks_the_row_deleted_pending_and_nothing_else() {
        let db = temp_db();
        note(ACCT, "u1").insert(&db);
        delete_note_db(&db, ACCT, Some("u1")).unwrap();
        let row = db.get("u1", ACCT).unwrap().expect("the row stays until the worker trashes it");
        assert_eq!(row.sync_state, SyncState::DeletedPending);
    }

    // CLAUDE.md D7: without a uuid there used to be a fallback that trashed on
    // the remote by Gmail id, with no SQLite write first. It is refused now,
    // and a refusal leaves the cache untouched.
    #[test]
    fn no_usable_uuid_is_refused_and_changes_nothing() {
        let db = temp_db();
        note(ACCT, "u1").insert(&db);
        for uuid in [None, Some(""), Some("tmp:123")] {
            let err = delete_note_db(&db, ACCT, uuid).unwrap_err();
            assert!(err.contains("uuid is required"), "{uuid:?}: {err}");
        }
        assert_ne!(db.get("u1", ACCT).unwrap().unwrap().sync_state, SyncState::DeletedPending);
    }

    #[test]
    fn the_command_never_reaches_the_remote() {
        let body = crate::test_support::extract_fn_body(include_str!("listing_commands.rs"), "async fn delete_note(");
        assert!(!body.contains("vertical_for"), "delete_note must not call a vertical:\n{body}");
        assert!(body.contains("delete_note_db("), "{body}");
    }
}

#[cfg(test)]
mod package_c_persistence_tests {
    use super::*;
    #[test]
    fn sqlite_status_survives_reopen_rekey_and_an_edit_during_push() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let a = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some("same"), "A", "body", "Notes", None, None, &crate::provenance::Actor::test(), None).unwrap();
        let b = save_note_db(&db, "b", accounts::BackendKind::Microsoft, Some(&a.uuid), "B", "other", "Notes", None, None, &crate::provenance::Actor::test(), None).unwrap();
        // Offline/transient transport failure never calls mark_pushed.
        assert_eq!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().sync_state, db::SyncState::Dirty);
        let newer = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some(&a.uuid), "A", "new edit", "Notes", None, Some(a.local_version), &crate::provenance::Actor::test(), None).unwrap();
        let saved = backend::SavedNote { id: "remote".into(), uuid: "assigned".into(), version: "v1".into(), date: "today".into(), created_date: None, body_html: a.body_html.clone(), local_version: 0 };
        push_one_dirty_db(&db, &a, &saved, "assigned", true, true).unwrap();
        let status = note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap();
        assert_eq!(status.uuid, "assigned");
        assert_eq!(status.local_version, newer.local_version);
        assert_eq!(status.sync_state, db::SyncState::Dirty);
        assert_eq!(note_persistence_db(&db, "b", &b.uuid).unwrap().unwrap().uuid, b.uuid);
        db.mark_push_blocked("assigned", "a", "permanent refusal").unwrap();
        let blocked = note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap();
        assert_eq!(blocked.push_blocked_reason.as_deref(), Some("permanent refusal"));
        assert!(!blocked.push_blocked_by_remote);
        assert_eq!(blocked.sync_state, db::SyncState::Dirty);
        db.mark_push_blocked_by_remote("assigned", "a", "permanent refusal").unwrap();
        assert!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().push_blocked_by_remote,
            "the editor's lock reaches it through this snapshot");
        drop(db);
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        assert!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().push_blocked_reason.is_some());
        let retry = save_note_db(&db, "a", accounts::BackendKind::Microsoft, Some(&a.uuid), "A", "final edit", "Notes", None, Some(newer.local_version), &crate::provenance::Actor::test(), None).unwrap();
        push_one_dirty_db(&db, &retry, &saved, "assigned", false, false).unwrap();
        assert_eq!(note_persistence_db(&db, "a", &a.uuid).unwrap().unwrap().sync_state, db::SyncState::Clean);
        assert_eq!(note_persistence_db(&db, "b", &b.uuid).unwrap().unwrap().sync_state, db::SyncState::Dirty);
        assert!(note_persistence_db(&db, "missing", &a.uuid).unwrap().is_none());
    }
    #[test]
    fn mutations_follow_rekeys_only_inside_the_requested_account() {
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        for u in ["single", "batch"] {
            crate::test_support::note("a", u).insert(&db);
            crate::test_support::note("b", u).insert(&db);
            db.rekey_note_uuid(u, &format!("new-{u}"), "a").unwrap();
        }
        db.set_pin("single", "a", true).unwrap();
        assert!(db.get("new-single", "a").unwrap().unwrap().pinned);
        assert_eq!(note_persistence_db(&db, "a", "single").unwrap().unwrap().sync_state, db::SyncState::Dirty);
        db.set_pin_batch("a", &["batch".into()], true).unwrap();
        assert!(db.get("new-batch", "a").unwrap().unwrap().pinned);
        assert!(!db.get("single", "b").unwrap().unwrap().pinned);
        db.mark_deleted("single", "a").unwrap();
        db.delete_notes_batch("a", &["batch".into()]).unwrap();
        assert_eq!(db.get("new-single", "a").unwrap().unwrap().sync_state, db::SyncState::DeletedPending);
        assert_eq!(db.get("new-batch", "a").unwrap().unwrap().sync_state, db::SyncState::DeletedPending);
        assert_eq!(db.get("batch", "b").unwrap().unwrap().sync_state, db::SyncState::Clean);
    }
    #[test]
    fn worker_receipt_matches_the_typescript_contract() {
        let value = serde_json::to_value(PushConfirmation { account_id: "a".into(), uuid: "u".into(), body_html: "body".into() }).unwrap();
        assert_eq!(value["accountId"], "a");
        assert_eq!(value["bodyHtml"], "body");
    }
}
