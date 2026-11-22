//! Folder commands: create, rename, delete and move, with the name rules
//! they share.
//!
//! All four are local-first: they change the SQLite `folders` table and
//! return, and the sync worker pushes the change. The validation helpers
//! live here too because every caller that names a folder needs the same
//! rules (`validate_folder_segment`, the reserved `__name__` workflow
//! pattern of gotcha #4, and the name-clash check — case on every backend,
//! `/` versus `-` on Gmail).
//!
//! Moved out of `lib.rs` 2026-11-16, unchanged.

use tauri::State;

use crate::backend::gmail::wire as gmail;
use crate::{account_backend_kind, accounts, backend, db, log, refuse_write, AppState};

// ─── Folder management ──────────────────────────────────────────────────────
//
// Folders are Gmail labels under the "Notes/" hierarchy. We always prepend
// "Notes/" to user-supplied names at the command layer so callers don't have
// to think about it (and can't accidentally create a label outside Notes/).
// Cache is invalidated after every mutation so the next list_notes refetches.

pub(crate) fn invalidate_label_cache(state: &State<'_, AppState>, account_id: &str) {
    let mut states = state.account_states.lock().unwrap();
    if let Some(s) = states.get_mut(account_id) {
        s.label_map_cache = None;
    }
}

/// Whether a push to `label` will create that Gmail label — true when this
/// account has a cached label map and the label is not in it. `wire::
/// save_note` creates a missing label rather than filing the note elsewhere,
/// which leaves the cache stale; the folder prune reads that cache and would
/// drop the folder row the push just brought back. No cache means nothing to
/// invalidate.
pub(crate) fn label_cache_lacks(state: &State<'_, AppState>, account_id: &str, label: &str) -> bool {
    let states = state.account_states.lock().unwrap();
    states
        .get(account_id)
        .and_then(|s| s.label_map_cache.as_ref())
        .is_some_and(|(map, _)| gmail::resolve_target(label, map) == gmail::LabelTarget::Create)
}

// Validate a single folder-name segment supplied by the user. Disallow "/"
// (would collide with hierarchy separator), empty/whitespace-only names, and
// excessively long names. Returned String is the trimmed name.
/// Storage name of the content-extraction workflow folder. Lives under Notes/
/// as `Notes/__Extracts__`. The `__name__` form is reserved syntax for
/// Jodd-managed workflow folders — see is_reserved_workflow_pattern. The
/// sidebar strips the underscore markers when displaying (so the user sees
/// "Extracts" with a 💡 icon, not `__Extracts__`).
/// No production code writes here since 2026-09-15 (extract filing); kept for
/// `ensure_workflow_folder`'s tests, including the Microsoft reconciliation
/// regression test.
pub const WORKFLOW_FOLDER_EXTRACTS: &str = "__Extracts__";

