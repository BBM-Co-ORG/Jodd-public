//! Whole flows: SSH vertical (over a local sh) ↔ SQLite, through the same
//! reconcile function the app runs. Gotcha #6's lesson applied: trace to the
//! row, not to "the script exited 3".

use std::collections::HashMap;

use super::read::tests::vault;
use crate::backend::{NoteStore, SaveOp, TransportError};
use crate::db::{self, Db, SyncState};
use crate::reconcile::reconcile_one_db;

const ACCT: &str = "ssh:test";
const TODO: &str = "<div class=\"jodd-task\"><input type=\"checkbox\" contenteditable=\"false\">&nbsp;ship it</div>";

fn open_db(d: &tempfile::TempDir) -> Db {
    Db::open_unencrypted(&d.path().join(".jodd-test-db")).unwrap()
}

async fn pull(v: &super::SshVertical, db: &Db) {
    let (notes, _) = v.list_all_notes(&HashMap::new()).await.unwrap();
    for n in &notes {
        reconcile_one_db(db, ACCT, n, db::RemotePin::LocalWins, "c0ffee00-0000-4000-8000-000000000001");
    }
}

fn create<'a>(title: &'a str, body: &'a str) -> SaveOp<'a> {
    SaveOp { title, body_html: body, existing_remote_id: None, existing_uuid: None, existing_created_date: None, label: "Notes", base_version: None }
}

/// Exactly the `SaveOp` `push_one_dirty` builds from a row.
fn from_row(row: &db::CachedNote) -> SaveOp<'_> {
    SaveOp {
        title: &row.title,
        body_html: &row.body_html,
        existing_remote_id: Some(&row.id),
        existing_uuid: Some(&row.uuid),
        existing_created_date: row.x_mail_created_date.as_deref(),
        label: &row.label,
        base_version: row.remote_version.as_deref(),
    }
}

