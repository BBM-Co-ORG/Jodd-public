//! The execution engine (spec §3.4): every fix SQLite-first, the sync worker
//! pushes. A removed note goes to `deleted_pending` → the backend's trash,
//! never a hard delete.

use std::path::Path;

use super::{Action, Kind, NoteRef, Payload};
use crate::accounts::Account;
use crate::backend::{Capabilities, Write};
use crate::db::{CachedNote, Db};
use crate::llm::markdown::escape_html;

/// The folder created and hidden when a secret must move and the account
/// hides nothing yet.
pub const PRIVATE_FOLDER: &str = "Notes/Private";

#[derive(Debug, PartialEq)]
pub enum ApplyError {
    /// A note changed or vanished since the proposal; the proposal is now
    /// `stale` and a rescan proposes afresh.
    Stale(String),
    AlreadyDecided,
    Refused(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Stale(t) => write!(f, "\"{t}\" changed since this was proposed. Find problems again for a fresh proposal."),
            ApplyError::AlreadyDecided => write!(f, "This proposal was already decided."),
            ApplyError::Refused(m) => write!(f, "{m}"),
        }
    }
}

fn require(account: &Account, w: Write, what: &str) -> Result<(), ApplyError> {
    if Capabilities::for_backend(account.backend_kind).writes.allows(w) {
        Ok(())
    } else {
        Err(ApplyError::Refused(format!("This account can't {what} from Jodd.")))
    }
}

/// The user may switch a duplicate between keep and append, and choose
/// which note survives — but only among the proposal's own notes, and only
/// to an action of the proposal's kind.
fn validate_choice(kind: Kind, payload: &Payload, chosen: &Action) -> Result<(), ApplyError> {
    let member = |u: &str| payload.notes.iter().any(|n| n.uuid == u);
    let ok = match (kind, chosen) {
        (Kind::Duplicate, Action::Keep { keep }) => member(keep),
        (Kind::Duplicate, Action::Append { into }) => member(into),
        (Kind::Misfiled, Action::Move { .. }) => *chosen == payload.action,
        (Kind::Secret, Action::Hide) => true,
        _ => false,
    };
    if ok { Ok(()) } else { Err(ApplyError::Refused("That action does not fit this proposal.".into())) }
}

/// Re-read every note; any missing or edited since the proposal → `Stale`.
fn fresh(db: &Db, account_id: &str, notes: &[NoteRef]) -> Result<Vec<CachedNote>, ApplyError> {
    notes
        .iter()
        .map(|r| match db.note_by_uuid(account_id, &r.uuid) {
            Ok(Some(n)) if n.local_version == r.local_version && (r.hash.is_empty() || super::content_hash(&n) == r.hash) => Ok(n),
            _ => Err(ApplyError::Stale(r.title.clone())),
        })
        .collect()
}

/// `[[Old]]` → `[[Kept]]` in every note that links to a note being removed,
/// so a merge does not leave dangling links.
fn rewrite_links(db: &Db, account_id: &str, removed: &CachedNote, kept: &CachedNote, skip: &[String]) -> Result<(), ApplyError> {
    let from = format!("[[{}]]", escape_html(&removed.title));
    let to = format!("[[{}]]", escape_html(&kept.title));
    let linking = db.backlinks(account_id, &removed.uuid).map_err(|e| ApplyError::Refused(e.to_string()))?;
    for n in linking.into_iter().filter(|n| !skip.contains(&n.uuid)) {
        if n.body_html.contains(&from) {
            crate::agent_memory::edit_page(db, account_id, &n.uuid, |cur| cur.body_html.replace(&from, &to)).map_err(ApplyError::Refused)?;
        }
    }
    Ok(())
}

fn merge_body(into: &CachedNote, others: &[&CachedNote]) -> String {
    let mut body = into.body_html.clone();
    for o in others {
        body.push_str(&format!("<hr><h2>Merged from: {}</h2>", escape_html(&o.title)));
        body.push_str(&o.body_html);
    }
    body
}

