//! Applying a restore plan: SQLite writes only. The sync worker pushes the
//! rows afterwards like any other local edit (local-first doctrine).
//!
//! One note at a time, each in its own transaction(s) and with no batch
//! transaction across notes: a crash mid-restore leaves a prefix of fully
//! written notes, which a re-run skips through `restore_map`.

use super::plan::{
    checksum_ok, decoded_html, finish_body, live_note, source_id, source_sha, Action, RestorePlan, TargetTraits,
    CHECKSUM_MISMATCH, NOT_IN_BACKUP,
};
use super::transform::{rewrite_id_suffixes, Loss};
use crate::accounts::Account;
use crate::backend::gmail::wire::Attachment;
use crate::backup::bundle::OpenBundle;
use crate::backup::{HistoryEvent, NoteDoc};
use crate::db::{now_ms, CachedNote, Db, RestoreMapRow, SyncState};
use crate::provenance::{Actor, Event};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    /// New notes, Keep-both copies included.
    pub created: usize,
    pub replaced: usize,
    pub skipped: usize,
    /// `(source uuid, reason)` for every note not written.
    pub refused: Vec<(String, String)>,
    /// `(source uuid, loss)` for every note written.
    pub losses: Vec<(String, Loss)>,
}

/// The history a restored note starts with, and the origin that stands in for
/// it when the bundle carries none (a foreign note: one `create` by the
/// restore process, so it is never mistaken for human-written).
///
/// Copied rows lose their versions, which is where an unrecorded change
/// (provenance rule 2b) lives. The writer leaves `generated` out exactly when
/// it hid the writer for such a gap; if the copied history alone would name
/// one, an `unrecorded` marker keeps the note Unreviewed (R11's shape).
fn restored_history(doc: &NoteDoc) -> (Vec<HistoryEvent>, Option<Actor>) {
    let Some(j) = &doc.jodd else {
        return (Vec::new(), Some(Actor::Process("restore")));
    };
    let mut history = j.history.clone();
    let copied: Vec<Event> = history
        .iter()
        .enumerate()
        .map(|(i, e)| Event { id: i as i64, kind: e.kind.clone(), actor: e.by.clone(), at: 0, local_version: None })
        .collect();
    if doc.generated.is_none() && crate::provenance::derive(&copied, 1).by.is_some() {
        history.push(HistoryEvent {
            kind: crate::provenance::UNRECORDED_KIND.to_string(),
            by: None,
            at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        });
    }
    (history, None)
}

/// Read a note's attachments out of the bundle, or why they cannot be.
fn attachments_of(doc: &NoteDoc, bundle: &mut OpenBundle) -> Result<Vec<Attachment>, String> {
    let refs = doc.jodd.as_ref().map(|j| j.attachments.as_slice()).unwrap_or(&[]);
    refs.iter()
        .map(|r| {
            let data = bundle
                .attachment(&r.path)
                .map_err(|e| format!("An attachment could not be read from the backup: {e}"))?;
            Ok(Attachment {
                content_id: r.content_id.clone(),
                mime_type: r.mime.clone(),
                filename: r.filename.clone(),
                x_apple_part_url: r.x_apple_part_url.clone(),
                data,
            })
        })
        .collect()
}

