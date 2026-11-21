//! View commands: list, run, preview and create saved views (spec
//! 2026-11-19-properties-tasks-views §8).
//!
//! A view is a note in `Notes/__Views__` whose property block is a query.
//! Everything here reads the derived SQLite tables and runs off the main
//! thread (`db_blocking`); the one write, `create_view`, is a local note
//! insert that the sync worker pushes, never a remote call.

use tauri::State;

use crate::views::query::parse_view;
use crate::views::run::{ViewDef, ViewResult};
use crate::{accounts, backend, db, refuse_write, save_note_db, AppState};

/// Where views live: a workflow folder, so the sidebar shows it as "Views".
const VIEWS_FOLDER: &str = "__Views__";

/// A new view's title and starting query.
const NEW_VIEW_TITLE: &str = "New view";
const NEW_VIEW_BODY: &str = "<div>Show: notes</div><div>Folder: Inbox</div>";

/// The account's views. Empty when it has none.
#[tauri::command]
pub async fn list_views(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ViewDef>, String> {
    crate::db_blocking(&state, move |db| {
        db.list_views(&account_id).map_err(|e| format!("list_views: {e}"))
    })
    .await
}

/// Run a saved view. A view that does not parse is `Err` carrying its
/// `ViewError`s as JSON, never a widened result.
#[tauri::command]
pub async fn run_view(
    account_id: String,
    uuid: String,
    state: State<'_, AppState>,
) -> Result<ViewResult, String> {
    crate::db_blocking(&state, move |db| run_saved_view_db(db, &account_id, &uuid)).await
}

fn run_saved_view_db(db: &db::Db, account_id: &str, uuid: &str) -> Result<ViewResult, String> {
    let props = db.props_for_note(account_id, uuid).map_err(|e| format!("run_view: {e}"))?;
    run_props_db(db, account_id, &props)
}

fn run_props_db(db: &db::Db, account_id: &str, props: &[crate::props::Prop]) -> Result<ViewResult, String> {
    let q = parse_view(props).map_err(errors_as_json)?;
    let today = chrono::Local::now().date_naive();
    db.run_view(account_id, &q, &|_| false, today)
        .map_err(|e| format!("run_view: {e}"))
}

fn errors_as_json(errors: Vec<crate::views::query::ViewError>) -> String {
    serde_json::to_string(&errors).unwrap_or_else(|_| "this view cannot be read".into())
}

/// Run the query an editor holds right now, for its live result strip. The
/// property block is read from `body_html` exactly as the derivation reads a
/// saved body. Never writes. The title is not part of the query (a title is
/// never a property, gotcha #11), so it is accepted for symmetry and ignored.
#[tauri::command]
pub async fn preview_view(
    account_id: String,
    #[allow(unused_variables)] title: String,
    body_html: String,
    state: State<'_, AppState>,
) -> Result<ViewResult, String> {
    crate::db_blocking(&state, move |db| {
        let props = db::props_from_body(&account_id, &body_html);
        run_props_db(db, &account_id, &props)
    })
    .await
}

/// Make a starter view: ensure `Notes/__Views__`, then a local note in it.
/// Returns the new note's uuid.
#[tauri::command]
pub async fn create_view(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    refuse_view_without_folders(&state, &account_id)?;
    let kind = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.backend_kind)
        .ok_or_else(|| "Account not found.".to_string())?;
    crate::db_blocking(&state, move |db| create_view_db(db, &account_id, kind)).await
}

const VIEWS_NEED_FOLDERS_MSG: &str = "Views need an account that can create folders.";

fn refuse_view_without_folders(state: &State<'_, AppState>, account_id: &str) -> Result<(), String> {
    refuse_write(state, account_id, backend::Write::Folders)
        .map_err(|_| VIEWS_NEED_FOLDERS_MSG.to_string())
}

fn create_view_db(
    db: &db::Db,
    account_id: &str,
    kind: accounts::BackendKind,
) -> Result<String, String> {
    if !backend::Capabilities::for_backend(kind).writes.allows(backend::Write::Folders) {
        return Err(VIEWS_NEED_FOLDERS_MSG.to_string());
    }
    let label = db
        .ensure_workflow_folder(account_id, VIEWS_FOLDER)
        .map_err(|e| format!("create_view: {e}"))?;
    let saved = save_note_db(
        db,
        account_id,
        kind,
        None,
        NEW_VIEW_TITLE,
        NEW_VIEW_BODY,
        &label,
        None,
        None,
        &crate::provenance::Actor::Human,
        None,
    )?;
    Ok(saved.uuid)
}

/// The account's property keys with counts and common values, for the
/// pickers in the view and note editors.
#[tauri::command]
pub async fn property_vocabulary(
    account_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<(String, u32, Vec<String>)>, String> {
    crate::db_blocking(&state, move |db| {
        db.property_vocabulary(&account_id)
            .map_err(|e| format!("property_vocabulary: {e}"))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::BackendKind;
    use crate::test_support::temp_db;

    fn count(db: &db::Db) -> i64 {
        db.with_conn(|c| c.query_row("SELECT count(*) FROM notes", [], |r| r.get(0)))
            .unwrap()
    }

    #[test]
    fn create_view_refuses_without_folder_writes() {
        let db = temp_db();
        let e = create_view_db(&db, "microsoft:a@b.c", BackendKind::Microsoft).unwrap_err();
        assert_eq!(e, "Views need an account that can create folders.");
        assert_eq!(count(&db), 0);
    }

    #[test]
    fn create_view_makes_a_note_in_views() {
        let db = temp_db();
        let acct = "gmail:a@b.c";
        let uuid = create_view_db(&db, acct, BackendKind::Gmail).unwrap();
        let views = db.list_views(acct).unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].uuid, uuid);
        assert_eq!(views[0].name, "New view");
        assert_eq!(views[0].label, "Notes/__Views__");
        assert_eq!(views[0].lines, vec!["Show: notes", "Folder: Inbox"]);
        assert!(views[0].errors.is_empty());
        // Local-first: the row is dirty for the worker, not pushed.
        let row = db.get(&uuid, acct).unwrap().unwrap();
        assert_eq!(row.sync_state, db::SyncState::Dirty);
        // The new view runs.
        assert!(run_saved_view_db(&db, acct, &uuid).is_ok());
    }

    #[test]
    fn preview_view_does_not_write() {
        let db = temp_db();
        let acct = "gmail:a@b.c";
        let before = count(&db);
        let props = db::props_from_body(acct, "<div>Show: notes</div><div>Folder: Inbox</div>");
        assert!(run_props_db(&db, acct, &props).is_ok());
        let bad = db::props_from_body(acct, "<div>Show: notes</div><div>Pinned: maybe</div>");
        let e = run_props_db(&db, acct, &bad).unwrap_err();
        assert!(e.contains("\"key\":\"pinned\""), "{e}");
        assert_eq!(count(&db), before);
        assert_eq!(db.list_views(acct).unwrap().len(), 0);
    }
}