/// Apply proposal `id`, optionally with the user's changed action.
pub fn apply(db: &Db, account: &Account, id: i64, chosen: Option<Action>, scope_path: &Path) -> Result<(), ApplyError> {
    let (account_id, kind, payload, status) = db
        .curate_get(id)
        .map_err(|e| ApplyError::Refused(e.to_string()))?
        .ok_or_else(|| ApplyError::Refused("No such proposal.".into()))?;
    if account_id != account.id {
        return Err(ApplyError::Refused("That proposal belongs to another account.".into()));
    }
    if status != "pending" {
        return Err(ApplyError::AlreadyDecided);
    }
    let kind = Kind::parse(&kind).ok_or_else(|| ApplyError::Refused("Unknown proposal kind.".into()))?;
    let payload: Payload = serde_json::from_str(&payload).map_err(|e| ApplyError::Refused(e.to_string()))?;
    let action = chosen.unwrap_or_else(|| payload.action.clone());
    validate_choice(kind, &payload, &action)?;

    let notes = match fresh(db, &account.id, &payload.notes) {
        Ok(n) => n,
        Err(e) => {
            let _ = db.curate_decide(id, "pending", "stale", Some(&e.to_string()));
            return Err(e);
        }
    };
    let result = run(db, account, &action, &notes, scope_path);
    match &result {
        Ok(()) => {
            if !db.curate_decide(id, "pending", "applied", None).map_err(|e| ApplyError::Refused(e.to_string()))? {
                return Err(ApplyError::AlreadyDecided);
            }
        }
        Err(e) => {
            let _ = db.curate_decide(id, "pending", "failed", Some(&e.to_string()));
        }
    }
    result
}