pub fn apply_restore(
    db: &Db,
    target: &Account,
    plan: &RestorePlan,
    bundle: &mut OpenBundle,
    backup_id: &str,
) -> Result<RestoreReport, String> {
    let t = TargetTraits::of(target);
    let source_account = bundle.info().account_id.clone();
    let index: HashMap<String, usize> = bundle.notes().iter().enumerate().map(|(i, n)| (source_id(n), i)).collect();

    if t.caps.writes.folders {
        for folder in &plan.folders_to_create {
            db.create_folder_local_new(&target.id, folder).map_err(|e| e.to_string())?;
        }
    }

    // Every written note's identity in the target, decided before any body is
    // built, so links between restored notes can follow them.
    let mut new_uuid: HashMap<String, String> = HashMap::new();
    for p in &plan.notes {
        let uuid = match &p.action {
            Action::Create | Action::KeepBoth { .. } => crate::backend::mint_uuid_for(t.kind),
            Action::Replace { existing_uuid } => db.resolve_note_uuid(existing_uuid, &target.id).map_err(|e| e.to_string())?,
            _ => continue,
        };
        new_uuid.insert(p.source_uuid.clone(), uuid);
    }

    let mut report = RestoreReport::default();
    for p in &plan.notes {
        let id = &p.source_uuid;
        let mut refuse = |reason: &str| report.refused.push((id.clone(), reason.to_string()));
        match &p.action {
            Action::Refuse { reason } => {
                refuse(reason);
                continue;
            }
            a if !a.writes() => {
                report.skipped += 1;
                continue;
            }
            _ => {}
        }
        let Some(&i) = index.get(id) else {
            refuse(NOT_IN_BACKUP);
            continue;
        };
        let doc = bundle.notes()[i].doc.clone();
        let html = decoded_html(&doc);
        if !checksum_ok(&doc, &html) {
            refuse(CHECKSUM_MISMATCH);
            continue;
        }
        let body = finish_body(&rewrite_id_suffixes(&html, &new_uuid), &doc, &t);
        let uuid = new_uuid[id].clone();
        let pinned = doc.jodd.as_ref().is_some_and(|j| j.pinned);
        let created = doc.jodd.as_ref().and_then(|j| j.created.clone());
        let sha = source_sha(&doc, &html);

        // Apply is idempotent on its own: the same plan applied twice (a
        // double-clicked Confirm, "Try again" after an error part-way) must
        // not create a note a previous apply already wrote (R10(a)'s test).
        let replacing = match &p.action {
            Action::Replace { .. } => match live_note(db, &uuid, &target.id)? {
                Some(current) => Some(current),
                None => {
                    refuse("The note to replace no longer exists.");
                    continue;
                }
            },
            _ => {
                if let Some(row) = db.restore_lookup(&source_account, id, &target.id).map_err(|e| e.to_string())? {
                    if row.content_sha256 == sha && live_note(db, &row.target_uuid, &target.id)?.is_some() {
                        report.skipped += 1;
                        continue;
                    }
                }
                None
            }
        };

        // Attachments first: a crash after them leaves only orphan rows, never
        // a restored note (which a re-run skips) missing its attachments.
        if t.holds_attachments {
            let attachments = match attachments_of(&doc, bundle) {
                Ok(a) => a,
                Err(reason) => {
                    refuse(&reason);
                    continue;
                }
            };
            for a in &attachments {
                db.upsert_attachment(&target.id, &uuid, a).map_err(|e| e.to_string())?;
            }
        }

        let mut map = RestoreMapRow {
            source_account: source_account.clone(),
            source_uuid: id.clone(),
            target_account: target.id.clone(),
            target_uuid: uuid.clone(),
            content_sha256: sha,
            backup_id: backup_id.to_string(),
            restored_at: now_ms(),
        };
        match &p.action {
            Action::Replace { .. } => {
                let current = replacing.expect("a Replace resolved its note above");
                let row = crate::note_mutations::save_note_db(
                    db,
                    &target.id,
                    t.kind,
                    Some(&current.uuid),
                    &p.title,
                    &body,
                    &current.label,
                    created.as_deref(),
                    None,
                    &Actor::Process("restore"),
                    None,
                )?;
                map.target_uuid = row.uuid.clone();
                db.record_restore(&map).map_err(|e| e.to_string())?;
                if t.caps.writes.sidecars && row.pinned != pinned {
                    db.set_pin(&row.uuid, &target.id, pinned).map_err(|e| e.to_string())?;
                }
                report.replaced += 1;
            }
            _ => {
                // The pin rides in the insert itself, so it cannot be lost
                // between two writes the way a separate `set_pin` could.
                let pinned = pinned && t.caps.writes.sidecars;
                let now = now_ms();
                let note = CachedNote {
                    uuid,
                    account_id: target.id.clone(),
                    id: String::new(),
                    title: p.title.clone(),
                    body_html: body,
                    date: chrono::Local::now().to_rfc2822(),
                    x_mail_created_date: created,
                    label: p.target_folder.clone(),
                    local_version: 1,
                    remote_version: None,
                    sync_state: SyncState::Dirty,
                    last_synced_at: None,
                    last_local_modified_at: now,
                    last_remote_modified_at: None,
                    pinned,
                    meta_msg_id: None,
                    pin_dirty: pinned,
                    push_blocked_reason: None,
                    push_blocked_by_remote: false,
                };
                let (history, origin) = restored_history(&doc);
                db.insert_restored(&note, &history, origin.as_ref(), &map).map_err(|e| e.to_string())?;
                report.created += 1;
            }
        }
        report.losses.extend(p.losses.iter().map(|l| (id.clone(), l.clone())));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{Account, AccountStatus, BackendKind, LlmConfig};
    use crate::backup::body::content_sha256;
    use crate::backup::bundle::{write_bundle, BundleSource, OpenBundle};
    use crate::backup::restore::plan::{plan_restore, Action, ExistingChoice, RestorePlan, RestoreRequest};
    use crate::backup::restore::transform::{Loss, Placement};
    use crate::backup::Trigger;
    use crate::db::{CachedNote, Db, FolderSyncState, RemotePin, SyncState};
    use crate::provenance::{Actor, Tier};
    use std::io::{Read, Write};

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

    fn gmail(email: &str) -> Account {
        account(BackendKind::Gmail, email)
    }

    /// A bundle on disk; the temp file lives as long as the bundle is used.
    struct Backup {
        _file: tempfile::NamedTempFile,
        bundle: OpenBundle,
    }

    fn write_backup_file(db: &Db, src: &Account) -> tempfile::NamedTempFile {
        let read = db.backup_read(&src.id).unwrap();
        let root = src.effective_notes_label().to_string();
        let source = BundleSource {
            account_id: &src.id,
            backend: crate::accounts::backend_prefix(src.backend_kind),
            root_label: &root,
            trigger: Trigger::Manual,
            created_ms: 1_792_747_800_000,
            app_version: "0.0.0-test",
            device: "test",
            read: &read,
            only: None,
        };
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut fetch = |uuid: &str, cid: &str| db.attachment_bytes(&src.id, uuid, cid).map_err(|e| e.to_string());
        write_bundle(&source, tmp.reopen().unwrap(), &mut fetch).unwrap();
        tmp
    }

    fn backup(db: &Db, src: &Account) -> Backup {
        let file = write_backup_file(db, src);
        let bundle = OpenBundle::open(file.path()).unwrap();
        Backup { _file: file, bundle }
    }

    fn all_uuids(b: &OpenBundle) -> Vec<String> {
        b.notes().iter().filter_map(|n| n.doc.jodd.as_ref().map(|j| j.uuid.clone())).collect()
    }

    fn request(b: &OpenBundle, target: &Account, existing: ExistingChoice) -> RestoreRequest {
        RestoreRequest {
            destination_id: "dest".into(),
            slug: "slug".into(),
            stamp: "2026-10-23T09-30-00Z".into(),
            source_uuids: all_uuids(b),
            target_account: target.id.clone(),
            placement: Placement::Original,
            existing,
        }
    }

    fn restore_with(db: &Db, target: &Account, b: &mut Backup, existing: ExistingChoice) -> (RestorePlan, RestoreReport) {
        let req = request(&b.bundle, target, existing);
        let plan = plan_restore(db, target, &b.bundle, &req).unwrap();
        let report = apply_restore(db, target, &plan, &mut b.bundle, "backup-1").unwrap();
        (plan, report)
    }

    fn restore(db: &Db, target: &Account, b: &mut Backup) -> (RestorePlan, RestoreReport) {
        restore_with(db, target, b, ExistingChoice::KeepBoth)
    }

    fn synced(db: &Db, acct: &Account) {
        db.record_full_listing(&acct.id).unwrap();
    }

    /// A clean note pulled from the remote, as an existing account holds it.
    fn seed(db: &Db, acct: &Account, uuid: &str, title: &str, label: &str, body: &str) -> CachedNote {
        let mut n = crate::test_support::note(&acct.id, uuid).title(title).label(label).body(body).build();
        n.x_mail_created_date = Some("Thu, 4 Jun 2026 01:19:50 +0700".into());
        db.upsert_from_remote(&n, RemotePin::RemoteWins).unwrap();
        if label != acct.effective_notes_label() {
            db.create_folder_local_new(&acct.id, label).unwrap();
        }
        n
    }

    fn by_title(db: &Db, acct: &Account, title: &str) -> CachedNote {
        db.list_notes(&acct.id)
            .unwrap()
            .into_iter()
            .find(|n| n.title == title)
            .unwrap_or_else(|| panic!("no note titled {title:?}"))
    }

    fn today() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    fn actions(plan: &RestorePlan) -> Vec<Action> {
        plan.notes.iter().map(|n| n.action.clone()).collect()
    }

    const APPLE_A: &str = "3F2A9C1B-0000-4000-8000-000000000001";
    const APPLE_B: &str = "7D4E2F10-0000-4000-8000-000000000002";

    #[test]
    fn restore_into_other_account_creates_dirty_rows_with_new_uuids() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        let b = seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::Create, Action::Create]);
        assert_eq!(plan.folders_to_create, vec!["Notes/Work".to_string()]);
        assert_eq!((report.created, report.replaced, report.skipped), (2, 0, 0));
        assert!(report.refused.is_empty() && report.losses.is_empty());

        for src_note in [&a, &b] {
            let got = by_title(&db, &dst, &src_note.title);
            assert_ne!(got.uuid, src_note.uuid);
            assert_eq!(got.sync_state, SyncState::Dirty);
            assert_eq!(got.body_html, src_note.body_html);
            assert_eq!(got.x_mail_created_date, src_note.x_mail_created_date);
            assert_eq!(got.label, src_note.label);
            assert_eq!(got.id, "");
            let row = db.restore_lookup(&src.id, &src_note.uuid, &dst.id).unwrap().unwrap();
            assert_eq!(row.target_uuid, got.uuid);
            assert_eq!(row.content_sha256, content_sha256(&src_note.body_html));
            assert_eq!(row.backup_id, "backup-1");
        }
        let work = db.list_folders(&dst.id).unwrap().into_iter().find(|f| f.path == "Notes/Work").unwrap();
        assert_eq!(work.sync_state, FolderSyncState::DirtyNew);
        // The source account is untouched.
        assert_eq!(db.list_notes(&src.id).unwrap().len(), 2);
    }

    #[test]
    fn restore_twice_creates_nothing_the_second_time() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);
        let before = db.list_notes(&dst.id).unwrap().len();

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored, Action::SkipAlreadyRestored]);
        assert!(plan.folders_to_create.is_empty());
        assert_eq!((report.created, report.skipped), (0, 2));
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), before);
    }

    #[test]
    fn already_restored_survives_an_edit_of_the_restored_copy() {
        // R10(a): the map's SOURCE sha decides, not the target's current body.
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);
        let copy = by_title(&db, &dst, "Alpha");
        db.apply_local_edit(&copy.uuid, &dst.id, "Alpha", "<div>Alpha</div><div>edited</div>", "Notes", &Actor::Human)
            .unwrap();

        let (plan, _) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored]);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), 1);
    }

    #[test]
    fn restore_from_a_later_unchanged_version_is_skipped() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut monday = backup(&db, &src);
        restore(&db, &dst, &mut monday);
        let before = db.list_notes(&dst.id).unwrap().len();

        let mut tuesday = backup(&db, &src);
        let (plan, report) = restore(&db, &dst, &mut tuesday);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored, Action::SkipAlreadyRestored]);
        assert_eq!(report.created, 0);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), before);
    }

    #[test]
    fn a_changed_version_against_an_earlier_restore_keeps_both() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &dst);
        let mut monday = backup(&db, &src);
        restore(&db, &dst, &mut monday);
        let first = by_title(&db, &dst, "Alpha");
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>v2</div>", "Notes", &Actor::Human)
            .unwrap();

        let mut tuesday = backup(&db, &src);
        let (plan, report) = restore(&db, &dst, &mut tuesday);
        assert_eq!(actions(&plan), vec![Action::KeepBoth { existing_uuid: first.uuid.clone() }]);
        assert_eq!(report.created, 1);
        let copy = by_title(&db, &dst, &format!("Alpha (restored {})", today()));
        assert_eq!(copy.body_html, "<div>Alpha</div><div>v2</div>");
        // The map now points at the newest copy, so a third run skips.
        let row = db.restore_lookup(&src.id, &a.uuid, &dst.id).unwrap().unwrap();
        assert_eq!(row.target_uuid, copy.uuid);
        let (again, _) = restore(&db, &dst, &mut tuesday);
        assert_eq!(actions(&again), vec![Action::SkipAlreadyRestored]);
    }

    #[test]
    fn an_older_version_restored_again_after_a_newer_one_creates_nothing() {
        // Monday's copy M is still live when Tuesday's restore repoints the
        // map at Tuesday's copy T; restoring Monday again must find M.
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &dst);
        let mut monday = backup(&db, &src);
        restore(&db, &dst, &mut monday);
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>v2</div>", "Notes", &Actor::Human)
            .unwrap();
        let mut tuesday = backup(&db, &src);
        restore(&db, &dst, &mut tuesday);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), 2);

        let (plan, report) = restore(&db, &dst, &mut monday);
        assert_eq!(actions(&plan), vec![Action::SkipContentMatch]);
        assert_eq!(report.created, 0);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), 2);
    }

    #[test]
    fn a_same_account_replace_can_be_repeated_after_another_edit() {
        let db = crate::test_support::temp_db();
        let src = gmail("a@x.com");
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &src);
        let mut bk = backup(&db, &src);
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>edited</div>", "Notes", &Actor::Human)
            .unwrap();
        let (_, first) = restore_with(&db, &src, &mut bk, ExistingChoice::Replace);
        assert_eq!(first.replaced, 1);

        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>edited again</div>", "Notes", &Actor::Human)
            .unwrap();
        let (plan, second) = restore_with(&db, &src, &mut bk, ExistingChoice::Replace);
        assert_eq!(actions(&plan), vec![Action::Replace { existing_uuid: a.uuid.clone() }]);
        assert_eq!((second.replaced, second.created), (1, 0));
        assert_eq!(db.get(&a.uuid, &src.id).unwrap().unwrap().body_html, a.body_html);
        assert_eq!(db.list_notes(&src.id).unwrap().len(), 1);

        // Nothing changed since: a third Replace has nothing to do.
        let (plan, _) = restore_with(&db, &src, &mut bk, ExistingChoice::Replace);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored]);
        // Keep both still treats the restored note as restored.
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>third edit</div>", "Notes", &Actor::Human)
            .unwrap();
        let (plan, _) = restore(&db, &src, &mut bk);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored]);
    }

    #[test]
    fn a_target_folder_being_deleted_refuses_its_notes() {
        // R15: re-creating the folder would leave its pending delete in place,
        // and the worker would push that delete with the restored notes inside.
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes/Work/Sub", "beta body");
        seed(&db, &src, "11111111-0000-4000-8000-000000000003", "Gamma", "Notes", "gamma body");
        db.create_folder_local_new(&dst.id, "Notes/Work").unwrap();
        db.force_folder_sync_state(&dst.id, "Notes/Work", FolderSyncState::DeletedPending);
        synced(&db, &dst);
        let mut bk = backup(&db, &src);

        let (plan, report) = restore(&db, &dst, &mut bk);
        let reason = "This note's folder is being deleted in this account; restore it under a new folder instead.";
        let by_title = |t: &str| plan.notes.iter().find(|n| n.title.starts_with(t)).unwrap().action.clone();
        assert_eq!(by_title("Alpha"), Action::Refuse { reason: reason.into() });
        // A folder whose ancestor is being deleted goes with it.
        assert_eq!(by_title("Beta"), Action::Refuse { reason: reason.into() });
        assert_eq!(by_title("Gamma"), Action::Create);
        assert!(plan.folders_to_create.is_empty(), "{:?}", plan.folders_to_create);
        assert_eq!((report.created, report.refused.len()), (1, 2));
        assert_eq!(db.folder_sync_state(&dst.id, "Notes/Work").unwrap(), Some(FolderSyncState::DeletedPending));
        assert_eq!(db.folder_sync_state(&dst.id, "Notes/Work/Sub").unwrap(), None);
        let titles: Vec<String> = db.list_notes(&dst.id).unwrap().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, vec!["Gamma".to_string()]);

        // Under a new folder is the way round it.
        let mut req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        req.placement = Placement::UnderNewFolder("Restored a@x.com 2026-10-24".into());
        let plan = plan_restore(&db, &dst, &bk.bundle, &req).unwrap();
        assert!(plan.notes.iter().all(|n| !matches!(n.action, Action::Refuse { .. })), "{:?}", plan.notes);
    }

    #[test]
    fn restore_on_another_device_is_caught_by_content_match() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), account(BackendKind::ICloud, "b@x.com"));
        // Tags make the iCloud body differ from the source (a Keywords line),
        // so this also pins R10(b): the match uses the planned final body.
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha #work body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);
        assert_ne!(by_title(&db, &dst, "Alpha").body_html, db.get(APPLE_A, &src.id).unwrap().unwrap().body_html);
        db.clear_restore_map_for_test();
        let before = db.list_notes(&dst.id).unwrap().len();

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::SkipContentMatch, Action::SkipContentMatch]);
        assert_eq!(report.created, 0);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), before);
    }

    #[test]
    fn same_account_default_keeps_both_with_suffix() {
        let db = crate::test_support::temp_db();
        let src = gmail("a@x.com");
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        synced(&db, &src);
        let mut bk = backup(&db, &src);
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>edited</div>", "Notes/Work", &Actor::Human)
            .unwrap();

        let (plan, report) = restore(&db, &src, &mut bk);
        assert_eq!(actions(&plan), vec![Action::KeepBoth { existing_uuid: a.uuid.clone() }]);
        let title = format!("Alpha (restored {})", today());
        assert_eq!(plan.notes[0].title, title);
        assert_eq!(report.created, 1);
        let copy = by_title(&db, &src, &title);
        assert_ne!(copy.uuid, a.uuid);
        assert_eq!(copy.body_html, a.body_html);
        assert_eq!(copy.label, "Notes/Work");
        assert_eq!(copy.sync_state, SyncState::Dirty);
        // The original keeps its edit.
        assert_eq!(db.get(&a.uuid, &src.id).unwrap().unwrap().body_html, "<div>Alpha</div><div>edited</div>");

        // Skip leaves everything alone.
        db.clear_restore_map_for_test();
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>edited twice</div>", "Notes/Work", &Actor::Human)
            .unwrap();
        // The copy made above is a content match for the backup, by its suffixed title.
        let (plan, _) = restore_with(&db, &src, &mut bk, ExistingChoice::Skip);
        assert_eq!(actions(&plan), vec![Action::SkipContentMatch]);
    }

    /// Live pass 2026-10-24: a same-account restore of notes still there,
    /// unchanged, with Keep both chosen, restored nothing because the content
    /// match found the original note itself. The owner's choice decides.
    #[test]
    fn same_account_unchanged_notes_follow_the_owners_choice() {
        let db = crate::test_support::temp_db();
        let src = gmail("a@x.com");
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        synced(&db, &src);
        let mut bk = backup(&db, &src);

        let (plan, _) = restore_with(&db, &src, &mut bk, ExistingChoice::Skip);
        assert_eq!(actions(&plan), vec![Action::SkipExisting]);
        let (plan, _) = restore_with(&db, &src, &mut bk, ExistingChoice::Replace);
        assert_eq!(actions(&plan), vec![Action::SkipContentMatch], "replacing identical text changes nothing");

        let (plan, report) = restore_with(&db, &src, &mut bk, ExistingChoice::KeepBoth);
        assert_eq!(actions(&plan), vec![Action::KeepBoth { existing_uuid: a.uuid.clone() }]);
        assert_eq!(report.created, 1);
        assert!(by_title(&db, &src, &format!("Alpha (restored {})", today())).uuid != a.uuid);

        // Running it again adds no second copy.
        let (plan, report) = restore_with(&db, &src, &mut bk, ExistingChoice::KeepBoth);
        assert_eq!(actions(&plan), vec![Action::SkipAlreadyRestored]);
        assert_eq!(report.created, 0);
    }

    #[test]
    fn same_account_skip_choice_skips_existing() {
        let db = crate::test_support::temp_db();
        let src = gmail("a@x.com");
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &src);
        let mut bk = backup(&db, &src);
        db.apply_local_edit(&a.uuid, &src.id, "Alpha", "<div>Alpha</div><div>edited</div>", "Notes", &Actor::Human)
            .unwrap();
        let (plan, report) = restore_with(&db, &src, &mut bk, ExistingChoice::Skip);
        assert_eq!(actions(&plan), vec![Action::SkipExisting]);
        assert_eq!((report.created, report.skipped), (0, 1));
        assert_eq!(db.list_notes(&src.id).unwrap().len(), 1);
    }

    #[test]
    fn replace_edits_in_place_and_downgrades_for_partial_icloud_notes() {
        let db = crate::test_support::temp_db();
        let src = account(BackendKind::ICloud, "a@x.com");
        let plain = seed(&db, &src, "0a1b2c3d-0000-4000-8000-000000000001", "Plain", "Notes", "plain body");
        let partial = seed(&db, &src, "0a1b2c3d-0000-4000-8000-000000000002", "Drawing", "Notes", "drawing body");
        db.set_render_loss(&src.id, &partial.uuid, Some("inline_objects")).unwrap();
        synced(&db, &src);
        let mut bk = backup(&db, &src);
        for n in [&plain, &partial] {
            db.apply_local_edit(&n.uuid, &src.id, &n.title, &format!("<div>{}</div><div>edited</div>", n.title), "Notes", &Actor::Human)
                .unwrap();
        }

        let (plan, report) = restore_with(&db, &src, &mut bk, ExistingChoice::Replace);
        let by_src = |u: &str| plan.notes.iter().find(|n| n.source_uuid == u).unwrap().clone();
        assert_eq!(by_src(&plain.uuid).action, Action::Replace { existing_uuid: plain.uuid.clone() });
        assert_eq!(by_src(&partial.uuid).action, Action::KeepBoth { existing_uuid: partial.uuid.clone() });
        assert_eq!(by_src(&partial.uuid).losses, vec![Loss::Partial { reason: "inline_objects".into() }]);
        assert_eq!((report.created, report.replaced), (1, 1));

        let replaced = db.get(&plain.uuid, &src.id).unwrap().unwrap();
        assert_eq!(replaced.body_html, plain.body_html);
        assert_eq!(replaced.sync_state, SyncState::Dirty);
        let last = db.events_for_test(&src.id, &plain.uuid).pop().unwrap();
        assert_eq!((last.kind.as_str(), last.actor.as_deref()), ("edit", Some("process:jodd-restore")));
        let row = db.restore_lookup(&src.id, &plain.uuid, &src.id).unwrap().unwrap();
        assert_eq!(row.target_uuid, plain.uuid);
        assert_eq!(row.content_sha256, content_sha256(&plain.body_html));

        // The partial note is untouched; its copy carries the backed-up body.
        assert_eq!(db.get(&partial.uuid, &src.id).unwrap().unwrap().body_html, "<div>Drawing</div><div>edited</div>");
        let copy = by_title(&db, &src, &format!("Drawing (restored {})", today()));
        assert_eq!(copy.body_html, partial.body_html);
        assert_eq!(db.list_notes(&src.id).unwrap().len(), 3);
    }

    /// Rewrite every concept file's text in a bundle zip with `f`.
    fn rewrite_concepts(path: &std::path::Path, f: impl Fn(&str) -> String) {
        let bytes = std::fs::read(path).unwrap();
        let mut src = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for i in 0..src.len() {
            let mut e = src.by_index(i).unwrap();
            let name = e.name().unwrap().into_owned();
            let mut buf = Vec::new();
            e.read_to_end(&mut buf).unwrap();
            if name.ends_with(".md") && name != "index.md" && name != "log.md" {
                buf = f(&String::from_utf8(buf).unwrap()).into_bytes();
            }
            out.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            out.write_all(&buf).unwrap();
        }
        out.finish().unwrap();
    }

    #[test]
    fn tampered_body_is_refused_and_not_written() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let file = write_backup_file(&db, &src);
        rewrite_concepts(file.path(), |t| t.replace("alpha body", "alpha BODY"));
        let mut bk = Backup { bundle: OpenBundle::open(file.path()).unwrap(), _file: file };

        let (plan, report) = restore(&db, &dst, &mut bk);
        let reason = "This note's content does not match its checksum.".to_string();
        assert_eq!(actions(&plan), vec![Action::Refuse { reason: reason.clone() }, Action::Create]);
        assert_eq!(report.refused, vec![(APPLE_A.to_string(), reason)]);
        assert_eq!(report.created, 1);
        let titles: Vec<String> = db.list_notes(&dst.id).unwrap().into_iter().map(|n| n.title).collect();
        assert_eq!(titles, vec!["Beta".to_string()]);
        assert!(db.restore_lookup(&src.id, APPLE_A, &dst.id).unwrap().is_none());
    }

    #[test]
    fn target_not_synced_refuses_whole_plan() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        let bk = backup(&db, &src);
        let req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        let err = plan_restore(&db, &dst, &bk.bundle, &req).err().expect("must refuse");
        assert_eq!(err, "TARGET_NOT_SYNCED_YET");
        assert!(db.list_notes(&dst.id).unwrap().is_empty());
    }

    fn attach(db: &Db, acct: &Account, uuid: &str, cid: &str, filename: &str, data: Vec<u8>) {
        db.upsert_attachment(
            &acct.id,
            uuid,
            &crate::backend::gmail::wire::Attachment {
                content_id: cid.into(),
                mime_type: "image/png".into(),
                filename: Some(filename.into()),
                x_apple_part_url: None,
                data,
            },
        )
        .unwrap();
    }

    #[test]
    fn microsoft_target_flattens_folders_and_drops_attachments() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), account(BackendKind::Microsoft, "b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "see <img src=\"cid:pic1\"> here");
        attach(&db, &src, APPLE_A, "pic1", "photo.png", vec![1, 2, 3]);
        synced(&db, &dst);
        let mut bk = backup(&db, &src);

        let (plan, report) = restore(&db, &dst, &mut bk);
        let want = vec![
            Loss::FolderFlattened { from: "Notes/Work".into() },
            Loss::Attachment { filename: "photo.png".into() },
        ];
        assert_eq!(plan.notes[0].losses, want);
        assert_eq!(plan.notes[0].target_folder, "Notes");
        assert!(plan.folders_to_create.is_empty());
        assert_eq!(report.losses, want.into_iter().map(|l| (APPLE_A.to_string(), l)).collect::<Vec<_>>());
        let got = by_title(&db, &dst, "Alpha");
        assert_eq!(got.label, "Notes");
        assert!(got.body_html.contains("<span>[Attachment not restored: photo.png]</span>"), "{}", got.body_html);
        assert!(!got.body_html.contains("cid:"));
        assert!(db.list_attachments(&dst.id, &got.uuid).unwrap().is_empty());
        assert!(db.list_folders(&dst.id).unwrap().iter().all(|f| f.path != "Notes/Work"));
    }

    #[test]
    fn a_cid_left_after_stripping_is_refused_for_targets_without_attachments() {
        // R9: a cid: reference strip_attachments does not recognise.
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), account(BackendKind::Ssh, "b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "<a href=\"cid:pic1\">link</a>");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        let (plan, report) = restore(&db, &dst, &mut bk);
        let reason = "This note references an attachment Jodd could not remove for this account.".to_string();
        assert_eq!(actions(&plan), vec![Action::Refuse { reason: reason.clone() }]);
        assert_eq!(report.refused, vec![(APPLE_A.to_string(), reason)]);
        assert!(db.list_notes(&dst.id).unwrap().is_empty());
    }

    #[test]
    fn prose_that_merely_contains_cid_colon_is_not_refused() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), account(BackendKind::Ssh, "b@x.com"));
        seed(&db, &src, APPLE_A, "Vitamins", "Notes", "Folic acid: 400 mcg");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::Create]);
        assert_eq!(report.created, 1);
        assert!(by_title(&db, &dst, "Vitamins").body_html.contains("Folic acid: 400 mcg"));
    }

    #[test]
    fn applying_the_same_plan_twice_creates_nothing_the_second_time() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "see <img src=\"cid:pic1\"> here");
        attach(&db, &src, APPLE_A, "pic1", "photo.png", vec![1, 2, 3]);
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        let req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        let plan = plan_restore(&db, &dst, &bk.bundle, &req).unwrap();

        let first = apply_restore(&db, &dst, &plan, &mut bk.bundle, "backup-1").unwrap();
        assert_eq!((first.created, first.skipped), (2, 0));
        let second = apply_restore(&db, &dst, &plan, &mut bk.bundle, "backup-1").unwrap();
        assert_eq!((second.created, second.replaced, second.skipped), (0, 0, 2));
        assert!(second.losses.is_empty());
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), 2);
        // No orphan attachment rows were written for the skipped note either.
        let alpha = by_title(&db, &dst, "Alpha");
        assert_eq!(db.list_attachments(&dst.id, &alpha.uuid).unwrap().len(), 1);
    }

    #[test]
    fn reapplying_after_a_partial_apply_writes_only_the_rest() {
        // An apply that stopped part-way (an error after the first note) left
        // a prefix; "Try again" with the same plan must finish, not duplicate.
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        let req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        let plan = plan_restore(&db, &dst, &bk.bundle, &req).unwrap();
        let prefix = RestorePlan { notes: plan.notes[..1].to_vec(), folders_to_create: plan.folders_to_create.clone() };
        apply_restore(&db, &dst, &prefix, &mut bk.bundle, "backup-1").unwrap();
        let written = db.list_notes(&dst.id).unwrap();
        assert_eq!(written.len(), 1);

        let report = apply_restore(&db, &dst, &plan, &mut bk.bundle, "backup-1").unwrap();
        assert_eq!((report.created, report.skipped), (1, 1));
        let mut titles: Vec<String> = db.list_notes(&dst.id).unwrap().into_iter().map(|n| n.title).collect();
        titles.sort();
        assert_eq!(titles, vec!["Alpha".to_string(), "Beta".to_string()]);
        assert!(db.list_notes(&dst.id).unwrap().iter().any(|n| n.uuid == written[0].uuid));
    }

    #[test]
    fn icloud_target_gets_keywords_and_reports_pin_loss() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), account(BackendKind::ICloud, "b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "about #work today");
        db.set_pin(APPLE_A, &src.id, true).unwrap();
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        assert_eq!(bk.bundle.notes()[0].doc.tags, vec!["work".to_string()]);

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(plan.notes[0].losses, vec![Loss::Pin, Loss::TagsAsKeywords]);
        assert_eq!(report.created, 1);
        let got = by_title(&db, &dst, "Alpha");
        assert!(got.body_html.contains("Keywords: work"), "{}", got.body_html);
        assert!(!got.pinned);
        assert_eq!(got.uuid, got.uuid.to_lowercase(), "iCloud mints lowercase record names");
    }

    #[test]
    fn gmail_to_gmail_carries_attachments_with_same_content_id_and_pin() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        let a = seed(&db, &src, APPLE_A, "Alpha", "Notes", "see <img src=\"cid:pic1\"> here");
        attach(&db, &src, APPLE_A, "pic1", "photo.png", vec![1, 2, 3]);
        db.set_pin(APPLE_A, &src.id, true).unwrap();
        synced(&db, &dst);
        let mut bk = backup(&db, &src);

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert!(plan.notes[0].losses.is_empty());
        assert!(report.losses.is_empty());
        let got = by_title(&db, &dst, "Alpha");
        assert_eq!(got.body_html, a.body_html);
        assert!(got.pinned && got.pin_dirty);
        let atts = db.list_attachments(&dst.id, &got.uuid).unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].content_id, "pic1");
        assert_eq!(atts[0].filename.as_deref(), Some("photo.png"));
        assert_eq!(db.attachment_bytes(&dst.id, &got.uuid, "pic1").unwrap(), Some(vec![1, 2, 3]));
    }

    #[test]
    fn provenance_tier_survives_restore() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        let mk = |uuid: &str, title: &str, actor: &Actor| {
            let n = crate::test_support::note(&src.id, uuid).title(title).build();
            db.insert_local_new(&n, actor).unwrap();
        };
        mk(APPLE_A, "Human", &Actor::Human);
        mk(APPLE_B, "Agent", &Actor::agent("claude-code", "2.1.0"));
        // Human-created, then changed with no event (rule 2b): Unreviewed.
        mk("11111111-0000-4000-8000-000000000003", "Gap", &Actor::Human);
        db.bump_local_version("11111111-0000-4000-8000-000000000003", &src.id).unwrap();
        synced(&db, &dst);
        let tier = |acct: &Account, uuid: &str| db.trust_for(&acct.id, &[uuid.to_string()]).unwrap()[uuid].tier;
        assert_eq!(tier(&src, APPLE_A), Tier::HumanWritten);
        assert_eq!(tier(&src, APPLE_B), Tier::Unreviewed);
        assert_eq!(tier(&src, "11111111-0000-4000-8000-000000000003"), Tier::Unreviewed);

        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);
        assert_eq!(tier(&dst, &by_title(&db, &dst, "Human").uuid), Tier::HumanWritten);
        assert_eq!(tier(&dst, &by_title(&db, &dst, "Agent").uuid), Tier::Unreviewed);
        assert_eq!(tier(&dst, &by_title(&db, &dst, "Gap").uuid), Tier::Unreviewed);
        // The restore added no event of its own to the human note.
        let events = db.events_for_test(&dst.id, &by_title(&db, &dst, "Human").uuid);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].kind.as_str(), events[0].local_version), ("create", None));
    }

    #[test]
    fn links_between_restored_notes_are_rewritten() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        // APPLE_B's id suffix is 7d4e2f10; Gamma is not in the restore set.
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "see [[Beta-7d4e2f10]] and [[Gamma-deadbeef]]");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);

        let beta = by_title(&db, &dst, "Beta");
        let suffix: String = beta.uuid.replace('-', "").to_lowercase()[..8].to_string();
        let alpha = by_title(&db, &dst, "Alpha");
        assert!(alpha.body_html.contains(&format!("[[Beta-{suffix}]]")), "{}", alpha.body_html);
        assert!(alpha.body_html.contains("[[Gamma-deadbeef]]"));
    }

    #[test]
    fn under_new_folder_placement_nests_everything() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes/Work", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        let mut req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        req.placement = Placement::UnderNewFolder("Restored a@x.com 2026-10-24".into());
        let plan = plan_restore(&db, &dst, &bk.bundle, &req).unwrap();
        assert_eq!(
            plan.folders_to_create,
            vec!["Notes/Restored a@x.com 2026-10-24".to_string(), "Notes/Restored a@x.com 2026-10-24/Work".to_string()]
        );
        apply_restore(&db, &dst, &plan, &mut bk.bundle, "backup-1").unwrap();
        assert_eq!(by_title(&db, &dst, "Alpha").label, "Notes/Restored a@x.com 2026-10-24/Work");
        assert_eq!(by_title(&db, &dst, "Beta").label, "Notes/Restored a@x.com 2026-10-24");
    }

    #[test]
    fn only_selected_notes_are_planned_and_unknown_ones_refused() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        seed(&db, &src, APPLE_B, "Beta", "Notes", "beta body");
        synced(&db, &dst);
        let bk = backup(&db, &src);
        let mut req = request(&bk.bundle, &dst, ExistingChoice::KeepBoth);
        req.source_uuids = vec![APPLE_B.into(), "missing".into(), APPLE_B.into()];
        let plan = plan_restore(&db, &dst, &bk.bundle, &req).unwrap();
        let got: Vec<(String, Action)> = plan.notes.iter().map(|n| (n.source_uuid.clone(), n.action.clone())).collect();
        assert_eq!(
            got,
            vec![
                (APPLE_B.to_string(), Action::Create),
                ("missing".to_string(), Action::Refuse { reason: "This note is not in the backup.".into() }),
            ]
        );
    }

    #[test]
    fn a_deleted_target_note_is_restored_again() {
        let db = crate::test_support::temp_db();
        let (src, dst) = (gmail("a@x.com"), gmail("b@x.com"));
        seed(&db, &src, APPLE_A, "Alpha", "Notes", "alpha body");
        synced(&db, &dst);
        let mut bk = backup(&db, &src);
        restore(&db, &dst, &mut bk);
        let copy = by_title(&db, &dst, "Alpha");
        db.mark_deleted(&copy.uuid, &dst.id).unwrap();

        let (plan, report) = restore(&db, &dst, &mut bk);
        assert_eq!(actions(&plan), vec![Action::Create]);
        assert_eq!(report.created, 1);
        assert_eq!(db.list_notes(&dst.id).unwrap().len(), 1);
    }
}
