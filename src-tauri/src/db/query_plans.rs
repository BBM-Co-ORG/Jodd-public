// The queries that run every few seconds while the user types must not
// read every note.
//
// They hold `Db`'s one lock, and Tauri runs a synchronous command on the
// main thread, which on macOS is also the thread that takes keystrokes. A
// full scan of `notes` reads every note's body, because the columns these
// queries filter on are stored after `body_html`. On 2026-11-18 a 45 s
// sample of typing in Jodd found 2.3 s of the main thread waiting for this
// lock inside `count_pending_pushes`.
//
// EXPLAIN QUERY PLAN on a freshly migrated database. A step on the notes
// table (by its name or its alias) passes when it reads no table rows (a
// covering index) or reads only the rows a queue index lists. A SCAN fails,
// and so does a SEARCH by account alone: `idx_notes_account_label
// (account_id=?)` reads every note of the account, which for a user with one
// account is every note there is. The small `folders` table is not checked.

use super::folders::CLEAR_FOLDER_WAIT_BLOCKS_SQL;
use super::{fts_set_folder_sql, FTS_DELETE_SQL};
use super::*;

fn plan(db: &Db, sql: &str) -> Vec<String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = stmt.parameter_count();
    let args: Vec<String> = (0..n).map(|_| "x".to_string()).collect();
    stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| r.get::<_, String>(3))
        .unwrap()
        .collect::<SqlResult<Vec<_>>>()
        .unwrap()
}

/// Indexes that hold only rows some queue is waiting on, so reading the
/// table through them touches a handful of notes, not all of them.
const QUEUE_INDEXES: &[&str] = &["idx_notes_sync_queue", "idx_notes_push_blocked", "idx_notes_pin_dirty"];

/// Every step of `sql`'s plan that reads note rows other than queue rows.
fn notes_scans(db: &Db, sql: &str) -> Vec<String> {
    plan(db, sql)
        .into_iter()
        .filter(|d| {
            let mut w = d.split_whitespace();
            let (Some(op), Some(table)) = (w.next(), w.next()) else { return false };
            if !matches!(op, "SCAN" | "SEARCH") || !matches!(table, "notes" | "n") {
                return false;
            }
            let rest: Vec<&str> = w.collect();
            let covering = rest.windows(2).any(|p| p == ["COVERING", "INDEX"]);
            let queue = rest.iter().any(|t| QUEUE_INDEXES.contains(t));
            !(covering || queue)
        })
        .collect()
}

fn assert_no_notes_scan(name: &str, sql: &str) {
    let db = crate::test_support::temp_db();
    let scans = notes_scans(&db, sql);
    assert!(scans.is_empty(), "{name} reads every note: {scans:?}\nfull plan: {:?}", plan(&db, sql));
}

#[test]
fn the_dirty_queue_is_found_by_index() {
    assert_no_notes_scan("list_dirty", LIST_DIRTY_SQL);
}

#[test]
fn the_delete_queue_is_found_by_index() {
    assert_no_notes_scan("list_deleted_pending", LIST_DELETED_PENDING_SQL);
}

#[test]
fn every_pending_push_count_is_found_by_index() {
    for (name, sql) in [
        ("notes", PENDING_NOTES_SQL),
        ("blocked", PENDING_BLOCKED_SQL),
        ("deletes", PENDING_DELETES_SQL),
        ("pins", PENDING_PINS_SQL),
        ("folders", PENDING_FOLDERS_SQL),
        ("has_pending_pushes", HAS_PENDING_SQL),
    ] {
        assert_no_notes_scan(name, sql);
    }
}

#[test]
fn re_arming_folder_waits_is_found_by_index() {
    assert_no_notes_scan("clear_folder_wait_blocks", CLEAR_FOLDER_WAIT_BLOCKS_SQL);
}

/// The check itself must be able to fail: a filter on an unindexed column
/// is a scan.
#[test]
fn the_check_sees_a_scan() {
    let db = crate::test_support::temp_db();
    assert!(!notes_scans(&db, "SELECT COUNT(*) FROM notes WHERE title = ?1").is_empty());
    assert!(!notes_scans(&db, "SELECT 1 FROM notes n WHERE n.title = ?1").is_empty());
    // A search by account alone reads every note of the account.
    assert!(!notes_scans(&db, "SELECT COUNT(*) FROM notes WHERE account_id = ?1 AND title = ?2").is_empty());
}



// The search index. `notes_fts`'s `uuid` and `account_id` are UNINDEXED, so
// a statement that finds one note's row by them reads the whole index —
// 50–110 ms on a 67 MB encrypted index of 3,000 notes (debug build,
// 2026-11-18), against 0.7 ms by rowid. Every save deletes and re-inserts
// its note's row, so it must go by rowid (`notes_fts_rowid`). FTS5 reports a
// full scan as `VIRTUAL TABLE INDEX 0:` with nothing after the colon.

fn fts_full_scans(db: &Db, sql: &str) -> Vec<String> {
    plan(db, sql)
        .into_iter()
        .filter(|d| d.starts_with("SCAN notes_fts VIRTUAL TABLE INDEX") && d.ends_with(':'))
        .collect()
}

#[test]
fn one_notes_search_row_is_found_by_rowid() {
    let db = crate::test_support::temp_db();
    for (name, sql) in [
        ("fts delete", FTS_DELETE_SQL.to_string()),
        ("folder trigger", fts_set_folder_sql("?1", "?2", "?3")),
    ] {
        let scans = fts_full_scans(&db, &sql);
        assert!(scans.is_empty(), "{name} reads the whole search index: {scans:?}");
    }
}

#[test]
fn the_fts_check_sees_a_scan() {
    let db = crate::test_support::temp_db();
    assert!(!fts_full_scans(&db, "DELETE FROM notes_fts WHERE uuid = ?1 AND account_id = ?2").is_empty());
}