fn run(db: &Db, account: &Account, action: &Action, notes: &[CachedNote], scope_path: &Path) -> Result<(), ApplyError> {
    let acct = account.id.as_str();
    let db_err = |e: rusqlite::Error| ApplyError::Refused(e.to_string());
    match action {
        Action::Keep { keep } | Action::Append { into: keep } => {
            require(account, Write::Relocate, "move notes to the trash")?;
            // Review finding 1: on a backend without a trash (Microsoft) the
            // worker's delete is permanent — never let a merge do that.
            if !Capabilities::for_backend(account.backend_kind).has_trash {
                return Err(ApplyError::Refused("This account deletes notes permanently, so Organize won't remove one. Merge them by hand.".into()));
            }
            let kept = notes.iter().find(|n| &n.uuid == keep).ok_or_else(|| ApplyError::Refused("The note to keep is missing.".into()))?;
            let others: Vec<&CachedNote> = notes.iter().filter(|n| &n.uuid != keep).collect();
            if matches!(action, Action::Append { .. }) {
                require(account, Write::Notes, "edit notes")?;
                // Review finding 4: an inline image is stored per note; its
                // `cid:` reference would break in the merged note.
                if others.iter().any(|o| o.body_html.contains("cid:")) {
                    return Err(ApplyError::Refused("A note to append has an image, which would not survive the merge. Keep one instead, or merge by hand.".into()));
                }
                // Review finding 11: a note the backend refuses to accept would
                // take the merged text nowhere while the others are trashed.
                if kept.push_blocked_reason.is_some() {
                    return Err(ApplyError::Refused(format!("\"{}\" can't be synced right now, so nothing will be appended to it.", kept.title)));
                }
                let body = merge_body(kept, &others);
                // Compare-and-swap on the version `fresh` just checked: an edit
                // landing in between makes this fail rather than be overwritten.
                if !db.apply_local_edit_versioned(&kept.uuid, acct, &kept.title, &body, &kept.label, kept.local_version).map_err(db_err)? {
                    return Err(ApplyError::Stale(kept.title.clone()));
                }
            }
            let group: Vec<String> = notes.iter().map(|n| n.uuid.clone()).collect();
            if Capabilities::for_backend(account.backend_kind).writes.notes {
                for o in &others {
                    rewrite_links(db, acct, o, kept, &group)?;
                }
            }
            let trash: Vec<String> = others.iter().map(|n| n.uuid.clone()).collect();
            db.delete_notes_batch(acct, &trash).map_err(db_err)?;
            Ok(())
        }
        Action::Move { to } => {
            require(account, Write::Relocate, "move notes")?;
            if db.get_folder(acct, to).map_err(db_err)?.is_none() {
                return Err(ApplyError::Refused(format!("The folder {to} no longer exists.")));
            }
            db.move_notes_batch(acct, &[notes[0].uuid.clone()], to).map_err(db_err)?;
            Ok(())
        }
        Action::Hide => {
            require(account, Write::Relocate, "move notes")?;
            let scope = match crate::mcp_scope::load_from(scope_path) {
                Ok(s) => s,
                Err(crate::mcp_scope::ScopeError::NotConfigured) => Default::default(),
                Err(crate::mcp_scope::ScopeError::Unparseable(e)) => return Err(ApplyError::Refused(format!("mcp_write_scope.json can't be read ({e}); fix it first."))),
            };
            // Only a hidden folder that exists here — a hand-written path with
            // no folder row would move the note to a label nobody has.
            let existing_hidden = scope.hidden_folders(acct).iter().find(|h| db.get_folder(acct, h).ok().flatten().is_some()).cloned();
            let target = match existing_hidden {
                Some(h) => h,
                None => {
                    require(account, Write::Folders, "create folders")?;
                    if db.get_folder(acct, PRIVATE_FOLDER).map_err(db_err)?.is_none() {
                        db.create_folder_local_new(acct, PRIVATE_FOLDER).map_err(db_err)?;
                    }
                    crate::mcp_scope::edit_at(scope_path, |v| crate::mcp_scope::set_hidden(v, acct, PRIVATE_FOLDER, true)).map_err(ApplyError::Refused)?;
                    PRIVATE_FOLDER.to_string()
                }
            };
            db.move_notes_batch(acct, &[notes[0].uuid.clone()], &target).map_err(db_err)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curate::{candidates::tests::note, list, propose};
    use crate::db::SyncState;
    use crate::test_support::temp_db;

    const A: &str = "gmail:a@x.com";

    fn account(backend: &str) -> Account {
        serde_json::from_str(&format!(r#"{{"id":"{A}","email":"a@x.com","added_at":"2026-01-01T00:00:00Z","backend_kind":"{backend}"}}"#)).unwrap()
    }
    fn put(db: &Db, uuid: &str, title: &str, label: &str, body: &str) -> CachedNote {
        let mut n = note(uuid, title, label, body, 1);
        n.account_id = A.into();
        db.insert_local_new(&n).unwrap();
        db.note_by_uuid(A, uuid).unwrap().unwrap()
    }
    fn proposal(db: &Db, kind: Kind, notes: &[&CachedNote], action: Action) -> i64 {
        let p = Payload { notes: notes.iter().map(|n| NoteRef::of(n)).collect(), action, reason: "r".into(), evidence: vec![] };
        propose(db, A, kind, &p, "test").unwrap();
        list(db, A, &["pending"]).unwrap().last().unwrap().id
    }
    fn scope_file() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        (d, p)
    }
    fn state(db: &Db, uuid: &str) -> SyncState {
        db.get(uuid, A).unwrap().unwrap().sync_state
    }

    #[test]
    fn keep_trashes_the_others_and_rewrites_links_to_them() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let new = put(&db, "NEW", "Course notes v2", "Notes", "<div>full</div>");
        let old = put(&db, "OLD", "Course notes", "Notes", "<div>partial</div>");
        let linker = put(&db, "L", "Index", "Notes/Work", "<div>see [[Course notes]]</div>");
        // Re-derive edges for the linker so backlinks() sees it.
        db.apply_local_edit_versioned("L", A, &linker.title, &linker.body_html, &linker.label, linker.local_version).unwrap();
        let id = proposal(&db, Kind::Duplicate, &[&new, &old], Action::Keep { keep: "NEW".into() });
        apply(&db, &account("gmail"), id, None, &sp).unwrap();
        assert_eq!(state(&db, "OLD"), SyncState::DeletedPending);
        assert_ne!(state(&db, "NEW"), SyncState::DeletedPending);
        assert!(db.get("L", A).unwrap().unwrap().body_html.contains("[[Course notes v2]]"));
        assert_eq!(list(&db, A, &["applied"]).unwrap().len(), 1);
    }

    #[test]
    fn append_merges_into_one_then_trashes_and_the_user_may_switch_which() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let a = put(&db, "A", "Plan", "Notes", "<div>alpha</div>");
        let b = put(&db, "B", "Plan 2", "Notes", "<div>beta</div>");
        let id = proposal(&db, Kind::Duplicate, &[&a, &b], Action::Keep { keep: "A".into() });
        apply(&db, &account("gmail"), id, Some(Action::Append { into: "B".into() }), &sp).unwrap();
        let merged = db.get("B", A).unwrap().unwrap().body_html;
        assert!(merged.starts_with("<div>beta</div>") && merged.contains("Merged from: Plan") && merged.contains("alpha"), "{merged}");
        assert_eq!(state(&db, "A"), SyncState::DeletedPending);
    }

    #[test]
    fn an_edit_after_the_proposal_makes_it_stale_and_nothing_is_trashed() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let a = put(&db, "A", "Plan", "Notes", "<div>alpha</div>");
        let b = put(&db, "B", "Plan 2", "Notes", "<div>beta</div>");
        let id = proposal(&db, Kind::Duplicate, &[&a, &b], Action::Keep { keep: "A".into() });
        db.apply_local_edit_versioned("B", A, "Plan 2", "<div>beta + my new edit</div>", "Notes", b.local_version).unwrap();
        assert!(matches!(apply(&db, &account("gmail"), id, None, &sp), Err(ApplyError::Stale(_))));
        assert_ne!(state(&db, "B"), SyncState::DeletedPending);
        assert_eq!(list(&db, A, &["stale"]).unwrap().len(), 1);
        assert_eq!(apply(&db, &account("gmail"), id, None, &sp), Err(ApplyError::AlreadyDecided));
    }

    #[test]
    fn a_choice_outside_the_proposal_is_refused() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let a = put(&db, "A", "Plan", "Notes", "<div>alpha</div>");
        let b = put(&db, "B", "Plan 2", "Notes", "<div>beta</div>");
        put(&db, "C", "Unrelated", "Notes", "<div>c</div>");
        let id = proposal(&db, Kind::Duplicate, &[&a, &b], Action::Keep { keep: "A".into() });
        assert!(matches!(apply(&db, &account("gmail"), id, Some(Action::Keep { keep: "C".into() }), &sp), Err(ApplyError::Refused(_))));
        assert!(matches!(apply(&db, &account("gmail"), id, Some(Action::Hide), &sp), Err(ApplyError::Refused(_))));
        assert_eq!(state(&db, "C"), SyncState::Clean);
    }

    #[test]
    fn a_remote_edit_without_a_version_bump_is_still_stale() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let a = put(&db, "A", "Plan", "Notes", "<div>alpha</div>");
        let b = put(&db, "B", "Plan 2", "Notes", "<div>beta</div>");
        let id = proposal(&db, Kind::Duplicate, &[&a, &b], Action::Keep { keep: "A".into() });
        // What a pull does: new body, same local_version.
        let mut pulled = db.get("B", A).unwrap().unwrap();
        pulled.body_html = "<div>beta, edited on the phone</div>".into();
        db.upsert_from_remote(&pulled, crate::db::RemotePin::LocalWins).unwrap();
        assert_eq!(db.get("B", A).unwrap().unwrap().local_version, b.local_version, "fixture: the pull kept the version");
        assert!(matches!(apply(&db, &account("gmail"), id, None, &sp), Err(ApplyError::Stale(_))));
        assert_ne!(state(&db, "B"), SyncState::DeletedPending);
    }

    #[test]
    fn no_merge_on_a_backend_that_deletes_permanently_or_with_images() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let a = put(&db, "A", "Plan", "Notes", "<div>alpha</div>");
        let b = put(&db, "B", "Plan 2", "Notes", r#"<div>beta <img src="cid:ii_1"></div>"#);
        let id = proposal(&db, Kind::Duplicate, &[&a, &b], Action::Keep { keep: "A".into() });
        assert!(matches!(apply(&db, &account("microsoft"), id, None, &sp), Err(ApplyError::Refused(m)) if m.contains("permanently")));
        let id2 = proposal(&db, Kind::Duplicate, &[&b, &a], Action::Append { into: "A".into() });
        let _ = id2;
        let id2 = list(&db, A, &["pending"]).unwrap().last().unwrap().id;
        assert!(matches!(apply(&db, &account("gmail"), id2, Some(Action::Append { into: "A".into() }), &sp), Err(ApplyError::Refused(m)) if m.contains("image")));
        assert_ne!(state(&db, "B"), SyncState::DeletedPending);
    }

    #[test]
    fn move_files_the_note() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        db.create_folder_local_new(A, "Notes/Work").unwrap();
        let n = put(&db, "N", "Note", "Notes", "<div>x</div>");
        let id = proposal(&db, Kind::Misfiled, &[&n], Action::Move { to: "Notes/Work".into() });
        apply(&db, &account("gmail"), id, None, &sp).unwrap();
        assert_eq!(db.get("N", A).unwrap().unwrap().label, "Notes/Work");
    }

    #[test]
    fn hide_creates_and_hides_a_private_folder_when_none_is_hidden() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        let n = put(&db, "S", "Broker login", "Notes/Projects", "<div>secret</div>");
        let id = proposal(&db, Kind::Secret, &[&n], Action::Hide);
        apply(&db, &account("gmail"), id, None, &sp).unwrap();
        assert_eq!(db.get("S", A).unwrap().unwrap().label, PRIVATE_FOLDER);
        assert!(db.get_folder(A, PRIVATE_FOLDER).unwrap().is_some());
        assert!(crate::mcp_scope::load_from(&sp).unwrap().is_hidden(A, PRIVATE_FOLDER));
    }

    #[test]
    fn hide_uses_an_existing_hidden_folder() {
        let (db, (_d, sp)) = (temp_db(), scope_file());
        crate::mcp_scope::edit_at(&sp, |v| crate::mcp_scope::set_hidden(v, A, "Notes/Personal", true)).unwrap();
        db.create_folder_local_new(A, "Notes/Personal").unwrap();
        let n = put(&db, "S", "Broker login", "Notes/Projects", "<div>secret</div>");
        let id = proposal(&db, Kind::Secret, &[&n], Action::Hide);
        apply(&db, &account("gmail"), id, None, &sp).unwrap();
        assert_eq!(db.get("S", A).unwrap().unwrap().label, "Notes/Personal");
        assert!(db.get_folder(A, PRIVATE_FOLDER).unwrap().is_none());
    }
}