/// True for folder name segments matching the reserved `__name__` pattern —
/// Jodd's convention for system-managed workflow folders. The pattern requires
/// at least one character between the markers to avoid trivially rejecting
/// short user-typed sequences like `____` (which is unusual but legal).
pub(crate) fn is_reserved_workflow_pattern(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

/// Strip the `__` markers from a reserved-pattern folder name for display.
/// Returns the input unchanged if it doesn't match the pattern.
pub fn strip_workflow_markers(name: &str) -> &str {
    if is_reserved_workflow_pattern(name) {
        &name[2..name.len() - 2]
    } else {
        name
    }
}

/// App-side folder-name validation: the shared safety core in
/// [`crate::folder_label::validate_label_segment`] plus the one rule that is
/// app-only policy — the `__name__` reservation. The MCP write tools
/// deliberately do NOT inherit that rule (an agent writing into
/// `Notes/__Claude__` is the intended use), which is exactly why the two
/// halves live in different places rather than in one function with a flag.
pub(crate) fn validate_folder_segment(name: &str) -> Result<String, String> {
    let trimmed = crate::folder_label::validate_label_segment(name)?;
    // Reserve the __name__ pattern for Jodd-managed system workflow folders.
    // Anything matching `__*__` is off-limits to user creation — the sidebar
    // strips those markers when displaying so the user can still see a
    // "clean" name (e.g. __Extracts__ → Extracts) but the actual storage path
    // makes the system-managed status unambiguous. Users CAN still create a
    // folder named just "Extracts" (no markers) — only the underscored form
    // is reserved.
    if is_reserved_workflow_pattern(trimmed) {
        return Err(format!(
            "'{}' uses Jodd's reserved __name__ syntax for system folders. \
             Please pick a name without leading and trailing double-underscores.",
            trimmed
        ));
    }
    Ok(trimmed.to_string())
}

/// An existing folder whose path differs from `path` only in letter case,
/// other than `except` (the folder being renamed — a case-only rename of a
/// folder onto itself stays allowed). `create_folder` and `rename_folder`
/// refuse such a name up front: Gmail may treat label names case-insensitively
/// (UNMEASURED), and if it does, pushing `Notes/work` beside `Notes/Work` gets a
/// 409 the folder push can only drop or retry forever — and the notes filed in
/// it lose their sidebar row (PR #143 review). Refusing in the command, before
/// SQLite, removes that trigger whatever Gmail does. Compared with Rust's
/// `to_lowercase`, not SQLite's ASCII-only `lower()`.
pub(crate) fn folder_case_clash<'a>(
    existing: impl IntoIterator<Item = &'a str>,
    path: &str,
    except: Option<&str>,
) -> Option<&'a str> {
    let want = path.to_lowercase();
    existing
        .into_iter()
        .find(|p| *p != path && Some(*p) != except && p.to_lowercase() == want)
}

/// An existing name Gmail would read as the same label as `path` — equal
/// under `wire::label_clash_key`, which treats `/` and `-` alike (measured
/// 2026-11-19) — other than `path` itself and `except` (the folder being
/// renamed or moved). Gmail refuses such a create with a 409 no retry can
/// pass, so on a Gmail account the folder would sit push-blocked forever.
pub(crate) fn gmail_label_clash<'a>(
    existing: impl IntoIterator<Item = &'a str>,
    path: &str,
    except: Option<&str>,
) -> Option<&'a str> {
    let want = gmail::label_clash_key(path);
    existing
        .into_iter()
        .find(|p| *p != path && Some(*p) != except && gmail::label_clash_key(p) == want)
}

