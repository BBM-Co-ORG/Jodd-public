//! `list_views` / `query_notes`: the app's view engine (`Db::run_view`) for
//! agents. One engine for both — only the `hidden` predicate differs.

use crate::scope::WriteScope;
use chrono::NaiveDate;
use jodd_lib::accounts::Account;
use jodd_lib::views::query::parse_view;
use jodd_lib::views::run::{ViewDef, ViewRows};
use serde_json::{json, Value};

/// Every view the agent may see, with its lines and any parse errors. A view
/// in a hidden folder is omitted (gotcha #33).
pub fn do_list_views(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    account_id: &str,
    scope: &WriteScope,
) -> Result<Vec<ViewDef>, String> {
    crate::write::resolve_active_account(accounts, account_id)?;
    Ok(db
        .list_views(account_id)
        .map_err(|e| format!("list_views: {e}"))?
        .into_iter()
        .filter(|v| !scope.is_hidden(account_id, &v.label))
        .collect())
}

/// Run a saved view (`view`: name or uuid) or ad-hoc `lines`; exactly one.
///
/// Deadlock guard: `Db::run_view` calls `hidden` while it holds the DB lock.
/// The closure below captures only `scope` (an owned, in-memory struct loaded
/// by the caller before this call) and the account id, and reads no `Db`.
pub fn do_query_notes(
    db: &jodd_lib::db::Db,
    accounts: &[Account],
    account_id: &str,
    scope: &WriteScope,
    view: Option<&str>,
    lines: Option<&[String]>,
    today: NaiveDate,
) -> Result<Value, String> {
    crate::write::resolve_active_account(accounts, account_id)?;
    let props = match (view, lines) {
        (Some(_), Some(_)) | (None, None) => {
            return Err("Pass exactly one of `view` (a saved view's name or uuid; see list_views) or `lines` (ad-hoc condition lines such as \"Show: tasks\").".into())
        }
        (Some(v), None) => {
            let def = pick_view(do_list_views(db, accounts, account_id, scope)?, v)?;
            if !def.errors.is_empty() {
                return Err(cannot_run(&def.name, &def.errors));
            }
            db.props_for_note(account_id, &def.uuid).map_err(|e| format!("query_notes: {e}"))?
        }
        (None, Some(lines)) => {
            let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
            let props = jodd_lib::props::parse_property_block(&refs);
            // The block ends at the first non-property line; dropping the rest
            // silently would widen the query, so refuse instead.
            if let Some(bad) = lines.get(props.len()) {
                return Err(format!(
                    "line {} is not a `Key: value` line (a blank or malformed line ends the block): {bad:?}",
                    props.len() + 1
                ));
            }
            props
        }
    };
    let q = parse_view(&props).map_err(|e| cannot_run("this query", &e))?;
    let result = db
        .run_view(account_id, &q, &|label| scope.is_hidden(account_id, label), today)
        .map_err(|e| format!("run_view: {e}"))?;
    let (show, rows) = match &result.rows {
        ViewRows::Notes(r) => ("notes", serde_json::to_value(r)),
        ViewRows::Tasks(r) => ("tasks", serde_json::to_value(r)),
    };
    let rows = rows.map_err(|e| format!("query_notes: serializing rows: {e}"))?;
    Ok(json!({ "show": show, "rows": rows, "notices": result.notices }))
}

fn cannot_run(name: &str, errors: &[jodd_lib::views::query::ViewError]) -> String {
    format!(
        "{name} cannot run: {}",
        serde_json::to_string(errors).unwrap_or_else(|_| "unreadable clauses".into())
    )
}