#[tokio::test]
async fn an_outside_edit_while_dirty_becomes_a_keep_both_conflict() {
    let (d, v) = vault();
    let db = open_db(&d);
    let saved = v.save_full(&create("Plan", "<div>original</div>")).await.unwrap();
    pull(&v, &db).await;
    assert_eq!(db.get(&saved.uuid, ACCT).unwrap().unwrap().remote_version.as_deref(), Some(saved.version.as_str()));

    db.apply_local_edit(&saved.uuid, ACCT, "Plan", "<div>mine</div>", "Notes").unwrap();
    let path = d.path().join(&saved.id);
    let theirs = std::fs::read_to_string(&path).unwrap().replace("original", "edited on the server");
    std::fs::write(&path, &theirs).unwrap();

    let row = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    let err = v.save_full(&from_row(&row)).await.unwrap_err();
    assert!(matches!(err, TransportError::Conflict { .. }), "{err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs, "the server's edit survived");

    pull(&v, &db).await;
    let primary = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    assert!(primary.body_html.contains("edited on the server"), "{}", primary.body_html);
    let copy = db.get("c0ffee00-0000-4000-8000-000000000001", ACCT).unwrap().expect("conflict copy");
    assert!(copy.body_html.contains("mine"), "{}", copy.body_html);
}

/// I-7: the worker never pulls SSH on its own, so a note whose push
/// conflicts in a folder nobody is viewing (or on a Draining account) would
/// retry forever. The worker's conflict arm refetches by uuid and
/// reconciles — this is that step, through the same DB function.
#[tokio::test]
async fn a_conflict_refetch_by_uuid_produces_the_keep_both_copy_without_a_pull() {
    let (d, v) = vault();
    let db = open_db(&d);
    let saved = v.save_full(&create("Plan", "<div>original</div>")).await.unwrap();
    pull(&v, &db).await;
    db.apply_local_edit(&saved.uuid, ACCT, "Plan", "<div>mine</div>", "Notes").unwrap();
    let path = d.path().join(&saved.id);
    std::fs::write(&path, std::fs::read_to_string(&path).unwrap().replace("original", "edited on the server")).unwrap();
    let row = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    assert!(matches!(v.save_full(&from_row(&row)).await, Err(TransportError::Conflict { .. })));

    let ids = v.find_ids_for_uuid(&row.uuid).await.unwrap();
    let fetched = v.fetch_note(&ids[0]).await.unwrap();
    let outcome = crate::sync_worker::reconcile_after_conflict_db(&db, ACCT, crate::accounts::BackendKind::Ssh, &fetched);
    assert!(outcome.changed, "the worker must tell the frontend (gotcha #6)");
    let copy_uuid = outcome.copy_uuid.expect("the copy's uuid, so its push can nudge the list");
    assert!(db.get(&copy_uuid, ACCT).unwrap().unwrap().body_html.contains("mine"));

    // The editor still holds `row.local_version`; its next versioned save
    // would otherwise land the pre-conflict text over the server's.
    let stale = db.apply_local_edit_versioned(&saved.uuid, ACCT, "Plan", "<div>mine, typed on</div>", "Notes", row.local_version).unwrap();
    assert!(!stale, "a save against the pre-conflict version must be refused");
    assert!(db.get(&saved.uuid, ACCT).unwrap().unwrap().body_html.contains("edited on the server"));

    let rows = db.list_notes(ACCT).unwrap();
    assert_eq!(rows.len(), 2, "primary + conflict copy");
    let primary = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    assert!(primary.body_html.contains("edited on the server"), "{}", primary.body_html);
    assert_ne!(primary.sync_state, SyncState::Dirty, "the conflicted row stops retrying");
    assert!(rows.iter().any(|r| r.uuid != saved.uuid && r.body_html.contains("mine")), "the local edit survives as a copy");
}

/// The LocalFs regression, for this backend: the version `save_full`
/// reports must equal what the next listing reports, or every push would
/// read back as a remote change.
#[tokio::test]
async fn a_push_does_not_manufacture_a_conflict_on_the_next_pull() {
    let (d, v) = vault();
    let db = open_db(&d);
    let saved = v.save_full(&create("Plan", "<div>one</div>")).await.unwrap();
    pull(&v, &db).await;
    db.apply_local_edit(&saved.uuid, ACCT, "Plan", "<div>two</div>", "Notes").unwrap();
    let row = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    let pushed = v.save_full(&from_row(&row)).await.unwrap();
    db.mark_pushed(&saved.uuid, ACCT, &pushed.id, &pushed.version, &pushed.date, &pushed.body_html, row.local_version).unwrap();
    pull(&v, &db).await;
    let rows = db.list_notes(ACCT).unwrap();
    assert_eq!(rows.len(), 1, "no conflict copy");
    assert_eq!(rows[0].sync_state, SyncState::Clean);
}

/// Spec Q4-C: a program on the server ticks a box with sed.
#[tokio::test]
async fn a_sed_tick_on_the_server_arrives_as_a_checked_box() {
    let (d, v) = vault();
    let db = open_db(&d);
    let saved = v.save_full(&create("Tasks", TODO)).await.unwrap();
    pull(&v, &db).await;
    let status = std::process::Command::new("sed")
        .args(["-i.bak", r"s/- \[ \]/- [x]/"])
        .arg(d.path().join(&saved.id))
        .status()
        .unwrap();
    assert!(status.success());
    pull(&v, &db).await;
    let row = db.get(&saved.uuid, ACCT).unwrap().unwrap();
    assert!(row.body_html.contains("checked"), "{}", row.body_html);
    assert_eq!(row.sync_state, SyncState::Clean);
}

/// Spec Q7-A: an agent on the server writes a new note in plain Markdown.
#[tokio::test]
async fn a_file_an_agent_drops_becomes_a_note() {
    let (d, v) = vault();
    let db = open_db(&d);
    std::fs::create_dir_all(d.path().join("Notes/Inbox")).unwrap();
    std::fs::write(d.path().join("Notes/Inbox/summary.md"), "# Daily summary\n\n- [ ] follow up #work\n").unwrap();
    pull(&v, &db).await;
    let rows = db.list_notes(ACCT).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].title.as_str(), rows[0].label.as_str(), rows[0].id.as_str()), ("Daily summary", "Notes/Inbox", "Notes/Inbox/summary.md"));
    assert!(rows[0].body_html.contains("checkbox") && rows[0].body_html.contains("#work"));
}
