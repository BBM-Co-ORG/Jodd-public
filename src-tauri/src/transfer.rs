//! Copy to / Move to another account (spec
//! `docs/superpowers/specs/2026-11-13-cross-account-move-copy-design.md`).
//!
//! One engine for both. Each source note is planned on its own: refused with a
//! reason, or written with the losses it takes in the target. A copy is a
//! brand-new dirty note in the target, built by the same body rules a backup
//! restore uses (`backup::restore::transform`): attachments become
//! placeholders where the target cannot hold them, tags become a `Keywords:`
//! line where body hashtags are not tags. A move is that copy plus the
//! original hidden as `deleted_pending` with a `note_transfers` row, written
//! in the same transaction (`Db::insert_note_copy`); the worker lets the
//! original's deletion go only once the copy is safe (`Db::settle_transfers`).
//!
//! SQLite only. Nothing here touches a remote (local-first doctrine).

use crate::accounts::{Account, BackendKind};
use crate::backend::{Attachment, Capabilities, TagModel};
use crate::backup::body::content_sha256;
use crate::backup::restore::plan::CID_LEFT;
use crate::backup::restore::transform::{append_keywords, strip_attachments, Loss};
use crate::backup::AttachmentRef;
use crate::db::{now_ms, CachedNote, Db, FolderSyncState, NoteTransfer, SyncState, TRANSFER_AWAITING_TARGET};
use crate::provenance::Actor;
use serde::{Deserialize, Serialize};

pub const NOT_FOUND: &str = "This note no longer exists.";
pub const UNSAVED: &str = "Save this note before copying or moving it.";
pub const TARGET_CANNOT_WRITE: &str = "Jodd cannot write notes to this account.";
pub const SOURCE_CANNOT_REMOVE: &str = "Jodd cannot remove notes from this note's account, so it can only copy it.";
pub const IN_CONFLICT: &str = "This note has a conflict. Resolve it before moving the note.";
pub const PARTIAL: &str =
    "Jodd cannot read all of this note, so moving it would lose the part it cannot read. Copy it instead.";
pub const FOLDER_DELETING: &str = "That folder is being deleted in this account.";
pub const SAME_ACCOUNT_MOVE: &str = "This note is already in that account.";
pub const CHANGED_DURING_MOVE: &str = "This note changed while it was being moved. Try again.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TransferMode {
    Copy,
    Move,
}

/// One note, by the key every note carries (`noteKey` on the frontend).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRef {
    pub account_id: String,
    pub uuid: String,
}

/// What one source note will do (the preview) or did (the report).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTransfer {
    pub source: NoteRef,
    pub title: String,
    /// Why it is not copied or moved; `None` means it will be (or was).
    pub refused: Option<String>,
    pub losses: Vec<Loss>,
    /// The copy's uuid, in a report only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_uuid: Option<String>,
}

/// A transfer's preview, or what it did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferReport {
    pub notes: Vec<PlannedTransfer>,
}

impl TransferReport {
    pub fn written(&self) -> usize {
        self.notes.iter().filter(|n| n.refused.is_none()).count()
    }
}

/// A planned note that will be written, with everything the write needs.
struct Prepared {
    source: CachedNote,
    body: String,
    attachments: Vec<Attachment>,
    pinned: bool,
}

fn holds_attachments(kind: BackendKind) -> bool {
    matches!(kind, BackendKind::Gmail | BackendKind::LocalFs)
}

