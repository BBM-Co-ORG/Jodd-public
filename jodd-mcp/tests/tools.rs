use jodd_lib::db::{CachedNote, Db, SyncState};

fn temp_db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    std::mem::forget(dir);
    Db::open(&path).expect("open temp db")
}

fn mk_note(account_id: &str, uuid: &str, title: &str, body_html: &str) -> CachedNote {
    CachedNote {
        uuid: uuid.to_string(),
        account_id: account_id.to_string(),
        id: String::new(),
        title: title.to_string(),
        body_html: body_html.to_string(),
        date: "Thu, 4 Jun 2026 01:19:50 +0700".to_string(),
        x_mail_created_date: None,
        label: "Notes".to_string(),
        local_version: 0,
        remote_version: None,
        sync_state: SyncState::Clean,
        last_synced_at: None,
        last_local_modified_at: 0,
        last_remote_modified_at: None,
        pinned: false,
        meta_msg_id: None,
        pin_dirty: false,
        tags_meta_msg_id: None,
        tags_dirty: false,
    }
}

#[test]
fn db_search_notes_finds_seeded_note() {
    let db = temp_db();
    let acct = "test@example.com";
    db.insert_local_new(&mk_note(acct, "AAAAAAAA-0000-0000-0000-000000000000", "Meeting Notes", "<div>quarterly planning</div>"))
        .unwrap();

    let results = db.search_notes(Some(acct), None, "quarterly", &[]).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title, "Meeting Notes");
}

/// Deactivating an account in the app must also hide it here. jodd-mcp reads
/// the same SQLite file directly, so without this "not one can see" has a hole
/// the size of every Claude Code session.
#[test]
fn search_hides_notes_from_dismissed_accounts() {
    let db = temp_db();
    db.insert_local_new(&mk_note(
        "a@x",
        "AAAAAAAA-0000-0000-0000-00000000000a",
        "Shared Title",
        "<div>quarterly planning</div>",
    ))
    .unwrap();
    db.insert_local_new(&mk_note(
        "b@x",
        "BBBBBBBB-0000-0000-0000-00000000000b",
        "Shared Title",
        "<div>quarterly planning</div>",
    ))
    .unwrap();

    assert_eq!(db.search_notes(None, None, "quarterly", &[]).unwrap().len(), 2);

    let hidden = vec!["b@x".to_string()];
    let rows = db.search_notes(None, None, "quarterly", &hidden).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account_id, "a@x");
}

#[test]
fn db_note_connections_finds_backlink_and_outgoing() {
    let db = temp_db();
    let acct = "test@example.com";
    let target_uuid = "BBBBBBBB-0000-0000-0000-000000000000";
    db.insert_local_new(&mk_note(acct, target_uuid, "Target Note", "<div>content</div>")).unwrap();
    let linker_uuid = "CCCCCCCC-0000-0000-0000-000000000000";
    db.insert_local_new(&mk_note(acct, linker_uuid, "Linker Note", "<div>[[Target Note]]</div>")).unwrap();

    let backlinks = db.backlinks(acct, target_uuid).unwrap();
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0].uuid, linker_uuid);

    let outgoing = db.outgoing_links(acct, linker_uuid).unwrap();
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].uuid, target_uuid);
}