/// The name guard `create_folder`, `rename_folder` and `move_folder` run
/// before SQLite: a case-only clash on every backend, and on Gmail also a
/// `/`-versus-`-` clash — checked against the account's folders plus its
/// sidecar label, which Gmail holds though it is not a folder row.
pub(crate) fn refuse_folder_name_clash(state: &State<'_, AppState>, account_id: &str, path: &str, except: Option<&str>) -> Result<(), String> {
    let folders = state.db.list_folders(account_id).map_err(|e| e.to_string())?;
    let paths = || folders.iter().map(|f| f.path.as_str());
    if let Some(other) = folder_case_clash(paths(), path, except) {
        return Err(format!(
            "'{}' differs from the existing folder '{}' only in letter case — choose a different name",
            path, other
        ));
    }
    if account_backend_kind(state, account_id) != Some(accounts::BackendKind::Gmail) {
        return Ok(());
    }
    let meta_label = state
        .accounts
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.id == account_id)
        .map(|a| a.effective_meta_label().to_string());
    match gmail_label_clash(paths().chain(meta_label.as_deref()), path, except) {
        Some(other) => Err(format!(
            "'{}' can't sync to Gmail: Gmail treats '/' and '-' in folder names as the same \
             character, so it clashes with the existing '{}'. Choose a different name.",
            path, other
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod folder_case_clash_tests {
    use super::folder_case_clash;

    #[test]
    fn a_name_differing_only_in_case_clashes_and_others_do_not() {
        let existing = ["Notes", "Notes/Work", "Notes/Work/Q3", "Notes/École"];
        let ex = || existing.iter().copied();
        assert_eq!(folder_case_clash(ex(), "Notes/work", None), Some("Notes/Work"));
        assert_eq!(folder_case_clash(ex(), "Notes/WORK/q3", None), Some("Notes/Work/Q3"));
        // Non-ASCII folds too (SQLite's lower() would miss this).
        assert_eq!(folder_case_clash(ex(), "Notes/école", None), Some("Notes/École"));
        // An exact duplicate is the existing check's job, not this one's.
        assert_eq!(folder_case_clash(ex(), "Notes/Work", None), None);
        assert_eq!(folder_case_clash(ex(), "Notes/Workshop", None), None);
        assert_eq!(folder_case_clash(ex(), "Notes/งาน", None), None);
    }

    #[test]
    fn a_case_only_rename_of_a_folder_onto_itself_is_allowed() {
        let existing = ["Notes/Work"];
        assert_eq!(folder_case_clash(existing.iter().copied(), "Notes/work", Some("Notes/Work")), None);
    }
}

#[cfg(test)]
mod gmail_label_clash_tests {
    use super::gmail_label_clash;

    /// The live failure of 2026-11-19: `AI/VDO` created in Jodd beside the
    /// existing `AI-VDO` was accepted locally, then Gmail refused it with 409
    /// and the folder sat push-blocked behind a raw JSON banner.
    #[test]
    fn a_path_differing_only_by_slash_or_hyphen_clashes() {
        let existing = ["Notes/Personal/Learning/AI", "Notes/Personal/Learning/AI-VDO", "Notes/_test_/probe-a"];
        let ex = || existing.iter().copied();
        assert_eq!(
            gmail_label_clash(ex(), "Notes/Personal/Learning/AI/VDO", None),
            Some("Notes/Personal/Learning/AI-VDO")
        );
        assert_eq!(gmail_label_clash(ex(), "Notes/_test_/probe/a", None), Some("Notes/_test_/probe-a"));
        // The control of the live probe: Gmail created this one.
        assert_eq!(gmail_label_clash(ex(), "Notes/_test_/probe/b", None), None);
        // An exact duplicate is the existing check's job, not this one's.
        assert_eq!(gmail_label_clash(ex(), "Notes/Personal/Learning/AI-VDO", None), None);
    }

    /// Jodd's sidecar label (`Notes-Meta` by default) is not a folder row,
    /// but Gmail holds it all the same: a root folder `Meta` is its twin.
    #[test]
    fn the_sidecar_label_counts_when_passed_among_the_existing_names() {
        let existing = ["Notes", "Notes-Meta"];
        assert_eq!(gmail_label_clash(existing.iter().copied(), "Notes/Meta", None), Some("Notes-Meta"));
    }

    #[test]
    fn renaming_a_folder_onto_its_own_twin_is_left_to_gmail() {
        let existing = ["Notes/a-b"];
        assert_eq!(gmail_label_clash(existing.iter().copied(), "Notes/a/b", Some("Notes/a-b")), None);
    }
}

/// Every command that gives a folder a new path runs the name-clash guard
/// before the row is written: the guard does nothing for a command that never
/// calls it, and none of the three can run without a Tauri `State`.
#[cfg(test)]
mod folder_name_clash_wiring_tests {
    #[test]
    fn create_rename_and_move_refuse_a_clash_before_sqlite() {
        let src = include_str!("folder_commands.rs");
        for (sig, write) in [
            ("async fn create_folder(", "insert_folder_local_new("),
            ("async fn rename_folder(", "rename_subtree"),
            ("async fn move_folder(", "rename_subtree("),
        ] {
            let body = crate::test_support::extract_fn_body(src, sig);
            let guard = body.find("refuse_folder_name_clash(").unwrap_or_else(|| panic!("{sig} runs the guard"));
            assert!(guard < body.find(write).unwrap(), "{sig}: guard before {write}");
        }
    }
}

#[cfg(test)]
mod validate_folder_segment_tests {
    use super::*;

    #[test]
    fn reserved_pattern_matches() {
        assert!(is_reserved_workflow_pattern("__Extracts__"));
        assert!(is_reserved_workflow_pattern("__Foo__"));
        assert!(is_reserved_workflow_pattern("__a__"));
    }

    #[test]
    fn reserved_pattern_does_not_match_edge_cases() {
        assert!(!is_reserved_workflow_pattern("Extracts")); // no markers
        assert!(!is_reserved_workflow_pattern("__foo")); // suffix missing
        assert!(!is_reserved_workflow_pattern("foo__")); // prefix missing
        assert!(!is_reserved_workflow_pattern("____")); // empty middle
        assert!(!is_reserved_workflow_pattern("__")); // too short
        assert!(!is_reserved_workflow_pattern("")); // empty
    }

    #[test]
    fn link_preview_first_line_is_the_bodys_first_line() {
        // The cached body never carries the title line (Gmail/LocalFs strip
        // it on decode, iCloud cuts it), so the preview is the body's FIRST
        // non-empty line — skipping one would show the second.
        let f = crate::link_preview_first_line;
        assert_eq!(f("<div>  the second line  </div><div>third</div>"), "the second line");
        // A one-line body previews that line, not nothing.
        assert_eq!(f("<div>only</div>"), "only");
        // Blank blocks before the first line are skipped.
        assert_eq!(f("<div><br></div><div>body</div>"), "body");
        assert_eq!(f(""), "");
        assert_eq!(f("<div><br></div>"), "");
        // 140 chars, on a char boundary (Thai is multi-byte).
        let long = "ก".repeat(200);
        let got = f(&format!("<div>{long}</div>"));
        assert_eq!(got.chars().count(), 140);
        assert!(got.chars().all(|c| c == 'ก'));
        // The cut can land after a space: trailing whitespace is trimmed.
        let spaced = format!("{} {}", "a".repeat(139), "b".repeat(20));
        assert_eq!(f(&format!("<div>{spaced}</div>")), "a".repeat(139));
    }

    #[test]
    fn strip_markers_when_pattern_matches() {
        assert_eq!(strip_workflow_markers("__Extracts__"), "Extracts");
        assert_eq!(strip_workflow_markers("__Foo__"), "Foo");
    }

    #[test]
    fn strip_markers_passes_through_when_not_pattern() {
        assert_eq!(strip_workflow_markers("Extracts"), "Extracts");
        assert_eq!(strip_workflow_markers("__foo"), "__foo");
        assert_eq!(strip_workflow_markers("normal-folder"), "normal-folder");
    }

    #[test]
    fn validate_rejects_reserved_pattern() {
        let err = validate_folder_segment("__Extracts__").unwrap_err();
        assert!(err.contains("__name__"), "actual: {err}");
    }

    #[test]
    fn validate_accepts_extracts_without_markers() {
        // Plain `Extracts` (no underscores) is fair game for users — only
        // the underscored form is reserved.
        assert_eq!(validate_folder_segment("Extracts").unwrap(), "Extracts");
    }

    #[test]
    fn validate_rejects_dot_and_dotdot() {
        // Prevent path-traversal: "." and ".." must be rejected so a
        // LocalFS folder path can never escape the notes root.
        let err_dot = validate_folder_segment(".").unwrap_err();
        assert!(err_dot.contains("'.'") || err_dot.contains("cannot be"), "actual: {err_dot}");
        let err_dotdot = validate_folder_segment("..").unwrap_err();
        assert!(
            err_dotdot.contains("'..'") || err_dotdot.contains("cannot be"),
            "actual: {err_dotdot}"
        );
    }
}

// ── Folder ops: local-first ─────────────────────────────────────────────
//
// All four mutate the SQLite `folders` table immediately and return. The
// background sync worker pushes the changes to Gmail on its next tick.
// Validation rules (name format, no-empty, no-root, etc.) run in the
// command path so the user gets an immediate error for invalid inputs.

#[tauri::command]
pub async fn create_folder(
    account_id: String,
    name: String,
    parent_path: Option<String>,
    state: State<'_, AppState>,
) -> Result<gmail::FolderInfo, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!(
        "create_folder: account={} name={:?} parent={:?}",
        account_id, name, parent_path
    );
    let segment = validate_folder_segment(&name)?;
    let full = match parent_path.as_deref() {
        Some(p) if p == "Notes" || p.starts_with("Notes/") => format!("{}/{}", p, segment),
        None => format!("Notes/{}", segment),
        Some(other) => return Err(format!("Invalid parent path: {}", other)),
    };
    // Reject duplicates against what's in the cache (which mirrors Gmail
    // + any in-flight local creates). Worker re-checks against Gmail.
    if let Ok(Some(_)) = state.db.get_folder(&account_id, &full) {
        return Err(format!("Folder '{}' already exists", full));
    }
    refuse_folder_name_clash(&state, &account_id, &full, None)?;
    let folder = db::CachedFolder {
        account_id: account_id.clone(),
        path: full.clone(),
        label_id: None,
        sync_state: db::FolderSyncState::DirtyNew,
        last_local_modified_at: db::now_ms(),
        last_synced_at: None,
        // User-created via the sidebar → kind='user'. Workflow folders
        // are minted by ensure_workflow_folder (Task 3) with kind=
        // 'system_workflow', not by this command.
        kind: "user".to_string(),
    };
    state.db.insert_folder_local_new(&folder).map_err(|e| e.to_string())?;
    log!("create_folder (local-first): path='{}'", full);
    // Return shape matches the old API so existing frontend works. id is
    // empty until the worker assigns one.
    Ok(gmail::FolderInfo { id: String::new(), name: full })
}

#[tauri::command]
pub async fn rename_folder(
    account_id: String,
    path: String,
    new_name: String,
    state: State<'_, AppState>,
) -> Result<gmail::FolderInfo, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!(
        "rename_folder: account={} path={:?} new_name={:?}",
        account_id, path, new_name
    );
    let new_segment = validate_folder_segment(&new_name)?;
    if path == "Notes" {
        return Err("Cannot rename the root 'Notes' folder".to_string());
    }
    if !path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", path));
    }
    let parent_path: String = path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
    let new_path = if parent_path.is_empty() {
        new_segment.clone()
    } else {
        format!("{}/{}", parent_path, new_segment)
    };
    if new_path == path {
        return Ok(gmail::FolderInfo { id: String::new(), name: new_path });
    }
    // Reject if a sibling already has this name — unless that row is only a
    // clean mirror of THIS folder (same label id): what a pull writes of the
    // remote's name while this folder's rename is refused. Renaming back to
    // it is how the user settles that, so it replaces the mirror instead.
    let over_mirror = state.db.is_mirror_of(&account_id, &new_path, &path).map_err(|e| e.to_string())?;
    if !over_mirror {
        if let Ok(Some(_)) = state.db.get_folder(&account_id, &new_path) {
            return Err(format!("'{}' already exists", new_path));
        }
    }
    refuse_folder_name_clash(&state, &account_id, &new_path, Some(&path))?;
    // Rename the folder AND cascade to descendants AND notes' label field
    // in one transaction. Each touched folder transitions to dirty_renamed
    // so the worker pushes each rename to Gmail individually.
    let touched = if over_mirror {
        state.db.rename_subtree_over_mirror(&account_id, &path, &new_path)
    } else {
        state.db.rename_subtree(&account_id, &path, &new_path)
    }
    .map_err(|e| e.to_string())?;
    log!(
        "rename_folder (local-first): '{}' → '{}', {} folder row(s) cascaded",
        path, new_path, touched
    );
    Ok(gmail::FolderInfo { id: String::new(), name: new_path })
}