fn folder_being_deleted(db: &Db, account_id: &str, folder: &str) -> Result<bool, String> {
    let mut path = String::new();
    for segment in folder.split('/') {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(segment);
        if db.folder_sync_state(account_id, &path).map_err(|e| e.to_string())? == Some(FolderSyncState::DeletedPending) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Plan one note. `Err` is a database failure; a refusal is `Ok` with
/// `refused` set.
fn plan_one(
    db: &Db,
    accounts: &[Account],
    target: &Account,
    folder: &str,
    mode: TransferMode,
    r: &NoteRef,
) -> Result<(PlannedTransfer, Option<Prepared>), String> {
    let refuse = |title: &str, reason: &str| {
        Ok((
            PlannedTransfer {
                source: r.clone(),
                title: title.to_string(),
                refused: Some(reason.to_string()),
                losses: Vec::new(),
                target_uuid: None,
            },
            None,
        ))
    };
    let Some(src_account) = accounts.iter().find(|a| a.id == r.account_id) else {
        return refuse("", NOT_FOUND);
    };
    let uuid = db.resolve_note_uuid(&r.uuid, &r.account_id).map_err(|e| e.to_string())?;
    let Some(src) = db
        .get(&uuid, &r.account_id)
        .map_err(|e| e.to_string())?
        .filter(|n| n.sync_state != SyncState::DeletedPending)
    else {
        return refuse("", NOT_FOUND);
    };
    let title = src.title.clone();
    if src.uuid.is_empty() || src.uuid.starts_with("tmp:") {
        return refuse(&title, UNSAVED);
    }
    let target_caps = Capabilities::for_backend(target.backend_kind);
    if !target_caps.writes.notes {
        return refuse(&title, TARGET_CANNOT_WRITE);
    }
    let render_loss = db.render_loss(&r.account_id, &src.uuid).map_err(|e| e.to_string())?;
    if mode == TransferMode::Move {
        if src_account.id == target.id {
            return refuse(&title, SAME_ACCOUNT_MOVE);
        }
        if !Capabilities::for_backend(src_account.backend_kind).writes.relocate {
            return refuse(&title, SOURCE_CANNOT_REMOVE);
        }
        if src.sync_state == SyncState::Conflict {
            return refuse(&title, IN_CONFLICT);
        }
        if render_loss.is_some() {
            return refuse(&title, PARTIAL);
        }
    }
    if folder_being_deleted(db, &target.id, folder)? {
        return refuse(&title, FOLDER_DELETING);
    }

    let mut losses = Vec::new();
    let attachments = db.list_attachments(&r.account_id, &src.uuid).map_err(|e| e.to_string())?;
    let mut body = src.body_html.clone();
    let carried = if holds_attachments(target.backend_kind) {
        attachments
    } else {
        if !attachments.is_empty() {
            let refs: Vec<AttachmentRef> = attachments
                .iter()
                .map(|a| AttachmentRef {
                    content_id: a.content_id.clone(),
                    filename: a.filename.clone(),
                    mime: a.mime_type.clone(),
                    path: String::new(),
                    x_apple_part_url: a.x_apple_part_url.clone(),
                })
                .collect();
            body = strip_attachments(&body, &refs);
            losses.extend(attachments.iter().map(|a| Loss::Attachment {
                filename: a
                    .filename
                    .clone()
                    .filter(|f| !f.is_empty())
                    .unwrap_or_else(|| a.content_id.trim_matches(|c| c == '<' || c == '>').to_string()),
            }));
        }
        Vec::new()
    };
    // A body hashtag is not a tag on iCloud: write the tags as a line, as a
    // restore into iCloud does. Between two accounts with the same tag model
    // the body already carries them as that model reads them.
    if TagModel::for_backend(target.backend_kind) == TagModel::InlineObjects
        && TagModel::for_backend(src_account.backend_kind) != TagModel::InlineObjects
    {
        let tags = db.tags_of_note(&r.account_id, &src.uuid).map_err(|e| e.to_string())?;
        let with_keywords = append_keywords(&body, &tags);
        if with_keywords != body {
            losses.push(Loss::TagsAsKeywords);
            body = with_keywords;
        }
    }
    // R9: a `cid:` left behind would be refused at push, for good.
    if !holds_attachments(target.backend_kind) && !crate::mime822::referenced_cids(&body).is_empty() {
        return refuse(&title, CID_LEFT);
    }
    if src.pinned && !target_caps.writes.sidecars {
        losses.push(Loss::Pin);
    }
    if src_account.id != target.id {
        let out = db.outgoing_links(&r.account_id, &src.uuid).map_err(|e| e.to_string())?;
        let back = db.backlinks(&r.account_id, &src.uuid).map_err(|e| e.to_string())?;
        if !out.is_empty() || !back.is_empty() {
            losses.push(Loss::Links);
        }
    }
    if let Some(reason) = render_loss {
        losses.push(Loss::Partial { reason });
    }
    let pinned = src.pinned && target_caps.writes.sidecars;
    Ok((
        PlannedTransfer { source: r.clone(), title, refused: None, losses, target_uuid: None },
        Some(Prepared { source: src, body, attachments: carried, pinned }),
    ))
}

/// The empty folder means the target's own root.
fn folder_in(target: &Account, folder: &str) -> String {
    let f = folder.trim().trim_matches('/');
    if f.is_empty() { target.effective_notes_label().to_string() } else { f.to_string() }
}

/// What a transfer would do, writing nothing: the confirm sheet's source.
pub fn preview(
    db: &Db,
    accounts: &[Account],
    target: &Account,
    folder: &str,
    mode: TransferMode,
    sources: &[NoteRef],
) -> Result<TransferReport, String> {
    let folder = folder_in(target, folder);
    let mut report = TransferReport::default();
    for r in sources {
        report.notes.push(plan_one(db, accounts, target, &folder, mode, r)?.0);
    }
    Ok(report)
}

/// Copy or move every source note into `target`/`folder`. Each note is
/// planned again here, not trusted from a preview: nothing may change
/// between what the user confirmed and what is written without being seen.
/// A refused note writes nothing; the others go ahead.
pub fn transfer(
    db: &Db,
    accounts: &[Account],
    target: &Account,
    folder: &str,
    mode: TransferMode,
    sources: &[NoteRef],
) -> Result<TransferReport, String> {
    let folder = folder_in(target, folder);
    let mut report = TransferReport::default();
    let mut seen = std::collections::HashSet::new();
    for r in sources {
        if !seen.insert((r.account_id.clone(), r.uuid.clone())) {
            continue;
        }
        let (mut planned, prepared) = plan_one(db, accounts, target, &folder, mode, r)?;
        if let Some(p) = prepared {
            let uuid = crate::backend::mint_uuid_for(target.backend_kind);
            let now = now_ms();
            let note = CachedNote {
                uuid: uuid.clone(),
                account_id: target.id.clone(),
                id: String::new(),
                title: p.source.title.clone(),
                body_html: p.body,
                date: chrono::Local::now().to_rfc2822(),
                x_mail_created_date: p.source.x_mail_created_date.clone(),
                label: folder.clone(),
                local_version: 1,
                remote_version: None,
                sync_state: SyncState::Dirty,
                last_synced_at: None,
                last_local_modified_at: now,
                last_remote_modified_at: None,
                pinned: p.pinned,
                meta_msg_id: None,
                pin_dirty: p.pinned,
                push_blocked_reason: None,
                push_blocked_by_remote: false,
            };
            let move_row = (mode == TransferMode::Move).then(|| NoteTransfer {
                source_account: p.source.account_id.clone(),
                source_uuid: p.source.uuid.clone(),
                target_account: target.id.clone(),
                target_uuid: uuid.clone(),
                source_sha: content_sha256(&p.source.body_html),
                source_prev_state: p.source.sync_state.as_str().to_string(),
                state: TRANSFER_AWAITING_TARGET.to_string(),
                created_at: now,
            });
            match db.insert_note_copy(
                &note,
                &p.attachments,
                &Actor::Human,
                &p.source.account_id,
                &p.source.uuid,
                move_row.as_ref(),
            ) {
                Ok(()) => planned.target_uuid = Some(uuid),
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    planned.refused = Some(CHANGED_DURING_MOVE.to_string());
                    planned.losses.clear();
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        report.notes.push(planned);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountStatus, LlmConfig};
    use crate::db::TRANSFER_TARGET_SAFE;
    use crate::test_support::temp_db;

    fn account(kind: BackendKind, email: &str) -> Account {
        Account {
            id: crate::accounts::account_id_for(kind, email),
            email: email.to_string(),
            added_at: "2026-01-01T00:00:00Z".to_string(),
            notes_label: None,
            meta_label: None,
            llm: LlmConfig::default(),
            backend_kind: kind,
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

    fn live_notes(db: &Db, account_id: &str) -> usize {
        db.backup_read(account_id).unwrap().notes.len()
    }

    fn note(account: &Account, uuid: &str, title: &str, body: &str, label: &str, state: SyncState) -> CachedNote {
        CachedNote {
            uuid: uuid.into(),
            account_id: account.id.clone(),
            id: if state == SyncState::Clean { format!("m-{uuid}") } else { String::new() },
            title: title.into(),
            body_html: body.into(),
            date: "Fri, 13 Nov 2026 10:00:00 +0700".into(),
            x_mail_created_date: Some("Mon, 2 Nov 2026 09:00:00 +0700".into()),
            label: label.into(),
            local_version: 1,
            remote_version: (state == SyncState::Clean).then(|| "v1".to_string()),
            sync_state: state,
            last_synced_at: None,
            last_local_modified_at: 1,
            last_remote_modified_at: None,
            pinned: false,
            meta_msg_id: None,
            pin_dirty: false,
            push_blocked_reason: None,
            push_blocked_by_remote: false,
        }
    }

    fn put(db: &Db, n: &CachedNote) {
        db.insert_local_new(n, &Actor::Human).unwrap();
        if n.sync_state != SyncState::Dirty {
            db.set_sync_state_for_test(&n.uuid, &n.account_id, n.sync_state, &n.id).unwrap();
        }
    }

    fn r(n: &CachedNote) -> NoteRef {
        NoteRef { account_id: n.account_id.clone(), uuid: n.uuid.clone() }
    }

    fn png(cid: &str) -> Attachment {
        Attachment {
            content_id: cid.into(),
            mime_type: "image/png".into(),
            filename: Some(format!("{cid}.png")),
            x_apple_part_url: None,
            data: vec![1, 2, 3],
        }
    }

    const PIC: &str = r#"<div>Trip</div><div><object data="cid:pic1" type="image/png"></object></div>"#;

    #[test]
    fn copy_writes_a_dirty_note_in_the_target_and_leaves_the_source() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let mut src = note(&a, "S1", "Trip", PIC, "Notes/Travel", SyncState::Clean);
        src.pinned = true;
        put(&db, &src);
        db.set_pin(&src.uuid, &a.id, true).unwrap();
        db.upsert_attachment(&a.id, "S1", &png("pic1")).unwrap();

        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes/Inbox", TransferMode::Copy, &[r(&src)]).unwrap();
        assert_eq!(rep.written(), 1, "{rep:?}");
        let got = &rep.notes[0];
        assert!(got.losses.is_empty(), "Gmail → Gmail loses nothing: {:?}", got.losses);
        let uuid = got.target_uuid.clone().unwrap();
        let copy = db.get(&uuid, &b.id).unwrap().unwrap();
        assert_eq!(copy.title, "Trip", "the title is kept (owner, §10 q4)");
        assert_eq!(copy.label, "Notes/Inbox");
        assert_eq!(copy.sync_state, SyncState::Dirty);
        assert!(copy.id.is_empty());
        assert_eq!(copy.body_html, PIC);
        assert!(copy.pinned && copy.pin_dirty);
        assert_eq!(copy.x_mail_created_date, src.x_mail_created_date);
        assert_eq!(db.attachment_bytes(&b.id, &uuid, "pic1").unwrap(), Some(vec![1, 2, 3]));
        let left = db.get("S1", &a.id).unwrap().unwrap();
        assert_eq!(left.sync_state, SyncState::Clean, "a copy never touches its source");
        assert!(db.open_transfer(&a.id, "S1").unwrap().is_none());
    }

    #[test]
    fn a_copy_carries_the_sources_history_into_the_other_account() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let src = note(&a, "S1", "Plan", "<div>Plan</div>", "Notes", SyncState::Dirty);
        db.insert_local_new(&src, &Actor::Workflow { name: "extract".into(), model: "m".into() }).unwrap();

        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "", TransferMode::Copy, &[r(&src)]).unwrap();
        let uuid = rep.notes[0].target_uuid.clone().unwrap();
        let events = db.events_for_test(&b.id, &uuid);
        let kinds: Vec<(&str, Option<&str>)> = events.iter().map(|e| (e.kind.as_str(), e.actor.as_deref())).collect();
        assert_eq!(kinds[0].0, "create");
        assert_eq!(kinds[0].1, Some("jodd-extract/m"), "the workflow that wrote it, carried over: {kinds:?}");
        assert_eq!(kinds.last().unwrap().1, Some("human:owner"), "{kinds:?}");
        assert_eq!(db.get(&uuid, &b.id).unwrap().unwrap().label, "Notes", "empty folder = the target's root");
    }

    #[test]
    fn into_outlook_attachments_become_placeholders_and_into_icloud_tags_become_a_line() {
        let db = temp_db();
        let a = account(BackendKind::Gmail, "a@x");
        let ms = account(BackendKind::Microsoft, "m@live.com");
        let ic = account(BackendKind::ICloud, "i@icloud.com");
        let src = note(&a, "S1", "Trip", PIC, "Notes", SyncState::Clean);
        put(&db, &src);
        db.upsert_attachment(&a.id, "S1", &png("pic1")).unwrap();
        let tagged = note(&a, "S2", "Ideas", "<div>Ideas</div><div>#music</div>", "Notes", SyncState::Clean);
        put(&db, &tagged);
        let all = [a.clone(), ms.clone(), ic.clone()];

        let rep = transfer(&db, &all, &ms, "Notes", TransferMode::Copy, &[r(&src)]).unwrap();
        let n = &rep.notes[0];
        assert_eq!(n.losses, vec![Loss::Attachment { filename: "pic1.png".into() }]);
        let copy = db.get(n.target_uuid.as_deref().unwrap(), &ms.id).unwrap().unwrap();
        assert!(copy.body_html.contains("[Attachment not restored: pic1.png]"), "{}", copy.body_html);
        assert!(db.list_attachments(&ms.id, &copy.uuid).unwrap().is_empty());

        let rep = transfer(&db, &all, &ic, "Notes", TransferMode::Copy, &[r(&tagged)]).unwrap();
        let n = &rep.notes[0];
        assert_eq!(n.losses, vec![Loss::TagsAsKeywords]);
        let copy = db.get(n.target_uuid.as_deref().unwrap(), &ic.id).unwrap().unwrap();
        assert!(copy.body_html.contains("Keywords:"), "{}", copy.body_html);
    }

    #[test]
    fn links_are_a_loss_only_across_accounts_and_only_when_there_are_some() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let linked = note(&a, "S1", "Hub", "<div>Hub</div><div>see [[Spoke]]</div>", "Notes", SyncState::Clean);
        let spoke = note(&a, "S2", "Spoke", "<div>Spoke</div>", "Notes", SyncState::Clean);
        let alone = note(&a, "S3", "Alone", "<div>Alone</div>", "Notes", SyncState::Clean);
        for n in [&linked, &spoke, &alone] {
            put(&db, n);
        }
        let all = [a.clone(), b.clone()];
        let p = preview(&db, &all, &b, "", TransferMode::Copy, &[r(&linked), r(&spoke), r(&alone)]).unwrap();
        let losses: Vec<&Vec<Loss>> = p.notes.iter().map(|n| &n.losses).collect();
        assert_eq!(losses, vec![&vec![Loss::Links], &vec![Loss::Links], &vec![]], "outgoing, incoming, none");
        let same = preview(&db, &all, &a, "", TransferMode::Copy, &[r(&linked)]).unwrap();
        assert!(same.notes[0].losses.is_empty(), "a copy within the account keeps its links");
        assert!(p.notes.iter().all(|n| n.target_uuid.is_none()));
        assert_eq!(live_notes(&db, &b.id), 0, "a preview writes nothing");
    }

    #[test]
    fn refusals_write_nothing() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Microsoft, "b@live.com"));
        let all = [a.clone(), b.clone()];
        let conflict = note(&a, "C", "Fork", "<div>Fork</div>", "Notes", SyncState::Clean);
        put(&db, &conflict);
        db.set_sync_state_for_test("C", &a.id, SyncState::Conflict, "m-C").unwrap();
        let partial = note(&a, "P", "Sketch", "<div>Sketch</div>", "Notes", SyncState::Clean);
        put(&db, &partial);
        db.set_render_loss(&a.id, "P", Some("inline drawing")).unwrap();
        let stray = note(&a, "X", "Stray", r#"<div>Stray</div><img src="cid:gone">"#, "Notes", SyncState::Clean);
        put(&db, &stray);
        let missing = NoteRef { account_id: a.id.clone(), uuid: "NOPE".into() };

        let rep = transfer(&db, &all, &b, "Notes", TransferMode::Move, &[r(&conflict), r(&partial), missing.clone()]).unwrap();
        let reasons: Vec<Option<&str>> = rep.notes.iter().map(|n| n.refused.as_deref()).collect();
        assert_eq!(reasons, vec![Some(IN_CONFLICT), Some(PARTIAL), Some(NOT_FOUND)]);

        // A copy of a partial note goes, and says what it could not read.
        let rep = transfer(&db, &all, &b, "Notes", TransferMode::Copy, &[r(&partial), r(&stray)]).unwrap();
        assert_eq!(rep.notes[0].losses, vec![Loss::Partial { reason: "inline drawing".into() }]);
        assert_eq!(rep.notes[1].refused.as_deref(), Some(CID_LEFT), "R9: a dead cid would be refused for good");
        assert_eq!(live_notes(&db, &b.id), 1);

        // Moving within one account is `move_notes_batch`'s job.
        let rep = transfer(&db, &all, &a, "Notes", TransferMode::Move, &[r(&stray)]).unwrap();
        assert_eq!(rep.notes[0].refused.as_deref(), Some(SAME_ACCOUNT_MOVE));
    }

    #[test]
    fn a_move_hides_the_original_and_holds_its_deletion_until_the_copy_is_safe() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let src = note(&a, "S1", "Trip", "<div>Trip</div>", "Notes", SyncState::Clean);
        put(&db, &src);

        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&src)]).unwrap();
        let uuid = rep.notes[0].target_uuid.clone().unwrap();
        assert_eq!(db.get("S1", &a.id).unwrap().unwrap().sync_state, SyncState::DeletedPending);
        let t = db.open_transfer(&a.id, "S1").unwrap().unwrap();
        assert_eq!((t.state.as_str(), t.source_prev_state.as_str()), (TRANSFER_AWAITING_TARGET, "clean"));
        assert!(db.list_deleted_pending().unwrap().is_empty(), "the deletion waits for the copy");
        assert!(db.has_pending_pushes(&a.id).unwrap(), "a move in flight is not an empty queue (gotcha #2)");
        assert!(rep.notes[0].losses.is_empty());

        // Not yet pushed: nothing changes.
        assert_eq!(db.settle_transfers().unwrap(), Default::default());
        // The copy reached its server.
        db.set_sync_state_for_test(&uuid, &b.id, SyncState::Clean, "m-new").unwrap();
        let s = db.settle_transfers().unwrap();
        assert_eq!(s.safe.len(), 1);
        assert_eq!(db.open_transfer(&a.id, "S1").unwrap().unwrap().state, TRANSFER_TARGET_SAFE);
        let pending: Vec<String> = db.list_deleted_pending().unwrap().into_iter().map(|n| n.uuid).collect();
        assert_eq!(pending, vec!["S1".to_string()]);
        // The deletion pushed: the row goes, and its move with it.
        db.delete("S1", &a.id).unwrap();
        assert!(db.open_transfer(&a.id, "S1").unwrap().is_none());
    }

    #[test]
    fn a_refused_copy_calls_the_move_off_and_keeps_both() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Microsoft, "b@live.com"));
        let src = note(&a, "S1", "Trip", "<div>Trip</div>", "Notes", SyncState::Dirty);
        put(&db, &src);
        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&src)]).unwrap();
        let uuid = rep.notes[0].target_uuid.clone().unwrap();
        db.mark_push_blocked(&uuid, &b.id, "Exchange said no").unwrap();

        let s = db.settle_transfers().unwrap();
        assert_eq!(s.called_off.len(), 1);
        assert_eq!(s.called_off[0].1, crate::db::TransferCallOff::TargetRefused);
        let back = db.get("S1", &a.id).unwrap().unwrap();
        assert_eq!(back.sync_state, SyncState::Dirty, "the original returns to the state it had");
        assert!(db.get(&uuid, &b.id).unwrap().is_some(), "the refused copy stays to be seen (gotcha #14)");
        assert!(db.open_transfer(&a.id, "S1").unwrap().is_none());
    }

    #[test]
    fn removing_an_account_mid_move_calls_off_or_drops_the_move() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let one = note(&a, "S1", "One", "<div>One</div>", "Notes", SyncState::Clean);
        put(&db, &one);
        transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&one)]).unwrap();
        // The target goes: the original comes back.
        db.delete_account(&b.id).unwrap();
        assert_eq!(db.get("S1", &a.id).unwrap().unwrap().sync_state, SyncState::Clean);
        assert!(db.list_transfers().unwrap().is_empty());

        // The source goes: its move goes with it; the copy is a normal note.
        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&one)]).unwrap();
        let uuid = rep.notes[0].target_uuid.clone().unwrap();
        db.delete_account(&a.id).unwrap();
        assert!(db.list_transfers().unwrap().is_empty());
        assert!(db.get(&uuid, &b.id).unwrap().is_some());
    }

    #[test]
    fn a_user_deleting_the_moved_copy_lets_the_original_go_too() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let src = note(&a, "S1", "Trip", "<div>Trip</div>", "Notes", SyncState::Clean);
        put(&db, &src);
        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&src)]).unwrap();
        db.mark_deleted(rep.notes[0].target_uuid.as_deref().unwrap(), &b.id).unwrap();
        assert_eq!(db.settle_transfers().unwrap().safe.len(), 1);
        assert_eq!(db.list_deleted_pending().unwrap().len(), 2);
    }

    #[test]
    fn a_rekeyed_copy_still_settles_its_move() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Microsoft, "b@live.com"));
        let src = note(&a, "S1", "Trip", "<div>Trip</div>", "Notes", SyncState::Clean);
        put(&db, &src);
        let rep = transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&src)]).unwrap();
        let placeholder = rep.notes[0].target_uuid.clone().unwrap();
        // Exchange assigns the identity at create (gotcha #16).
        db.rekey_note_uuid(&placeholder, "<AM0PR@x.outlook.com>", &b.id).unwrap();
        db.set_sync_state_for_test("<AM0PR@x.outlook.com>", &b.id, SyncState::Clean, "ms-1").unwrap();
        let s = db.settle_transfers().unwrap();
        assert_eq!(s.safe.len(), 1);
        assert_eq!(s.safe[0].target_uuid, "<AM0PR@x.outlook.com>");
    }

    #[test]
    fn a_base_change_on_the_original_calls_the_move_off() {
        let db = temp_db();
        let (a, b) = (account(BackendKind::Gmail, "a@x"), account(BackendKind::Gmail, "b@x"));
        let src = note(&a, "S1", "Trip", "<div>Trip</div>", "Notes", SyncState::Clean);
        put(&db, &src);
        transfer(&db, &[a.clone(), b.clone()], &b, "Notes", TransferMode::Move, &[r(&src)]).unwrap();
        let t = db.call_off_transfer(&a.id, "S1").unwrap().unwrap();
        assert_eq!(t.source_uuid, "S1");
        assert_eq!(db.get("S1", &a.id).unwrap().unwrap().sync_state, SyncState::Clean);
        assert!(db.call_off_transfer(&a.id, "S1").unwrap().is_none());
    }
}
