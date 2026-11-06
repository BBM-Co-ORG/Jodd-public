//! Planning a restore: what each selected note will do in the target account,
//! and what it loses there. Reads only — `apply` does the writes, from this
//! plan, so the preview the owner confirms is exactly what happens.
//!
//! The anti-goals are duplicates and silent loss, so a note is skipped only
//! when the target demonstrably holds it already (spec §6.2), and every other
//! note is either written or listed as refused with a reason.

use super::transform::{append_keywords, map_folder, strip_attachments, Loss, Placement};
use crate::accounts::{Account, BackendKind};
use crate::backend::{Capabilities, TagModel};
use crate::backup::body::{content_sha256, decode_body};
use crate::backup::bundle::{BundleNote, OpenBundle};
use crate::backup::{BodyFormat, NoteDoc};
use crate::db::{CachedNote, Db, FolderSyncState, SyncState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Error code for a target that has never completed a full listing: until it
/// has, the originals a restore must dedupe against may not be in the cache.
pub const TARGET_NOT_SYNCED_YET: &str = "TARGET_NOT_SYNCED_YET";
pub(super) const CHECKSUM_MISMATCH: &str = "This note's content does not match its checksum.";
pub(super) const CID_LEFT: &str = "This note references an attachment Jodd could not remove for this account.";
pub(super) const NOT_IN_BACKUP: &str = "This note is not in the backup.";
const FOLDER_BEING_DELETED: &str =
    "This note's folder is being deleted in this account; restore it under a new folder instead.";

/// What to do with a note that already exists in the target (spec §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExistingChoice {
    KeepBoth,
    Replace,
    Skip,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreRequest {
    pub destination_id: String,
    pub slug: String,
    pub stamp: String,
    pub source_uuids: Vec<String>,
    pub target_account: String,
    pub placement: Placement,
    pub existing: ExistingChoice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Action {
    Create,
    /// A previous restore already put this version in the target.
    SkipAlreadyRestored,
    /// The target holds a note with this title and body (another device's restore).
    SkipContentMatch,
    /// Create a copy titled `<title> (restored YYYY-MM-DD)` beside `existing_uuid`.
    KeepBoth { existing_uuid: String },
    /// Overwrite `existing_uuid` with the backed-up content, as a local edit.
    Replace { existing_uuid: String },
    /// The note exists and the owner chose Skip.
    SkipExisting,
    Refuse { reason: String },
}

impl Action {
    pub(super) fn writes(&self) -> bool {
        matches!(self, Action::Create | Action::KeepBoth { .. } | Action::Replace { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedNote {
    /// `jodd.uuid`, or the concept's path in a bundle note without `jodd:`.
    pub source_uuid: String,
    /// The title the note will have (a Keep-both copy carries its suffix).
    pub title: String,
    /// Where the note lands, or for a skip, where the target already holds it.
    pub target_folder: String,
    pub action: Action,
    pub losses: Vec<Loss>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    pub notes: Vec<PlannedNote>,
    /// Folders `apply` creates first (their ancestors come with them).
    pub folders_to_create: Vec<String>,
}

/// What the target backend can hold, from its kind alone.
pub(super) struct TargetTraits {
    pub kind: BackendKind,
    pub caps: Capabilities,
    /// Gmail and LocalFs keep attachment rows; every other backend drops them.
    pub holds_attachments: bool,
    /// iCloud: `#tag` text is not a tag, so tags become a `Keywords:` line.
    pub inline_tags: bool,
}

impl TargetTraits {
    pub fn of(target: &Account) -> TargetTraits {
        let kind = target.backend_kind;
        TargetTraits {
            kind,
            caps: Capabilities::for_backend(kind),
            holds_attachments: matches!(kind, BackendKind::Gmail | BackendKind::LocalFs),
            inline_tags: TagModel::for_backend(kind) == TagModel::InlineObjects,
        }
    }
}

/// A bundle note's identity in requests, plans and `restore_map`.
pub(super) fn source_id(n: &BundleNote) -> String {
    match &n.doc.jodd {
        Some(j) => j.uuid.clone(),
        None => n.path.clone(),
    }
}

/// The note's HTML as it was backed up. A note without `jodd:` is foreign OKF,
/// whose body is Markdown.
pub(super) fn decoded_html(doc: &NoteDoc) -> String {
    let format = doc.jodd.as_ref().map_or(BodyFormat::Markdown, |j| j.body);
    decode_body(format, &doc.body)
}

/// The checksum `restore_map` records: the backup's own, or for a foreign
/// note (which carries none) the hash of its decoded body.
pub(super) fn source_sha(doc: &NoteDoc, html: &str) -> String {
    doc.jodd.as_ref().map_or_else(|| content_sha256(html), |j| j.content_sha256.clone())
}

/// Whether the decoded body is the one the backup hashed. A foreign note has
/// no checksum to fail.
pub(super) fn checksum_ok(doc: &NoteDoc, html: &str) -> bool {
    doc.jodd.as_ref().is_none_or(|j| content_sha256(html) == j.content_sha256)
}

/// The target-dependent end of the body pipeline (after any link rewrite):
/// attachments become placeholders where they cannot be held, and tags
/// become a `Keywords:` line where body hashtags are not tags. Deterministic,
/// so a re-run on another device reproduces it (R10(b)).
pub(super) fn finish_body(html: &str, doc: &NoteDoc, t: &TargetTraits) -> String {
    let mut body = if t.holds_attachments {
        html.to_string()
    } else {
        let refs = doc.jodd.as_ref().map(|j| j.attachments.as_slice()).unwrap_or(&[]);
        strip_attachments(html, refs)
    };
    if t.inline_tags {
        body = append_keywords(&body, &doc.tags);
    }
    body
}

/// The live (not `deleted_pending`) note `uuid` names in `account_id`,
/// following a rekey alias (gotcha #16).
pub(super) fn live_note(db: &Db, uuid: &str, account_id: &str) -> Result<Option<CachedNote>, String> {
    let resolved = db.resolve_note_uuid(uuid, account_id).map_err(|e| e.to_string())?;
    let note = db.get(&resolved, account_id).map_err(|e| e.to_string())?;
    Ok(note.filter(|n| n.sync_state != SyncState::DeletedPending))
}

/// Whether `folder` or any of its ancestors is waiting for its delete to be
/// pushed (`list_folders` hides such a row, so it reads as absent).
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

fn restored_title(title: &str) -> String {
    format!("{title} (restored {})", chrono::Local::now().format("%Y-%m-%d"))
}

pub fn plan_restore(db: &Db, target: &Account, bundle: &OpenBundle, req: &RestoreRequest) -> Result<RestorePlan, String> {
    if db.full_listing_at(&target.id).map_err(|e| e.to_string())?.is_none() {
        return Err(TARGET_NOT_SYNCED_YET.to_string());
    }
    let t = TargetTraits::of(target);
    if !t.caps.writes.notes {
        return Err("Jodd cannot write notes to this account.".to_string());
    }
    let by_id: HashMap<String, &BundleNote> = bundle.notes().iter().map(|n| (source_id(n), n)).collect();

    let mut seen = HashSet::new();
    let mut notes = Vec::new();
    for id in &req.source_uuids {
        if !seen.insert(id.as_str()) {
            continue;
        }
        notes.push(match by_id.get(id) {
            Some(n) => plan_note(db, target, &t, bundle, n, req)?,
            None => PlannedNote {
                source_uuid: id.clone(),
                title: String::new(),
                target_folder: String::new(),
                action: Action::Refuse { reason: NOT_IN_BACKUP.to_string() },
                losses: Vec::new(),
            },
        });
    }

    let mut folders_to_create = Vec::new();
    if t.caps.writes.folders {
        let root = target.effective_notes_label();
        let existing: HashSet<String> =
            db.list_folders(&target.id).map_err(|e| e.to_string())?.into_iter().map(|f| f.path).collect();
        folders_to_create = notes
            .iter()
            .filter(|n| matches!(n.action, Action::Create | Action::KeepBoth { .. }))
            .map(|n| n.target_folder.clone())
            .filter(|f| f != root && !existing.contains(f))
            .collect();
        folders_to_create.sort();
        folders_to_create.dedup();
    }
    Ok(RestorePlan { notes, folders_to_create })
}

/// One note, the rules of the brief first match wins (rules 1–2 are the
/// whole-plan checks in `plan_restore`).
fn plan_note(
    db: &Db,
    target: &Account,
    t: &TargetTraits,
    bundle: &OpenBundle,
    bn: &BundleNote,
    req: &RestoreRequest,
) -> Result<PlannedNote, String> {
    let info = bundle.info();
    let doc = &bn.doc;
    let jodd = doc.jodd.as_ref();
    let id = source_id(bn);
    let planned = |action: Action, target_folder: String| PlannedNote {
        source_uuid: id.clone(),
        title: doc.title.clone(),
        target_folder,
        action,
        losses: Vec::new(),
    };

    // 3. A corrupted or hand-edited body must not restore silently wrong text.
    let html = decoded_html(doc);
    if !checksum_ok(doc, &html) {
        return Ok(planned(Action::Refuse { reason: CHECKSUM_MISMATCH.to_string() }, String::new()));
    }
    // R9: a cid: reference the strip did not recognise would block the push forever.
    let finished = finish_body(&html, doc, t);
    // The test is SSH's own refusal (`ssh/write.rs`), so prose such as
    // "Folic acid: 400 mcg" is not mistaken for a reference.
    if !t.holds_attachments && !crate::mime822::referenced_cids(&finished).is_empty() {
        return Ok(planned(Action::Refuse { reason: CID_LEFT.to_string() }, String::new()));
    }
    let sha = source_sha(doc, &html);
    let finished_sha = content_sha256(&finished);

    // 4. Restored before: R10(a) compares the SOURCE sha the map recorded.
    let mut existing: Option<CachedNote> = None;
    if let Some(row) = db.restore_lookup(&info.account_id, &id, &target.id).map_err(|e| e.to_string())? {
        if let Some(n) = live_note(db, &row.target_uuid, &target.id)? {
            if row.content_sha256 == sha {
                // A Replace never creates a row, so it may run again once the
                // restored note has been edited since; anything else skips.
                let current = content_sha256(&n.body_html);
                let edited_since = current != finished_sha && current != sha;
                if !(req.existing == ExistingChoice::Replace && edited_since) {
                    return Ok(planned(Action::SkipAlreadyRestored, n.label));
                }
            }
            existing = Some(n);
        }
    }
    // 5. The target holds it already (R10(b): the raw body, or the body this
    //    restore would write, before any link rewrite). Also when the map
    //    points at another note: a later version's restore repoints it, and
    //    this version's own copy is still live.
    //    The note this backup came from (same account, still there) is not a
    //    duplicate: it is the note the owner's Keep both / Replace / Skip is
    //    about, so finding it here must not overrule that choice (live pass
    //    2026-10-24: Keep both promised a copy and restored nothing).
    let original = match (target.id == info.account_id, jodd) {
        (true, Some(j)) => live_note(db, &j.uuid, &target.id)?,
        _ => None,
    };
    let found = db
        .find_content_match(&target.id, &doc.title, &[sha.as_str(), finished_sha.as_str()])
        .map_err(|e| e.to_string())?;
    if let Some(uuid) = found {
        let is_original = original.as_ref().is_some_and(|o| o.uuid == uuid);
        // Replacing a note with identical text changes nothing.
        if !is_original || req.existing == ExistingChoice::Replace {
            let folder = db.get(&uuid, &target.id).map_err(|e| e.to_string())?.map(|n| n.label).unwrap_or_default();
            return Ok(planned(Action::SkipContentMatch, folder));
        }
    }
    // 6. Same account, and the note is still there.
    if existing.is_none() {
        existing = original;
    }

    let action = match existing {
        None => Action::Create,
        Some(ref n) => match req.existing {
            ExistingChoice::KeepBoth => Action::KeepBoth { existing_uuid: n.uuid.clone() },
            // §11: a note Jodd renders only partially would be refused at push.
            ExistingChoice::Replace if db.render_loss(&target.id, &n.uuid).map_err(|e| e.to_string())?.is_some() => {
                Action::KeepBoth { existing_uuid: n.uuid.clone() }
            }
            ExistingChoice::Replace => Action::Replace { existing_uuid: n.uuid.clone() },
            ExistingChoice::Skip => return Ok(planned(Action::SkipExisting, n.label.clone())),
        },
    };

    let mut losses = Vec::new();
    let target_folder = match (&action, &existing) {
        // An edit never moves a note (`save_note_db`): it stays where it is.
        (Action::Replace { .. }, Some(n)) => n.label.clone(),
        _ => {
            let source_folder = jodd.map_or(info.root_label.as_str(), |j| j.folder.as_str());
            let (folder, loss) = map_folder(
                source_folder,
                &info.root_label,
                target.effective_notes_label(),
                &req.placement,
                t.caps.writes.folders,
            );
            losses.extend(loss);
            folder
        }
    };
    // R15: `create_folder_local_new` leaves a folder whose delete is pending
    // as it is, so the worker would push that delete with these notes inside.
    if !matches!(action, Action::Replace { .. }) && folder_being_deleted(db, &target.id, &target_folder)? {
        return Ok(planned(Action::Refuse { reason: FOLDER_BEING_DELETED.to_string() }, target_folder));
    }
    if let Some(j) = jodd {
        if !t.holds_attachments {
            losses.extend(j.attachments.iter().map(|r| Loss::Attachment {
                filename: r
                    .filename
                    .clone()
                    .filter(|f| !f.is_empty())
                    .unwrap_or_else(|| r.content_id.trim_matches(|c| c == '<' || c == '>').to_string()),
            }));
        }
        if j.pinned && !t.caps.writes.sidecars {
            losses.push(Loss::Pin);
        }
    }
    if t.inline_tags && !crate::llm::markdown::keywords_line(&doc.tags).is_empty() {
        losses.push(Loss::TagsAsKeywords);
    }
    if let Some(reason) = jodd.and_then(|j| j.partial.clone()) {
        losses.push(Loss::Partial { reason });
    }

    let title = match action {
        Action::KeepBoth { .. } => restored_title(&doc.title),
        _ => doc.title.clone(),
    };
    Ok(PlannedNote { source_uuid: id, title, target_folder, action, losses })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_shapes() {
        assert_eq!(serde_json::to_string(&ExistingChoice::KeepBoth).unwrap(), r#""keepBoth""#);
        assert_eq!(serde_json::to_string(&Action::SkipAlreadyRestored).unwrap(), r#"{"kind":"skipAlreadyRestored"}"#);
        assert_eq!(
            serde_json::to_string(&Action::KeepBoth { existing_uuid: "u".into() }).unwrap(),
            r#"{"kind":"keepBoth","existingUuid":"u"}"#
        );
        let req: RestoreRequest = serde_json::from_str(
            r#"{"destinationId":"d","slug":"s","stamp":"t","sourceUuids":["a"],"targetAccount":"gmail:b",
                "placement":"original","existing":"replace"}"#,
        )
        .unwrap();
        assert_eq!(req.existing, ExistingChoice::Replace);
    }
}