#[tauri::command]
pub async fn delete_folder(
    account_id: String,
    path: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    log!("delete_folder: account={} path={:?}", account_id, path);
    if path == "Notes" {
        return Err("Cannot delete the root 'Notes' folder".to_string());
    }
    if !path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", path));
    }
    if let Some(refusal) = state.db.folder_delete_refusal(&account_id, &path).map_err(|e| e.to_string())? {
        return Err(refusal);
    }
    state.db.mark_folder_deleted(&account_id, &path).map_err(|e| e.to_string())?;
    log!("delete_folder (local-first): marked deleted_pending for '{}'", path);
    Ok(())
}

#[tauri::command]
pub async fn move_folder(
    account_id: String,
    from_path: String,
    to_parent_path: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    // Refuse a write this area can't accept yet, BEFORE SQLite, so no
    // unpushable row is created. See `refuse_write`.
    refuse_write(&state, &account_id, backend::Write::Folders)?;
    // **Reparenting is a different write than rename, and `Writes.folders`
    // does not cover it.** create_folder/rename_folder/delete_folder are all
    // measured live on iCloud (2026-08-24); a rename NEVER changes
    // ParentFolder (it always keeps the same parent segment — see this
    // command's own `new_path` construction below). This command is the one
    // path that changes a folder's PARENT, which would mean writing
    // `ParentFolder` on a record that already exists — nothing here has ever
    // sent that, live or in a unit test. Refuse it explicitly rather than let
    // `Writes.folders = true` imply a shape that was never checked, the same
    // discipline gotcha #12/#18 apply per-backend with no wildcard arm.
    if account_backend_kind(&state, &account_id) == Some(accounts::BackendKind::ICloud) {
        return Err(
            "Moving a folder to a different parent isn't available on iCloud yet — do it \
             in Apple Notes and it will sync back here."
                .to_string(),
        );
    }
    log!(
        "move_folder: account={} from={:?} to_parent={:?}",
        account_id, from_path, to_parent_path
    );
    if from_path == "Notes" {
        return Err("Cannot move the root 'Notes' folder".to_string());
    }
    if !from_path.starts_with("Notes/") {
        return Err(format!("Not a Notes-tree folder: {}", from_path));
    }
    if to_parent_path != "Notes" && !to_parent_path.starts_with("Notes/") {
        return Err(format!("Destination must be under Notes/: {}", to_parent_path));
    }
    let leaf = from_path.rsplit('/').next()
        .ok_or_else(|| "Invalid from_path".to_string())?;
    let new_path = format!("{}/{}", to_parent_path, leaf);
    if new_path == from_path {
        return Ok(new_path);
    }
    if to_parent_path == from_path || to_parent_path.starts_with(&format!("{}/", from_path)) {
        return Err("Cannot move a folder into itself or its sub-folder".to_string());
    }
    // Reject collision at destination.
    if let Ok(Some(_)) = state.db.get_folder(&account_id, &new_path) {
        return Err(format!("'{}' already exists at the destination", new_path));
    }
    refuse_folder_name_clash(&state, &account_id, &new_path, Some(&from_path))?;
    let touched = state.db.rename_subtree(&account_id, &from_path, &new_path)
        .map_err(|e| e.to_string())?;
    log!(
        "move_folder (local-first): '{}' → '{}', {} folder row(s) cascaded",
        from_path, new_path, touched
    );
    Ok(new_path)
}

/// Both commands that send a folder delete run the same guard first: a clean
/// mirror of a pending folder must never be deleted (it shares that folder's
/// label id), and "Try again" on a refused delete must not delete a folder a
/// note was filed into since — LocalFs removes the directory with it. Neither
/// command can run without a Tauri `State`, so the wiring is pinned here.
#[cfg(test)]
mod folder_delete_guard_wiring_tests {
    #[test]
    fn delete_and_retry_check_the_delete_guard_before_queueing() {
        let delete = crate::test_support::extract_fn_body(include_str!("folder_commands.rs"), "async fn delete_folder(");
        let guard = delete.find("folder_delete_refusal(").expect("delete_folder runs the guard");
        assert!(guard < delete.find("mark_folder_deleted(").unwrap(), "before the row is queued");

        let retry = crate::test_support::extract_fn_body(include_str!("account_commands.rs"), "async fn retry_blocked_folder(");
        let guard = retry.find("folder_delete_refusal(").expect("Try again on a delete runs the guard");
        assert!(guard < retry.find("clear_folder_push_block(").unwrap(), "before the row is re-armed");
    }
}