/// The uuid if it names a visible view, else the one visible view with this
/// name (case-insensitive). Several by name is an error that lists them.
fn pick_view(views: Vec<ViewDef>, key: &str) -> Result<ViewDef, String> {
    if let Some(v) = views.iter().find(|v| v.uuid == key) {
        return Ok(v.clone());
    }
    let mut named: Vec<ViewDef> = views.into_iter().filter(|v| v.name.trim().to_lowercase() == key.trim().to_lowercase()).collect();
    match named.len() {
        0 => Err(format!("No view named or with uuid {key:?}. Call list_views to see the views.")),
        1 => Ok(named.remove(0)),
        _ => Err(format!(
            "{} views are named {key:?}; pass one's uuid instead: {}",
            named.len(),
            named.iter().map(|v| format!("{} (in {})", v.uuid, v.label)).collect::<Vec<_>>().join("; ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct() -> Account {
        serde_json::from_str(r#"{"id":"a@x.com","email":"a@x.com","added_at":"2026-01-01T00:00:00Z"}"#).unwrap()
    }
    fn scope(hidden: &[&str]) -> WriteScope {
        serde_json::from_str(&format!(
            r#"{{"accounts":{{"a@x.com":{{"allowed_folders":["Notes"],"hidden_folders":{}}}}}}}"#,
            serde_json::to_string(hidden).unwrap()
        ))
        .unwrap()
    }
    fn db() -> (tempfile::TempDir, jodd_lib::db::Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = jodd_lib::db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        (dir, db)
    }
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 19).unwrap()
    }
    /// A note whose cached body is `html`, written the way a sync/edit does.
    fn put(db: &jodd_lib::db::Db, label: &str, title: &str, html: &str) -> String {
        let uuid = crate::write::do_create_note(
            db, &[acct()], &scope_all(), "a@x.com", label, title, "seed", &jodd_lib::provenance::Actor::test(),
        )
        .unwrap();
        db.apply_local_edit(&uuid, "a@x.com", title, html, label, &jodd_lib::provenance::Actor::test()).unwrap();
        uuid
    }
    fn scope_all() -> WriteScope {
        serde_json::from_str(r#"{"accounts":{"a@x.com":{"allowed_folders":["Notes"]}}}"#).unwrap()
    }
    fn view_note(db: &jodd_lib::db::Db, label: &str, title: &str, lines: &[&str]) -> String {
        // A cached Gmail body carries no title line (`title_lines_in_cached_body`).
        let body: String = lines.iter().map(|l| format!("<div>{l}</div>")).collect();
        put(db, label, title, &body)
    }
    const TASKS: &str = "<div>Chores</div><ul><li><input type=\"checkbox\">buy milk</li><li><input type=\"checkbox\">call mum</li></ul>";

    #[test]
    fn query_notes_hidden_folder_excluded() {
        let (_d, db) = db();
        put(&db, "Notes/Private", "Secret", TASKS);
        let open = put(&db, "Notes/Open", "Chores", TASKS);
        let lines = vec!["Show: tasks".to_string(), "Done: no".to_string()];
        let q = |s: &WriteScope| do_query_notes(&db, &[acct()], "a@x.com", s, None, Some(&lines), today()).unwrap();
        let all = q(&scope(&[]));
        assert_eq!(all["rows"].as_array().unwrap().len(), 4, "{all}");
        let hid = q(&scope(&["Notes/Private"]));
        let rows = hid["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{hid}");
        assert!(rows.iter().all(|r| r["uuid"] == open.as_str()));
    }

    #[test]
    fn query_notes_task_idx_matches_list_tasks() {
        let (_d, db) = db();
        let uuid = put(&db, "Notes/Open", "Chores", TASKS);
        let lines = vec!["Show: tasks".to_string(), "Done: no".to_string()];
        let v = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), None, Some(&lines), today()).unwrap();
        let listed = crate::write::do_list_tasks(&db, &[acct()], "a@x.com", None, false, &scope(&[])).unwrap();
        let want: Vec<(usize, String)> = listed.iter().filter(|n| n.uuid == uuid).flat_map(|n| n.tasks.iter().map(|t| (t.index, t.text.clone()))).collect();
        let mut got: Vec<(usize, String)> = v["rows"].as_array().unwrap().iter()
            .map(|r| (r["idx"].as_u64().unwrap() as usize, r["text"].as_str().unwrap().to_string())).collect();
        got.sort();
        assert_eq!(want.len(), 2);
        assert_eq!(got, want);
    }

    #[test]
    fn query_notes_requires_exactly_one_of_view_or_lines() {
        let (_d, db) = db();
        let l = vec!["Show: notes".to_string()];
        let both = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some("x"), Some(&l), today()).unwrap_err();
        let neither = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), None, None, today()).unwrap_err();
        assert!(both.contains("exactly one") && neither.contains("exactly one"));
    }

    #[test]
    fn list_views_omits_hidden() {
        let (_d, db) = db();
        view_note(&db, "Notes/__Views__", "Open view", &["Show: notes", "Tag: x"]);
        view_note(&db, "Notes/Private/__Views__", "Secret view", &["Show: notes", "Tag: x"]);
        let all = do_list_views(&db, &[acct()], "a@x.com", &scope(&[])).unwrap();
        assert_eq!(all.len(), 2);
        let some = do_list_views(&db, &[acct()], "a@x.com", &scope(&["Notes/Private"])).unwrap();
        assert_eq!(some.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["Open view"]);
        // A hidden view cannot be run by name either.
        let e = do_query_notes(&db, &[acct()], "a@x.com", &scope(&["Notes/Private"]), Some("Secret view"), None, today()).unwrap_err();
        assert!(e.contains("No view"), "{e}");
    }

    #[test]
    fn a_view_runs_by_name_or_uuid_and_ambiguity_and_errors_are_reported() {
        let (_d, db) = db();
        put(&db, "Notes/Open", "Chores", TASKS);
        let a = view_note(&db, "Notes/__Views__", "Todo", &["Show: tasks", "Done: no"]);
        let by_name = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some("todo"), None, today()).unwrap();
        let by_uuid = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some(&a), None, today()).unwrap();
        assert_eq!(by_name["rows"].as_array().unwrap().len(), 2);
        assert_eq!(by_name, by_uuid);
        // Same name in a second view folder: refuse to pick.
        view_note(&db, "Notes/Work/__Views__", "Todo", &["Show: tasks"]);
        let e = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some("Todo"), None, today()).unwrap_err();
        assert!(e.contains("2 views are named") && e.contains(&a), "{e}");
        // An unparseable view returns its errors.
        view_note(&db, "Notes/__Views__", "Broken", &["Show: notes", "Pinned: maybe"]);
        let e = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some("Broken"), None, today()).unwrap_err();
        assert!(e.contains("cannot run") && e.contains("Pinned"), "{e}");
        let listed = do_list_views(&db, &[acct()], "a@x.com", &scope(&[])).unwrap();
        assert!(listed.iter().any(|v| v.name == "Broken" && !v.errors.is_empty()));
    }

    #[test]
    fn lines_with_an_unparsed_tail_are_refused_not_widened() {
        let (_d, db) = db();
        let l = vec!["Show: notes".to_string(), "Tag: x".to_string(), "".to_string(), "Folder: Notes/A".to_string()];
        let e = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), None, Some(&l), today()).unwrap_err();
        assert!(e.contains("line 3"), "{e}");
        let none: Vec<String> = vec![];
        assert!(do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), None, Some(&none), today()).is_err());
    }

    fn prop_keys(db: &jodd_lib::db::Db, uuid: &str) -> Vec<String> {
        db.props_for_note("a@x.com", uuid).unwrap().into_iter().map(|p| p.key).collect()
    }

    /// Final review I2: spec §8.6 says an agent makes a view with
    /// `create_note` into `__Views__`. Through plain Markdown the natural body
    /// is one paragraph that reads as ONE line, `Show: tasks Done: no`, and the
    /// view cannot run. The property block must reach SQLite a line per line.
    #[test]
    fn a_view_created_through_create_note_parses_and_runs() {
        let (_d, db) = db();
        put(&db, "Notes/Open", "Chores", TASKS);
        let view = crate::write::do_create_note(
            &db, &[acct()], &scope_all(), "a@x.com", "Notes/__Views__", "Open tasks",
            "Show: tasks\nDone: no\n\nsome text", &jodd_lib::provenance::Actor::test(),
        )
        .unwrap();
        assert_eq!(prop_keys(&db, &view), ["show", "done"]);
        let body = db.note_by_uuid("a@x.com", &view).unwrap().unwrap().body_html;
        assert!(body.contains("<div>Show: tasks</div><div>Done: no</div>"), "{body}");
        assert!(body.contains("some text"), "the rest is kept: {body}");

        let v = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some(&view), None, today()).unwrap();
        assert_eq!(v["show"], "tasks", "{v}");
        assert_eq!(v["rows"].as_array().unwrap().len(), 2, "{v}");
    }

    #[test]
    fn a_view_written_through_update_note_parses_and_runs() {
        let (_d, db) = db();
        put(&db, "Notes/Open", "Chores", TASKS);
        let actor = jodd_lib::provenance::Actor::test();
        // Replace: the whole body, so its head is a block.
        let view = crate::write::do_create_note(&db, &[acct()], &scope_all(), "a@x.com", "Notes/__Views__", "Replaced", "seed", &actor).unwrap();
        crate::write::do_update_note(
            &db, &[acct()], &scope_all(), "a@x.com", &view, "Show: notes\nFolder: Notes/Open",
            crate::write::UpdateMode::Replace, None, false, &actor,
        )
        .unwrap();
        assert_eq!(prop_keys(&db, &view), ["show", "folder"]);
        let v = do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some(&view), None, today()).unwrap();
        assert_eq!(v["show"], "notes", "{v}");
        assert_eq!(v["rows"].as_array().unwrap().len(), 1, "{v}");

        // Append onto a note with no text yet: the appended head is the block.
        let empty = crate::write::do_create_note(&db, &[acct()], &scope_all(), "a@x.com", "Notes/__Views__", "Appended", "", &actor).unwrap();
        crate::write::do_update_note(
            &db, &[acct()], &scope_all(), "a@x.com", &empty, "Show: tasks\nDone: no",
            crate::write::UpdateMode::Append, None, false, &actor,
        )
        .unwrap();
        assert_eq!(prop_keys(&db, &empty), ["show", "done"]);
        assert!(do_query_notes(&db, &[acct()], "a@x.com", &scope(&[]), Some(&empty), None, today()).is_ok());

        // Append onto text: not a block, so the Markdown rendering is unchanged.
        let prose = crate::write::do_create_note(&db, &[acct()], &scope_all(), "a@x.com", "Notes/Open", "Prose", "hello", &actor).unwrap();
        crate::write::do_update_note(
            &db, &[acct()], &scope_all(), "a@x.com", &prose, "Alpha: one\nBeta: two",
            crate::write::UpdateMode::Append, None, false, &actor,
        )
        .unwrap();
        let body = db.note_by_uuid("a@x.com", &prose).unwrap().unwrap().body_html;
        assert!(!body.contains("<div>Alpha: one</div>"), "{body}");
        assert!(body.contains("Alpha: one"), "{body}");
    }
}
