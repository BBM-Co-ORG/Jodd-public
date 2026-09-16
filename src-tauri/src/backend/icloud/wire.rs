//! CloudKit's private database web service: the request shape, and the decode.
//!
//! This is a **record store**, not an email host. There is no MIME, no HTML on
//! the wire and no envelope: `changes/zone` returns typed records with typed
//! fields, and a note's body is a compressed protobuf document that
//! [`super::doc`] turns into HTML.
//!
//! # Shape of this module
//!
//! Everything that turns JSON into Jodd's neutral types is a **pure function
//! over `serde_json::Value`**, tested against fixtures shaped like real
//! `changes/zone` responses. The HTTP call is a thin wrapper at the bottom.
//! That split is what lets the folder-tree builder — which has three separate
//! ways to be silently wrong (a `/` in a title, a cycle, a missing parent) —
//! be exercised without an Apple ID.
//!
//! # M1 is read-only
//!
//! Nothing here writes. `records/modify` and the `recordChangeTag` optimistic
//! lock are M2's, and the reason is stated in the design spec: lossiness can
//! never propagate back from a backend that cannot write, and this backend's
//! content model is only partly decoded (attribute runs are not).

use std::collections::{HashMap, HashSet};

use base64::Engine;
use serde_json::{json, Value};

use crate::backend::{Note, TransportError};

/// The Notes zone in the private database. Fixed by Apple.
const DATABASE_PATH: &str = "/database/1/com.apple.notes/production/private";
const ZONE_NAME: &str = "Notes";

/// CloudKit's own JS-client version strings.
///
/// **These are the one pair that genuinely cannot be read from the live
/// session** — they are not among the parameters Apple's first `setup.icloud.com`
/// request carries, which is what `icloud_auth::INIT_SCRIPT` harvests. So they
/// are captured from a real web-client session and kept here, named and loud,
/// rather than buried at a call site (PRIOR-ART practice #5). A documented
/// maintenance tax, not a hidden one: if Apple ships a web build that rejects
/// them, this is the place to look.
const CKJS_BUILD_VERSION: &str = "2310ProjectDev27";
const CKJS_VERSION: &str = "2.6.4";

/// Apple's own web client asks for exactly this set.
///
/// M1 reads five of them. The rest are sent anyway **on purpose**: this is a
/// private API, and a request whose shape does not match the only client Apple
/// expects is the kind of difference a server is free to notice. Asking for
/// what the web client asks for is the cheapest way to stay unremarkable.
pub const DESIRED_KEYS: &[&str] = &[
    "TitleEncrypted", "SnippetEncrypted", "FirstAttachmentUTIEncrypted",
    "FirstAttachmentThumbnail", "FirstAttachmentThumbnailOrientation",
    "CreationDate", "ModificationDate", "Deleted", "Folders", "Folder",
    "Attachments", "ParentFolder", "Note", "LastViewedModificationDate",
    "MinimumSupportedNotesVersion", "DisplayTextEncrypted",
    "StandardizedContentEncrypted", "TokenContentIdentifierEncrypted",
    "AltTextEncrypted", "UTIEncrypted", "MergeableDataEncrypted", "IsPinned",
    "TextDataEncrypted",
    // The fields the captured web client ECHOES on every note update
    // (icloud-md's `ECHOED_FIELDS`, `encodeNoteRecord.ts`) that the list
    // above did not already carry. Absent from every read until 2026-08-26,
    // which is the same blind spot that hid `FoldersModificationDate` before
    // the relocation fix (gotcha #22): a field no read requests is a field
    // no diagnostic can ever see, and a write that fails to echo it cannot
    // even know what it dropped. `ReplicaIDToNotesVersionDataEncrypted` is
    // the one that matters most — a per-replica version map Apple's own
    // merge machinery consults; a document whose replica table names a
    // replica this map has never heard of is the leading suspect for the
    // duplicated-note merge fallback the 2026-08-26 live edit produced.
    "ReplicaIDToNotesVersionDataEncrypted", "ReplicaIDToUserIDEncrypted",
    "AttachmentViewType", "PaperStyleType", "FoldersModificationDate",
    "TextDataAsset",
];

const DESIRED_RECORD_TYPES: &[&str] = &[
    "AccountData", "Note", "SearchIndexes", "Folder", "PasswordProtectedNote",
    "User", "Users", "Note_UserSpecific", "PasswordProtectedNote_UserSpecific",
    "Folder_UserSpecific", "cloudkit.share", "Hashtag", "InlineAttachment",
];

/// The root folder every account has. Maps to the path `Notes`.
pub const DEFAULT_FOLDER: &str = "DefaultFolder-CloudKit";
/// Excluded from the tree, and its notes from every listing.
pub const TRASH_FOLDER: &str = "TrashFolder-CloudKit";

/// The record types Apple uses to keep PER-USER state off the note itself.
///
/// **The pin is not on the note record.** Measured 2026-08-23 on a live
/// account: `IsPinned` appears on **zero** of 588 root `Note` records, while
/// the zone carries 261 `Note_UserSpecific` records alongside 779 notes. That
/// is where Apple puts state belonging to the viewing user rather than to the
/// note — which is also why the count is well under the note count: a note
/// nobody has pinned or opened has no such record at all.
///
/// `Note.pinned`'s own comment in `backend/mod.rs` says the pin "never travels
/// over the wire (Apple Notes stores pin in iCloud metadata, which the email
/// backend doesn't carry)". True of the email backends, and this is the
/// iCloud metadata it was talking about.
pub const USER_SPECIFIC_TYPES: &[&str] =
    &["Note_UserSpecific", "PasswordProtectedNote_UserSpecific"];

/// What the `*_UserSpecific` records said about pinning.
#[derive(Debug, Default, Clone)]
pub struct PinScan {
    /// Record names of the notes the user has pinned.
    pub pinned: HashSet<String>,
    /// How many per-user records were examined.
    pub seen: usize,
    /// Per-user records that are themselves tombstones.
    ///
    /// **They carry a stale `IsPinned` and must not be read.** Measured
    /// 2026-08-23: the account reported 17 pinned notes where Apple showed 2.
    /// The per-user record has its own `Deleted` field (present on 265 of 266
    /// records) — it is a record like any other, and a deleted one describes
    /// state that no longer applies.
    pub deleted: usize,
    /// How many carried no resolvable reference back to a note.
    ///
    /// **Not folded into a silent zero.** If Apple's field names are not what
    /// this expects, every pin would simply fail to join and the feature would
    /// look "implemented but the account has no pins" — the same silence that
    /// let the locked notes vanish. A number that should be zero and is not
    /// says which.
    pub unjoinable: usize,
}

/// Reads the pin out of the per-user records and keys it by the note it
/// belongs to.
pub fn collect_pins(records: &[Value]) -> PinScan {
    let mut scan = PinScan::default();
    for r in records
        .iter()
        .filter(|r| matches!(r["recordType"].as_str(), Some(t) if USER_SPECIFIC_TYPES.contains(&t)))
    {
        scan.seen += 1;
        // A tombstoned per-user record still carries the pin it had when it
        // died. Reading it pins notes the user unpinned.
        if is_truthy(&r["fields"]["Deleted"]["value"]) {
            scan.deleted += 1;
            continue;
        }
        // The reference back to the note. A per-user record is meaningless
        // without it, so a missing one is counted rather than skipped.
        let Some(note_id) = r["fields"]["Note"]["value"]["recordName"].as_str() else {
            scan.unjoinable += 1;
            continue;
        };
        if is_truthy(&r["fields"]["IsPinned"]["value"]) {
            scan.pinned.insert(note_id.to_string());
        }
    }
    scan
}

/// One inline text attachment — a hashtag, mention, or inline link token.
///
/// The note document carries `U+FFFC` where the object sits and an
/// `attributeRun.attachmentInfo` whose `attachmentIdentifier` IS the
/// `InlineAttachment` record's `recordName` (icloud-md's `push.js` looks
/// records up by it directly). The record's `AltTextEncrypted` is the
/// rendered text (`#work`) and `UTIEncrypted` says what kind of token it
/// is — both base64 of PLAIN TEXT, "Encrypted" being Apple's at-rest
/// naming, exactly like `TitleEncrypted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineRef {
    pub type_uti: String,
    pub alt_text: String,
}

/// Reads every `InlineAttachment` record in the walk into
/// `recordName -> InlineRef`. These records were requested (and discarded)
/// since the 2026-08-26 envelope conformance — the same blind spot that hid
/// `FoldersModificationDate`. One record can arrive on several pages
/// (gotcha #22); a later arrival replaces the earlier, matching the
/// `bases` rule. Tombstoned records are skipped: a dead ref describes a
/// tag the user removed.
pub fn collect_inline_refs(records: &[Value]) -> HashMap<String, InlineRef> {
    // Newest `ModificationDate` wins, feed order as the tiebreak — the same
    // rule the note dedup uses, and for the same reason (gotcha #22 records
    // last-in-the-walk-wins being wrong once already).
    let mut refs: HashMap<String, (Option<i64>, InlineRef)> = HashMap::new();
    for r in records.iter().filter(|r| r["recordType"] == Value::from("InlineAttachment")) {
        if r["deleted"] == Value::Bool(true) || is_deleted(r) {
            continue;
        }
        let Some(name) = r["recordName"].as_str() else { continue };
        let (Some(type_uti), Some(alt_text)) = (
            decode_text_field(&r["fields"]["UTIEncrypted"]),
            decode_text_field(&r["fields"]["AltTextEncrypted"]),
        ) else {
            // A ref with no text renders nothing useful; skip rather than
            // invent an empty tag.
            continue;
        };
        let modified = r["fields"]["ModificationDate"]["value"].as_i64();
        let candidate = (modified, InlineRef { type_uti, alt_text });
        match refs.get(name) {
            Some((held, _)) if candidate.0 < *held => {}
            _ => {
                refs.insert(name.to_string(), candidate);
            }
        }
    }
    refs.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

/// CloudKit spells a boolean as a number in some fields and a JSON bool in
/// others. Accept either, and treat anything else as "not set" rather than
/// guessing.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64().is_some_and(|i| i != 0),
        _ => false,
    }
}

/// The folder path Jodd uses as the root, matching every other backend.
///
/// Because `DefaultFolder-CloudKit` maps here, gotcha #9's five hardcoded
/// `"Notes"` literals on the folder side are **correct** on this backend rather
/// than the limitation they are on Gmail.
pub const ROOT_PATH: &str = "Notes";

/// U+2215 DIVISION SLASH — what a `/` inside a folder title becomes.
///
/// `folders.path` is a `/`-joined string, so a real slash in a title would
/// forge a segment: "Q1/Q2" becomes a folder "Q1" containing "Q2", and two
/// unrelated trees can collide on one path. **Measured — at least one folder on
/// the 776-note account has a `/` in its name**, so this is a live case, not a
/// hypothetical. The look-alike substitution is icloud-md's own trick, which
/// PRIOR-ART already records for the file-export case.
const SLASH_LOOKALIKE: char = '\u{2215}';

/// How deep a `ParentFolder` chain may go before it is treated as broken.
///
/// A cycle would spin forever, and a malformed chain is indistinguishable from
/// a deep one from inside the walk. Five levels is the measured depth of a real
/// account; sixteen leaves room without ever letting a bad chain run.
const MAX_FOLDER_DEPTH: usize = 16;

// ────────────────────────────────────────────────────────────────────────────
// Field decoding
// ────────────────────────────────────────────────────────────────────────────

/// Reads a CloudKit string field that is base64 of **plain text**.
///
/// `TitleEncrypted` and `SnippetEncrypted` are the ones that matter here.
/// "Encrypted" describes Apple's server-side at-rest encryption — there is
/// nothing for a client to undo, and treating the name literally is how a
/// reader concludes this backend needs a key it does not have.
pub fn decode_text_field(field: &Value) -> Option<String> {
    let b64 = field["value"].as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    String::from_utf8(bytes).ok()
}

/// Reads a CloudKit timestamp (milliseconds since the epoch) into the same
/// string shape every other backend stores.
///
/// Goes through `mime822::format_apple_date` deliberately: `notes.date` is
/// compared and sorted across backends, and a second date format in that column
/// would make an iCloud note sort against a Gmail note by luck.
pub fn decode_date_field(field: &Value) -> Option<String> {
    apple_date_from_ms(field["value"].as_i64()?)
}

fn record_name(r: &Value) -> Option<&str> {
    r["recordName"].as_str()
}

/// `Deleted == 1` means the record is a tombstone. Excluded everywhere.
///
/// Public because it applies to every record type, not only `Note` — a
/// password-protected note has tombstones too, and counting one as live would
/// report a locked note the user already deleted.
pub fn is_deleted_record(r: &Value) -> bool {
    is_deleted(r)
}

fn is_deleted(r: &Value) -> bool {
    r["fields"]["Deleted"]["value"].as_i64() == Some(1)
}

// ────────────────────────────────────────────────────────────────────────────
// Component G — the folder tree
// ────────────────────────────────────────────────────────────────────────────

/// One `Folder` record, reduced to what the tree needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRecord {
    pub record_name: String,
    pub title: String,
    pub parent: Option<String>,
}

/// Extracts the `Folder` records from a page of the zone.
pub fn folder_records(records: &[Value]) -> Vec<FolderRecord> {
    records
        .iter()
        .filter(|r| r["recordType"] == json!("Folder") && !is_deleted(r))
        .filter_map(|r| {
            Some(FolderRecord {
                record_name: record_name(r)?.to_string(),
                // A folder with no readable title still has an identity, and
                // dropping it would orphan every note filed under it. Fall back
                // to the record name so the subtree survives, visibly odd
                // rather than invisibly gone.
                title: decode_text_field(&r["fields"]["TitleEncrypted"])
                    .unwrap_or_else(|| record_name(r).unwrap_or_default().to_string()),
                parent: r["fields"]["ParentFolder"]["value"]["recordName"]
                    .as_str()
                    .map(String::from),
            })
        })
        .collect()
}

/// Builds `record id → folder path` for the whole tree.
///
/// **Real nesting, unlike Microsoft.** `Folder` records carry a `ParentFolder`
/// reference, so this is a genuine `Notes/A/B` tree and every existing subtree
/// query works untouched — measured 2026-08-22 at 102 folders, 5 levels deep.
/// (On Exchange the parent/child shape is unrecoverable at any price — gotcha
/// #12 — which is why that backend presents a flat list.)
///
/// Three hazards, all closed here, all silent if missed:
///
/// - **A `/` in a title** would forge a path segment. Replaced with
///   [`SLASH_LOOKALIKE`]. Measured live, not hypothetical.
/// - **A cycle or a broken chain** would spin forever. The walk is capped at
///   [`MAX_FOLDER_DEPTH`], and anything hitting the cap is filed directly
///   under [`ROOT_PATH`] rather than dropped — a folder in the wrong place is
///   recoverable, a folder that vanished takes its notes with it.
/// - **[`TRASH_FOLDER`]** is excluded entirely, consistent with
///   `has_trash: false`.
pub fn build_folder_paths(folders: &[FolderRecord]) -> HashMap<String, String> {
    let by_id: HashMap<&str, &FolderRecord> =
        folders.iter().map(|f| (f.record_name.as_str(), f)).collect();

    let mut out = HashMap::new();
    for f in folders {
        if f.record_name == TRASH_FOLDER {
            continue;
        }
        if let Some(path) = path_for(&f.record_name, &by_id) {
            out.insert(f.record_name.clone(), path);
        }
    }
    out
}

/// Walks one folder up to the root, joining titles.
///
/// Returns `None` only for the Trash subtree, which is excluded rather than
/// mapped.
fn path_for(id: &str, by_id: &HashMap<&str, &FolderRecord>) -> Option<String> {
    if id == TRASH_FOLDER {
        return None;
    }
    if id == DEFAULT_FOLDER {
        return Some(ROOT_PATH.to_string());
    }

    let mut segments: Vec<String> = Vec::new();
    let mut cursor = id;
    for _ in 0..MAX_FOLDER_DEPTH {
        let Some(f) = by_id.get(cursor) else {
            // The parent is not in this page. Filing under the root keeps the
            // folder and its notes visible; the next full sync repairs the
            // path once the parent arrives.
            break;
        };
        if f.record_name == DEFAULT_FOLDER {
            break;
        }
        if f.record_name == TRASH_FOLDER {
            return None;
        }
        segments.push(sanitize_segment(&f.title));
        match f.parent.as_deref() {
            Some(p) => cursor = p,
            None => break,
        }
    }

    segments.reverse();
    let mut path = String::from(ROOT_PATH);
    for s in segments {
        path.push('/');
        path.push_str(&s);
    }
    Some(path)
}

/// One path segment, with anything that would forge structure neutralized.
fn sanitize_segment(title: &str) -> String {
    let cleaned = title.replace('/', &SLASH_LOOKALIKE.to_string());
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        // An empty segment would collapse into the parent's path and two
        // siblings would share it. Name it rather than lose it.
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Component D — field mapping
// ────────────────────────────────────────────────────────────────────────────

/// Why a `Note` record did not become a [`Note`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `Deleted == 1` — a tombstone. Gone, with nothing to restore.
    ///
    /// **Not the same as sitting in Recently Deleted**, which is
    /// [`Decoded::Trashed`]. M1 collapsed the two because neither appears in a
    /// listing and the difference bought nothing; it buys a restore button.
    Deleted,
    /// The body did not decode. **Carries the split from
    /// [`super::doc::DecodeError`] intact** — the Advanced Data Protection
    /// verdict consumes it and must never re-derive compression magic of its
    /// own, and a `Malformed` note must never count toward that verdict.
    Undecodable(super::doc::DecodeError),
    /// The record is missing something structural — no `recordName`, no body
    /// field at all. Not an ADP signal.
    Incomplete(String),
}

/// One decoded record: either a note, or the reason it is not one.
#[derive(Debug)]
pub enum Decoded {
    Note(Box<Note>),
    /// Filed in Apple's Recently Deleted — **recoverable**, and decoded in
    /// full so the trash view can preview it. Its `label` is the root rather
    /// than a real folder: the record's `Folder` now names the Trash, and
    /// where it came from is not something this decode can know (see
    /// `Scan.trashed`).
    Trashed(Box<Note>),
    Skipped { record_name: String, reason: SkipReason },
}

/// Turns one `Note` record into Jodd's neutral [`Note`].
///
/// | `Note` field | Source |
/// |---|---|
/// | `id`, `uuid` | `recordName`, **verbatim** — lowercase and case-sensitive (gotcha #18) |
/// | `title` | `TitleEncrypted` |
/// | `body_html` | `TextDataEncrypted` → gzip/zlib → protobuf → HTML, title line removed |
/// | `version` | `recordChangeTag` — a real optimistic-lock token, unlike Gmail's id |
/// | `date` | `ModificationDate` |
/// | `x_mail_created_date` | `CreationDate` |
/// | `label` | the folder path from [`build_folder_paths`] |
/// | `pinned` | `IsPinned` — Apple's own pin, read for the first time on any backend |
///
/// **The title is cut by POSITION, and the title field only verifies** — see
/// [`super::doc::strip_leading_title`] and gotcha #21. That is the exact
/// inverse of the Exchange rule, and it is what 776 measured notes say.
///
/// `pin_dirty` is never set anywhere on this backend: `writes.sidecars: false`
/// blocks it at the command layer and `sidecars_supported` gates the worker's
/// drain below that, so one un-pushable pin cannot make the account
/// permanently unremovable (gotcha #2's wedge).
pub fn decode_note(r: &Value, folder_paths: &HashMap<String, String>) -> Decoded {
    // No refs to resolve: formatting still renders; hashtags keep their raw
    // object character. The change detector and the older tests use this.
    decode_note_with_refs(r, folder_paths, &HashMap::new())
}

/// [`decode_note`] with the walk's `InlineAttachment` map, so inline
/// hashtags render as their text (M3 F5). The zone walk calls this one.
pub fn decode_note_with_refs(
    r: &Value,
    folder_paths: &HashMap<String, String>,
    inline_refs: &HashMap<String, InlineRef>,
) -> Decoded {
    let Some(name) = record_name(r) else {
        return Decoded::Skipped {
            record_name: "<unnamed>".to_string(),
            reason: SkipReason::Incomplete("record carries no recordName".into()),
        };
    };
    let name = name.to_string();

    if is_deleted(r) {
        return Decoded::Skipped { record_name: name, reason: SkipReason::Deleted };
    }

    let fields = &r["fields"];
    let folder_id = fields["Folder"]["value"]["recordName"].as_str().unwrap_or(DEFAULT_FOLDER);
    // The Trash check is deliberately AFTER the body decode below, not here:
    // a trashed note is previewable, so its body has to come through. The cost
    // is one gunzip per trashed record on every walk, and Apple purges the
    // folder after thirty days.
    let trashed = folder_id == TRASH_FOLDER;

    let Some(b64) = fields["TextDataEncrypted"]["value"].as_str() else {
        return Decoded::Skipped {
            record_name: name,
            reason: SkipReason::Incomplete("no TextDataEncrypted field".into()),
        };
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        // Not base64 at all is a structural problem, not encrypted content.
        // Calling it Unreadable here would feed the ADP verdict a false
        // positive from what is really a malformed response.
        return Decoded::Skipped {
            record_name: name,
            reason: SkipReason::Incomplete("TextDataEncrypted is not base64".into()),
        };
    };

    // The WHOLE document, not just the text: the attribute runs are what the
    // format-aware rendering below reads (M3).
    let document = match super::compose::parse(&bytes) {
        Ok(d) => d,
        Err(e) => return Decoded::Skipped { record_name: name, reason: SkipReason::Undecodable(e) },
    };
    let text = document.text().to_string();

    let title_field = decode_text_field(&fields["TitleEncrypted"]).unwrap_or_default();
    // The note's own first line, NOT `TitleEncrypted` — see `doc::note_title`.
    // M2's write path recomposes `title + body` into the text, so caching
    // Apple's lossy derivation here would truncate the note's first line on
    // the server for one note in four.
    let title = super::doc::note_title(&text, &title_field);

    // A folder id with no path means the Folder record was not in this page.
    // The root is the honest fallback: the note stays visible and the next
    // sync files it correctly.
    let label = folder_paths
        .get(folder_id)
        .cloned()
        .unwrap_or_else(|| ROOT_PATH.to_string());

    let note = Box::new(Note {
        id: name.clone(),
        // Verbatim. `canonical_uuid_for(ICloud, …)` is pass-through precisely
        // because uppercasing a recordName produces an id naming nothing
        // (gotcha #18).
        uuid: name,
        title,
        body_html: super::doc::note_body_html_formatted(
            &text,
            &title_field,
            &document.string.attribute_run,
            inline_refs,
        ),
        date: decode_date_field(&fields["ModificationDate"]).unwrap_or_default(),
        version: r["recordChangeTag"].as_str().unwrap_or_default().to_string(),
        label,
        x_mail_created_date: decode_date_field(&fields["CreationDate"]),
        account_id: None,
        pinned: fields["IsPinned"]["value"] == json!(1)
            || fields["IsPinned"]["value"] == json!(true),
        local_version: 0,
        push_blocked_reason: None,
        // M1 does not fetch attachments. An inline attachment is a separate
        // record type and its bytes are a separate request.
        attachments: Vec::new(),
    });
    if trashed {
        Decoded::Trashed(note)
    } else {
        Decoded::Note(note)
    }
}

/// Turns a `PasswordProtectedNote` record into a visible, unreadable note.
///
/// **Measured 2026-08-23 on a live account**: a locked record carries the same
/// field set as a `Note` — `TitleEncrypted`, `Folder`, `CreationDate`,
/// `ModificationDate` — and its title is base64 of **plain text** exactly like
/// an ordinary note's. Only `TextDataEncrypted` is genuinely encrypted, with a
/// key Jodd does not have and should not want. So everything except the body
/// is readable, and a locked note can appear where it belongs: right folder,
/// real title, real dates.
///
/// **Why it is shown at all.** Skipping it — which is what the `recordType ==
/// "Note"` filter did — makes Jodd's folder counts disagree with Apple's with
/// nothing anywhere to explain the difference. Measured: Apple showed
/// `subfolder-Locked-Note` with two notes and Jodd showed one.
///
/// **Why the body is a sentence and not empty.** An empty body is gotcha #17's
/// landmine and Component H3 forbids caching one. A placeholder is not that
/// failure's shape: the danger there is *silent* emptiness that a later write
/// pushes back over real content, and this is the opposite of silent — the
/// user opens the note and reads why it is blank.
///
/// **M2 obligation, and it is not optional.** A write path must never push
/// this body. It cannot be correct even by accident: the remote record is a
/// `PasswordProtectedNote`, a different record type from the `Note` a write
/// would modify. M2 has to refuse these explicitly.
pub fn decode_locked_note(
    r: &Value,
    folder_paths: &HashMap<String, String>,
) -> Option<Note> {
    if is_deleted(r) {
        return None;
    }
    let name = record_name(r)?.to_string();
    let fields = &r["fields"];
    let folder_id = fields["Folder"]["value"]["recordName"].as_str().unwrap_or(DEFAULT_FOLDER);
    if folder_id == TRASH_FOLDER {
        return None;
    }

    Some(Note {
        id: name.clone(),
        uuid: name,
        title: decode_text_field(&fields["TitleEncrypted"]).unwrap_or_default(),
        body_html: LOCKED_BODY_HTML.to_string(),
        date: decode_date_field(&fields["ModificationDate"]).unwrap_or_default(),
        version: r["recordChangeTag"].as_str().unwrap_or_default().to_string(),
        label: folder_paths.get(folder_id).cloned().unwrap_or_else(|| ROOT_PATH.to_string()),
        x_mail_created_date: decode_date_field(&fields["CreationDate"]),
        account_id: None,
        pinned: false,
        local_version: 0,
        push_blocked_reason: None,
        attachments: Vec::new(),
    })
}

/// What the body of a locked note says.
///
/// Deliberately a full sentence naming the one place the content can be read.
/// The user's only possible action lives in Apple Notes, not in Jodd, so a
/// bare "unavailable" would leave them nowhere to go.
pub const LOCKED_BODY_HTML: &str = "<div>🔒 This note is password-protected in Apple Notes.      Jodd can see its title, but not its contents — open it in Apple Notes to read it.</div>";

/// One page of `changes/zone`, decoded.
#[derive(Debug, Default)]
pub struct ZonePage {
    pub records: Vec<Value>,
    pub sync_token: Option<String>,
    pub more_coming: bool,
}

/// What one `changes/zone` reply means to the walk.
///
/// **A rejected `syncToken` arrives inside an HTTP 200**, so it cannot be a
/// `TransportError` — nothing about the transport failed. icloud-md established
/// the shape by live probing, and the design spec puts the rule in M1 on
/// correctness grounds rather than deferring it with the rest of incremental
/// sync: an implementation that treats a rejected token as an error fails
/// rarely and confusingly, while the right answer (refetch from scratch, say
/// so) costs one extra page walk. A merely *old* token — icloud-md measured 15
/// days — still syncs incrementally, so this is not a routine event.
#[derive(Debug)]
pub enum ZoneReply {
    Page(ZonePage),
    /// The server refused the token. Start again with none.
    TokenRejected(String),
}

/// Detects a zone-level error inside an otherwise-successful response.
///
/// Returns the server's own reason string so a log says why rather than
/// "something went wrong". Any zone error counts, not only `BAD_REQUEST`: the
/// recovery — walk the zone from scratch — is the same for every one of them,
/// and a code this function has never seen is exactly the case where guessing
/// a narrower rule costs a silent empty sync.
pub fn zone_error(body: &Value) -> Option<String> {
    let z = &body["zones"][0];
    let code = z["serverErrorCode"].as_str()?;
    let reason = z["reason"].as_str().unwrap_or("no reason given");
    Some(format!("{code}: {reason}"))
}

/// Reads the envelope `changes/zone` wraps its records in.
pub fn decode_zone_page(body: &Value) -> ZonePage {
    let z = &body["zones"][0];
    ZonePage {
        records: z["records"].as_array().cloned().unwrap_or_default(),
        sync_token: z["syncToken"].as_str().map(String::from),
        more_coming: z["moreComing"] == json!(true),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// The request
// ────────────────────────────────────────────────────────────────────────────

/// The body `changes/zone` takes, with or without a resume token.
///
/// `reverse: true` matches the web client: newest first, so a partial sync
/// shows the notes a user is most likely to be looking for.
pub fn zone_request_body(sync_token: Option<&str>) -> Value {
    let mut zone = json!({
        "zoneID": { "zoneName": ZONE_NAME },
        "desiredKeys": DESIRED_KEYS,
        "desiredRecordTypes": DESIRED_RECORD_TYPES,
        "reverse": true,
    });
    if let Some(t) = sync_token {
        zone["syncToken"] = json!(t);
    }
    json!({ "zones": [zone] })
}

/// The `changes/zone` URL for one account's partition.
///
/// `ck_host` comes from `/validate`'s `webservices.ckdatabasews.url` and
/// **cannot be guessed** — it is a per-account partition (`p149-…`).
pub fn zone_url(ck_host: &str, dsid: &str, client: &crate::icloud_auth::ClientConfig) -> String {
    format!(
        "{ck_host}{DATABASE_PATH}/changes/zone\
         ?ckjsBuildVersion={CKJS_BUILD_VERSION}&ckjsVersion={CKJS_VERSION}\
         &clientId={}&clientBuildNumber={}&clientMasteringNumber={}&dsid={dsid}",
        client.client_id, client.client_build_number, client.client_mastering_number
    )
}

/// Maps an HTTP status onto the shared retry policy's vocabulary.
///
/// **421 is `Auth`, not `Transient`.** It is CloudKit's "this session is over",
/// and a session here is a browser cookie jar that the live webview rotates on
/// its own — a captured copy measured dead inside 3.5 hours. Retrying with the
/// same jar can only fail; the revival path (Component B4) is what fixes it.
pub fn classify_status(status: u16, retry_after: Option<std::time::Duration>) -> TransportError {
    match status {
        429 => TransportError::RateLimited { retry_after },
        401 | 403 | 421 => TransportError::Auth,
        404 => TransportError::NotFound,
        500..=599 => TransportError::Transient {
            source: anyhow::anyhow!("CloudKit answered HTTP {status}"),
        },
        _ => TransportError::Permanent {
            source: anyhow::anyhow!("CloudKit answered HTTP {status}"),
        },
    }
}

/// Fetches one page of the Notes zone.
///
/// Paging is the caller's, not this function's: a caller that persists
/// `sync_token` between pages can resume a sync that died halfway, and one that
/// does not can simply loop. Hiding the loop in here would take that choice
/// away and make the whole zone one all-or-nothing request.
pub async fn fetch_zone_page(
    http: &reqwest::Client,
    ck_host: &str,
    dsid: &str,
    client: &crate::icloud_auth::ClientConfig,
    cookie_header: &str,
    sync_token: Option<&str>,
) -> Result<ZoneReply, TransportError> {
    let resp = http
        .post(zone_url(ck_host, dsid, client))
        .header("Cookie", cookie_header)
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.icloud.com")
        .header("Referer", crate::icloud_auth::ICLOUD_URL)
        .header("Accept", "application/json")
        .json(&zone_request_body(sync_token))
        .send()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        return Err(classify_status(status.as_u16(), retry_after));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;
    // Checked BEFORE decoding: an errored zone carries no `records` array, so
    // decoding first would hand the caller a perfectly ordinary empty page and
    // the rejection would vanish.
    if let Some(reason) = zone_error(&body) {
        return Ok(ZoneReply::TokenRejected(reason));
    }
    Ok(ZoneReply::Page(decode_zone_page(&body)))
}

// ────────────────────────────────────────────────────────────────────────────
// M2 — `records/modify`
// ────────────────────────────────────────────────────────────────────────────

/// What one note write puts on the wire.
///
/// Assembled by the vertical, which is the only layer that knows both the
/// edit and the record it is replacing; this module only turns it into JSON.
/// Keeping it a plain struct is what lets the request shape — the part no test
/// here can confirm against Apple — be exercised by fixtures instead of by a
/// live account.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteWrite {
    /// The `recordName`. Minted by Jodd for a create (a **lowercase** UUID —
    /// gotcha #18), carried for an update.
    pub record_name: String,
    /// The optimistic lock. `None` on a create; on an update it is the
    /// `recordChangeTag` the read observed, and the server re-checks it.
    pub change_tag: Option<String>,
    /// The note's full new plain text — the same text the document was built
    /// from. `TitleEncrypted` and `SnippetEncrypted` are DERIVED from it here
    /// (`derive_note_title` / `derive_note_snippet`), the way the captured
    /// web client derives them on every save, rather than passed in: two
    /// callers deriving separately is how the two fields drift apart.
    pub text: String,
    /// The text with resolved inline objects rendered as their display text
    /// (`doc::text_with_objects_rendered`) — what the title/snippet are
    /// derived from, matching Apple's own derivation, which renders objects
    /// as text (gotcha #21). `None` falls back to `text` (correct when the
    /// text carries no objects).
    pub display_text: Option<String>,
    /// `TextDataEncrypted` — the compressed document, already built by
    /// [`super::compose::encode`].
    pub document: Vec<u8>,
    /// The destination folder's `recordName`.
    pub folder: String,
    /// `CreationDate`, epoch ms. Carried on an update so a note's creation
    /// time is not rewritten by an edit.
    pub created_ms: Option<i64>,
    /// `ModificationDate`, epoch ms.
    pub modified_ms: i64,
    /// Raw field values read off the CURRENT remote record, to be echoed
    /// verbatim on an update — the captured web client copies these back on
    /// every save (icloud-md's `ECHOED_FIELDS`), and until 2026-08-26 this
    /// backend silently dropped all of them. Keyed by field name, holding the
    /// field's inner `value` exactly as the read returned it. Empty on a
    /// create (nothing exists to echo).
    pub echo: serde_json::Map<String, Value>,
}

fn bytes_field(bytes: &[u8]) -> Value {
    json!({ "value": base64::engine::general_purpose::STANDARD.encode(bytes), "type": "BYTES" })
}

/// Title truncation, ported from icloud-md's `deriveNoteTitle`
/// (`encodeNoteRecord.ts`), whose limits were reverse-engineered from a real
/// capture: a 255-character first line was cut to 66 characters at the last
/// space before position 76. The title is the first line, cut back to a word
/// boundary when long — cosmetic list-view metadata, re-derived by every
/// device that edits the note. Lengths are UTF-16 code units, the unit every
/// other length in this protocol counts in.
const TITLE_MAX_LENGTH: usize = 76;
const SNIPPET_MAX_LENGTH: usize = 500;
/// What the web client stores when there is no content after the first line —
/// the placeholder lives in the DATA, not the UI (icloud-md observed it
/// verbatim in captured saves of single-line notes).
const EMPTY_SNIPPET_PLACEHOLDER: &str = "No additional text";

/// Byte offset of the char boundary at (or just before) `limit` UTF-16 units.
fn byte_at_utf16(s: &str, limit: usize) -> usize {
    let mut units = 0usize;
    for (byte, ch) in s.char_indices() {
        let next = units + ch.len_utf16();
        if next > limit {
            return byte;
        }
        units = next;
    }
    s.len()
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The `TitleEncrypted` value for a note whose text is `text`.
pub fn derive_note_title(text: &str) -> &str {
    let first_line = text.split('\n').next().unwrap_or("");
    if utf16_len(first_line) <= TITLE_MAX_LENGTH {
        return first_line;
    }
    let hard_cut = byte_at_utf16(first_line, TITLE_MAX_LENGTH);
    // `lastIndexOf(" ", 76)` in the reference: the last space at or before
    // the limit. A space is one UTF-16 unit, so a byte search over the
    // prefix (extended by one unit to include a space AT the limit) is the
    // same answer.
    let search_end = byte_at_utf16(first_line, TITLE_MAX_LENGTH + 1).min(first_line.len());
    match first_line[..search_end].rfind(' ') {
        Some(space) if space > 0 => &first_line[..space],
        _ => &first_line[..hard_cut],
    }
}

/// The `SnippetEncrypted` value: what remains of the first line past the
/// (possibly truncated) title, or the second line — whichever the leading
/// whitespace strip lands on — capped at 500 units, with the captured
/// client's literal placeholder when nothing is left.
pub fn derive_note_snippet(text: &str) -> String {
    let title_len = derive_note_title(text).len();
    let after_title = text[title_len..].trim_start();
    let line = after_title.split('\n').next().unwrap_or("");
    let capped = &line[..byte_at_utf16(line, SNIPPET_MAX_LENGTH)];
    if capped.is_empty() { EMPTY_SNIPPET_PLACEHOLDER.to_string() } else { capped.to_string() }
}

fn timestamp_field(ms: i64) -> Value {
    json!({ "value": ms, "type": "TIMESTAMP" })
}

/// The CKReference shape Apple's own web client sends for a note's folder —
/// `action: "VALIDATE"`, not `"NONE"`. Confirmed against icloud-md's
/// `folderReference()` (`src/notes/encodeNoteRecord.ts`), itself built by
/// byte-matching captured HAR requests from a real Notes web session, not
/// guessed. This backend used `"NONE"` here until 2026-08-25; every prior
/// `Folder`-only write still reached CloudKit successfully (the server never
/// rejected the shape), which is exactly why the mismatch went unnoticed —
/// it costs nothing at the API layer and everything at the "does Apple's own
/// client honor this" layer, which is gotcha #22's open question.
fn folder_reference(record_name: &str) -> Value {
    json!({
        "recordName": record_name,
        "action": "VALIDATE",
        "zoneID": { "zoneName": ZONE_NAME },
    })
}

fn folder_field(record_name: &str) -> Value {
    json!({ "value": folder_reference(record_name), "type": "REFERENCE" })
}

/// `Folders` (plural) — a one-element reference LIST, not a second copy of
/// `Folder`. Apple's own client writes both on every relocation, together
/// with `FoldersModificationDate` (see `folder_relocation_fields` below):
/// this backend never wrote either before 2026-08-25, having only ever read
/// `Folders` for the trash-restore-origin question (gotcha #22).
///
/// **Confirmed live the same day**, against two notes created fresh through
/// Apple Notes.app (so each carried a real pre-existing `Folders`, unlike a
/// Jodd-created scratch note, which never had one to contradict): a move
/// showed up in Apple Notes.app in under a minute, a delete in Apple's own
/// Recently Deleted in 26 seconds — ordinary latency, not the 18+ hours the
/// pre-fix incident measured. The theory (Apple's clients decide a note's
/// folder from `Folders`, not `Folder`, once `Folders` holds a real value)
/// is confirmed, not just evidence-based.
fn folders_field(record_name: &str) -> Value {
    json!({ "value": [folder_reference(record_name)], "type": "REFERENCE_LIST" })
}

/// The three fields Apple's own client always writes together on a
/// relocation (icloud-md's `buildNoteRelocationFields`, HAR-byte-matched):
/// `Folder`, `Folders`, and `FoldersModificationDate`. Bundled so a caller
/// cannot update one without the other two — the asymmetry (write `Folder`
/// alone) is the bug this fix closes.
fn folder_relocation_fields(record_name: &str, now_ms: i64) -> serde_json::Map<String, Value> {
    let mut fields = serde_json::Map::new();
    fields.insert("Folder".to_string(), folder_field(record_name));
    fields.insert("Folders".to_string(), folders_field(record_name));
    fields.insert("FoldersModificationDate".to_string(), timestamp_field(now_ms));
    fields
}

/// The body of a `records/modify` that writes one note.
///
/// **`update` with a `recordChangeTag`, never `forceUpdate`.** The tag is the
/// whole safety story for a note edited on two devices at once: the server
/// re-checks it and answers `CONFLICT` rather than overwriting, which is what
/// turns a silent last-writer-wins into the keep-both conflict copy the shared
/// reconciler already knows how to make. A `force*` operation would discard
/// the other device's edit with nothing anywhere to say so.
///
/// A create sends the same record with no tag and `operationType: "create"`,
/// which fails if the `recordName` already exists — the right answer, since a
/// name collision means the id was not ours to mint.
/// The fields the captured web client copies back VERBATIM on every note
/// update — icloud-md's `ECHOED_FIELDS` (`encodeNoteRecord.ts`), in the
/// captured order. Until 2026-08-26 this backend sent none of them; under
/// CloudKit's partial-update semantics an omitted field persists unchanged,
/// but conforming to the proven shape wholesale is the same discipline that
/// closed the `Folders`/`FoldersModificationDate` incident (gotcha #22) —
/// cherry-picking "which field can't matter" is how that one shipped broken.
const ECHOED_FIELDS: &[&str] = &[
    "MinimumSupportedNotesVersion",
    "Folders",
    "Deleted",
    "Folder",
    "CreationDate",
    "ReplicaIDToNotesVersionDataEncrypted",
    "FoldersModificationDate",
    "AttachmentViewType",
    "PaperStyleType",
    "ReplicaIDToUserIDEncrypted",
];

/// Sent as literal `null` values on a plain-note update (the captured client
/// echoes a non-null value only on attachment-carrying notes, which
/// `writability` refuses long before a write is built).
const NULL_FIELDS: &[&str] = &["FirstAttachmentThumbnail", "FirstAttachmentUTIEncrypted", "TextDataAsset"];

pub fn modify_note_body(w: &NoteWrite) -> Value {
    // Whether this update MOVES the note: the destination differs from the
    // folder the echoed record is filed in (or there is nothing to echo).
    // Unchanged folder → echo `Folder`/`Folders`/`FoldersModificationDate`
    // verbatim like every other echoed field; changed → write the relocation
    // trio fresh, the shape `folder_relocation_fields` proved live.
    let echoed_folder = w.echo.get("Folder").and_then(|v| v["recordName"].as_str());
    let moving = echoed_folder != Some(w.folder.as_str());

    // serde_json's Map is a BTreeMap, so the wire order is alphabetical
    // rather than the capture's order — JSON object order carries no meaning
    // and every write this backend has made was accepted that way. The SET
    // of fields is what conforms to the capture, not their order.
    let mut fields = serde_json::Map::new();
    match &w.change_tag {
        // ── update: icloud-md's `buildNoteUpdateFields` ──────────────────
        Some(_) => {
            fields.insert("ModificationDate".to_string(), timestamp_field(w.modified_ms));
            fields
                .insert(
                    "TitleEncrypted".to_string(),
                    bytes_field(derive_note_title(w.display_text.as_deref().unwrap_or(&w.text)).as_bytes()),
                );
            for name in ECHOED_FIELDS {
                if moving && ["Folder", "Folders", "FoldersModificationDate"].contains(name) {
                    continue;
                }
                if let Some(value) = w.echo.get(*name) {
                    fields.insert((*name).to_string(), json!({ "value": value }));
                }
            }
            if moving {
                fields.extend(folder_relocation_fields(&w.folder, w.modified_ms));
            }
            // An update against a record whose read carried no CreationDate
            // (never observed) still sends the one the caller carried.
            if !fields.contains_key("CreationDate") {
                if let Some(ms) = w.created_ms {
                    fields.insert("CreationDate".to_string(), timestamp_field(ms));
                }
            }
            fields.insert(
                "SnippetEncrypted".to_string(),
                bytes_field(derive_note_snippet(w.display_text.as_deref().unwrap_or(&w.text)).as_bytes()),
            );
            for name in NULL_FIELDS {
                let value = w.echo.get(*name).cloned().unwrap_or(Value::Null);
                fields.insert((*name).to_string(), json!({ "value": value }));
            }
            fields.insert("TextDataEncrypted".to_string(), bytes_field(&w.document));
        }
        // ── create: icloud-md's `buildNoteCreateFields` ──────────────────
        // Real date values (nothing exists to echo), both folder references,
        // derived display metadata, the placeholder trio as literal `{}`
        // (`{value: undefined}` in the capture serializes to exactly that),
        // and — matching the captured first-ever save — NO
        // `FoldersModificationDate`.
        None => {
            fields.insert(
                "CreationDate".to_string(),
                timestamp_field(w.created_ms.unwrap_or(w.modified_ms)),
            );
            fields.insert("Folders".to_string(), folders_field(&w.folder));
            fields.insert("Folder".to_string(), folder_field(&w.folder));
            fields.insert("ModificationDate".to_string(), timestamp_field(w.modified_ms));
            fields
                .insert(
                    "TitleEncrypted".to_string(),
                    bytes_field(derive_note_title(w.display_text.as_deref().unwrap_or(&w.text)).as_bytes()),
                );
            fields.insert(
                "SnippetEncrypted".to_string(),
                bytes_field(derive_note_snippet(w.display_text.as_deref().unwrap_or(&w.text)).as_bytes()),
            );
            for name in NULL_FIELDS {
                fields.insert((*name).to_string(), json!({}));
            }
            fields.insert("TextDataEncrypted".to_string(), bytes_field(&w.document));
        }
    }

    let fields = Value::Object(fields);
    let mut record = json!({
        "recordName": w.record_name,
        "recordType": "Note",
        "fields": fields,
    });
    let operation = match &w.change_tag {
        Some(tag) => {
            record["recordChangeTag"] = json!(tag);
            "update"
        }
        None => "create",
    };
    json!({
        "operations": [{ "operationType": operation, "record": record }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// The echo map a FUTURE update of this record should carry, given the write
/// that was just accepted — what a fresh read would hand [`write_base`].
/// One source of truth with [`modify_note_body`]: an update leaves every
/// echoed field as it was (they were echoed verbatim) except the folder trio
/// when the write moved the note, and writes the null trio to null; a create
/// stores the folder pair and dates and nothing else.
pub fn echo_after_write(w: &NoteWrite) -> serde_json::Map<String, Value> {
    let mut echo = w.echo.clone();
    match &w.change_tag {
        Some(_) => {
            let echoed_folder = w.echo.get("Folder").and_then(|v| v["recordName"].as_str());
            if echoed_folder != Some(w.folder.as_str()) {
                for (name, field) in folder_relocation_fields(&w.folder, w.modified_ms) {
                    echo.insert(name, field["value"].clone());
                }
            }
            for name in NULL_FIELDS {
                echo.remove(*name);
            }
        }
        None => {
            echo.insert("CreationDate".to_string(), json!(w.created_ms.unwrap_or(w.modified_ms)));
            echo.insert("Folder".to_string(), folder_reference(&w.folder));
            echo.insert("Folders".to_string(), json!([folder_reference(&w.folder)]));
        }
    }
    echo
}

/// The body of a `records/modify` that only relocates a note.
///
/// A move is a field write on the same record, not a separate endpoint — so an
/// ordinary content push carries the folder itself and the sync worker never
/// dispatches this (`SaveSemantics::InPlaceUpdateIncludingMove`). It exists for
/// the callers that relocate a note WITHOUT rewriting it: `Transport::move_note`
/// is a trait method with its own users, and writing the whole record there
/// would mean re-encoding a document nothing asked to change.
///
/// Writes `Folder`, `Folders` and `FoldersModificationDate` together — see
/// `folder_relocation_fields`. Until 2026-08-25 this wrote `Folder` alone,
/// which every write here has done since M2 shipped; a note relocated only
/// that way is the live incident this fix responds to.
pub fn move_note_body(record_name: &str, change_tag: Option<&str>, folder: &str, now_ms: i64) -> Value {
    let mut record = json!({
        "recordName": record_name,
        "recordType": "Note",
        "fields": Value::Object(folder_relocation_fields(folder, now_ms)),
    });
    if let Some(tag) = change_tag {
        record["recordChangeTag"] = json!(tag);
    }
    json!({
        "operations": [{ "operationType": "update", "record": record }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// Deleting a note **moves it to Apple's Trash**, and that is a deliberate
/// choice rather than the only mechanism.
///
/// `TrashFolder-CloudKit` is a real folder record, already excluded from every
/// listing (`build_folder_paths`, `decode_note`), and a note filed there is
/// what Apple shows as Recently Deleted — recoverable by the user, in Apple
/// Notes, for thirty days. Setting `Deleted = 1` would be a tombstone with no
/// way back. **Measured, not just argued (2026-08-24):** a note trashed
/// through Apple Notes.app and one trashed through this function, 19 seconds
/// apart, read back matching on `deleted`/`trashed`/`Folder`/`ModificationDate`
/// — the fields checked at the time. **That check did not cover `Folders`/
/// `FoldersModificationDate`, and a live incident the next day showed why it
/// should have**: a note this function trashed stayed fully live and
/// editable in Apple Notes.app for 18+ hours, and icloud-md's HAR-captured
/// record of Apple's own client shows it writes those two fields on every
/// relocation, this function never did before 2026-08-25. Apple's own client
/// does not set a tombstone; the recoverable shape is still not just the
/// safer guess, it is what Apple writes — but "no other field touched" was
/// never fully checked, and now isn't true.
///
/// `Capabilities::has_trash` stays **false** regardless: a Recently Deleted
/// view needs a restore button, restore needs the note's PREVIOUS folder, and
/// nothing measured says where CloudKit keeps it. False shows no view at all,
/// which is honest — and it warns the user more strongly than the mechanism
/// requires, which is the safe direction to be wrong in.
pub fn delete_note_body(record_name: &str, change_tag: Option<&str>, now_ms: i64) -> Value {
    move_note_body(record_name, change_tag, TRASH_FOLDER, now_ms)
}

/// The body of a `records/modify` that writes back a document **verbatim**.
///
/// This is the undo for a relocation that turned out to be destructive: if
/// CloudKit's `update` operation replaces the record rather than merging the
/// fields it was given, a move wipes `TextDataEncrypted`, and the only honest
/// repair is to put back the exact bytes that were read before the move.
///
/// **It composes nothing.** The bytes go out as they came in — no parse, no
/// splice, no re-encode — so it is not subject to any of `compose`'s six
/// refusals (gotcha #24). That is the whole reason it can exist on a backend
/// where `writes.notes` is false: restoring a document Jodd never understood
/// is not the same act as writing one it did.
pub fn restore_document_body(record_name: &str, change_tag: &str, document: &[u8]) -> Value {
    json!({
        "operations": [{
            "operationType": "update",
            "record": {
                "recordName": record_name,
                "recordType": "Note",
                "recordChangeTag": change_tag,
                "fields": { "TextDataEncrypted": bytes_field(document) },
            },
        }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// The body of a `records/modify` that creates one `Folder` record.
///
/// **A folder carries no document**, so none of the note write path's refusals
/// apply to it: the record is a title and a parent reference, and neither is
/// Apple's CRDT. `Capabilities::for_backend(ICloud).writes.folders` is still
/// false, and stays false until a live run says a folder created this way is
/// one Apple Notes displays — the point of this function is to let that run
/// happen, not to presume its answer.
///
/// `parent` is **`None` or a folder `recordName`, copied verbatim from a
/// folder that already sits where the new one should go** — never derived from
/// a Jodd path.
///
/// **Measured live, 2026-08-24, and it is why this argument is an option.**
/// The first folder Jodd created was given `ParentFolder =
/// DefaultFolder-CloudKit`, on the reading that the account root is what a
/// top-level folder hangs off. Apple Notes displayed it — the first write from
/// Jodd this account has ever honoured — **one level too deep**: inside the
/// `Notes` folder, while the folder it was meant to sit beside is a SIBLING of
/// `Notes`.
///
/// The cause is in the read model, not here. [`path_for`] gives a folder whose
/// parent is absent and a folder whose parent is the root the SAME path,
/// `Notes/<title>` — correct for M1, which only ever displayed the tree, and
/// ambiguous the moment something writes one back. Rather than pick a rule and
/// hope, the caller reads the field off a folder that is already in the right
/// place and reproduces it, present or absent: the same opaque-preservation
/// doctrine `compose` applies to a note's attribute runs.
pub fn create_folder_body(record_name: &str, title: &str, parent: Option<&str>) -> Value {
    let mut fields = json!({ "TitleEncrypted": bytes_field(title.as_bytes()) });
    if let Some(p) = parent {
        fields["ParentFolder"] = folder_field(p);
    }
    json!({
        "operations": [{
            "operationType": "create",
            "record": {
                "recordName": record_name,
                "recordType": "Folder",
                "fields": fields,
            },
        }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// The body of a `records/modify` that renames one `Folder` record.
///
/// Only `TitleEncrypted` goes out, so the folder's parent — and therefore
/// every path under it — is not restated by a rename.
pub fn rename_folder_body(record_name: &str, change_tag: &str, title: &str) -> Value {
    json!({
        "operations": [{
            "operationType": "update",
            "record": {
                "recordName": record_name,
                "recordType": "Folder",
                "recordChangeTag": change_tag,
                "fields": { "TitleEncrypted": bytes_field(title.as_bytes()) },
            },
        }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// The body of a `records/modify` that deletes one `Folder` record.
///
/// A folder delete is a real delete, not a move to the Trash: Apple's Trash is
/// itself a folder, and filing a folder inside it would nest a container the
/// read path excludes. The `recordChangeTag` goes along so a folder someone
/// changed on another device is refused rather than silently removed.
pub fn delete_folder_body(record_name: &str, change_tag: &str) -> Value {
    json!({
        "operations": [{
            "operationType": "delete",
            "record": {
                "recordName": record_name,
                "recordType": "Folder",
                "recordChangeTag": change_tag,
            },
        }],
        "zoneID": { "zoneName": ZONE_NAME },
        "atomic": true,
    })
}

/// A record's own `ModificationDate` in epoch ms, or `None` when absent.
///
/// **The recency signal a change feed does not otherwise give.** One record can
/// arrive on several pages of a `changes/zone` walk, and page order is not
/// documented as state order — so choosing the copy that came last is a guess
/// about CloudKit's paging. Apple's own timestamp on the record is a statement
/// about the record.
pub fn modification_ms(r: &Value) -> Option<i64> {
    r["fields"]["ModificationDate"]["value"].as_i64()
}

/// The record names in a `Folders` (plural) reference LIST, or `None` when the
/// field is absent.
///
/// Apple carries this alongside the singular `Folder` on a trashed note, and
/// on this account it sometimes names a folder that is not the Trash — which
/// is the only candidate anything has produced for "where did this note come
/// from". It is a list because CloudKit models it as one; nothing here assumes
/// how many entries mean what.
pub fn folders_plural(v: &Value) -> Option<Vec<String>> {
    let one = |x: &Value| x["recordName"].as_str().map(String::from);
    match &v["value"] {
        Value::Array(items) => Some(items.iter().filter_map(one).collect()),
        obj @ Value::Object(_) => one(obj).map(|s| vec![s]),
        _ => None,
    }
}

/// The `ParentFolder` a folder at `path` must carry, derived from a
/// convention **Apple's own client keeps and its server does not enforce**.
///
/// Notes.app will not let you create a FOLDER inside `Notes` — the option is
/// simply absent — so every folder a user makes is either a sibling of `Notes`
/// or nested under one of those siblings. **Notes themselves are a different
/// question and go in freely**: `Notes` is where a note lands by default and
/// held 584 of them on the account this was measured against. Nothing here
/// constrains where a NOTE may be filed; `folder_id_for` maps the path
/// `Notes` to the default folder exactly so a note can be written there. The census agrees without being
/// asked: 104 folders on a live account, `ParentFolder` absent on 11, **the
/// account root on 0**, another folder on 93 (2026-08-24).
///
/// **CloudKit allows it anyway, and Jodd proved that by accident**: the first
/// folder this code created was parented to the root, and Apple Notes
/// displayed it — inside `Notes`, a place its own UI cannot produce. So this
/// is not a server rule that would refuse a wrong answer. It is a shape no
/// Apple client makes, which means nothing about how Apple's clients handle it
/// has ever been exercised — the strongest possible argument for writing what
/// they write rather than what the API tolerates.
///
/// With that convention the ambiguity `path_for` opens closes in practice. On
/// the read side a folder with no parent and a folder parented to the root
/// both render as `Notes/<title>`; the second is a shape only a third-party
/// writer produces, so a path is enough to place a folder:
///
/// | path | parent |
/// |---|---|
/// | `Notes` | refused — that IS Apple's default folder, not a place to make one |
/// | `Notes/X` | **absent**, a sibling of `Notes` |
/// | `Notes/X/Y` | the folder at `Notes/X` |
///
/// Callers that have a folder already in the right place should still prefer
/// copying its field verbatim ([`parent_folder_of`]); this is for the ones
/// that do not, and for checking the copy against a rule.
pub fn parent_for_path(
    path: &str,
    folders: &[crate::backend::RemoteFolder],
) -> Result<Option<String>, String> {
    if path == ROOT_PATH {
        return Err(format!(
            "{ROOT_PATH:?} is Apple's own default folder — it already exists, and Notes.app \
             does not allow folders inside it"
        ));
    }
    let Some(parent_path) = path.rsplit_once('/').map(|(p, _)| p) else {
        return Err(format!(
            "{path:?} does not start with {ROOT_PATH:?}, and every folder path on this \
             backend does"
        ));
    };
    if parent_path == ROOT_PATH {
        return Ok(None);
    }
    folders
        .iter()
        .find(|f| f.path == parent_path)
        .map(|f| Some(f.id.clone()))
        .ok_or_else(|| format!("no folder named {parent_path:?} to put {path:?} inside"))
}

/// A folder record's `ParentFolder`, exactly as the zone carries it —
/// `None` when the field is absent.
///
/// `FolderRecord.parent` already holds this, but the write path needs it off a
/// raw record and needs the ABSENCE preserved rather than mapped onto a path;
/// see [`create_folder_body`] for what conflating the two cost.
pub fn parent_folder_of(r: &Value) -> Option<String> {
    r["fields"]["ParentFolder"]["value"]["recordName"].as_str().map(String::from)
}

/// The two fields a relocation diagnostic checks, read off whatever record it
/// is handed — a `changes/zone` record or a `records/modify` reply.
///
/// `None` means the record did not carry them, which on a reply is not a
/// failure: it says the server echoed only what it was sent, and the caller
/// has to go and read the zone instead. Distinguishing "the field is absent"
/// from "the field is wrong" is the whole point of returning an option here.
pub fn folder_and_document(r: &Value) -> Option<(String, Vec<u8>)> {
    let folder = r["fields"]["Folder"]["value"]["recordName"].as_str()?.to_string();
    let document = r["fields"]["TextDataEncrypted"]["value"]
        .as_str()
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())?;
    Some((folder, document))
}

/// Reads the few fields a future write needs off a record, as it goes past.
///
/// A separate pass over the zone to collect these would be a second whole-zone
/// read; a per-record fetch at push time (`records/lookup` — see
/// [`lookup_url`]) would be one extra round trip per keystroke-batch. Reading
/// them while the record is already in hand costs a few bytes per note and
/// no extra request at all.
pub fn write_base(r: &Value, locked: bool) -> super::WriteBase {
    let fields = &r["fields"];
    let document = fields["TextDataEncrypted"]["value"]
        .as_str()
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .unwrap_or_default();
    // The raw inner values a future update echoes back verbatim
    // (`modify_note_body`'s `ECHOED_FIELDS` + the null trio). Verbatim means
    // verbatim: nothing here is decoded, and an absent field stays absent
    // rather than defaulting — icloud-md's echo-if-present, which tolerates
    // a record missing any of them.
    let mut echo = serde_json::Map::new();
    for name in ECHOED_FIELDS.iter().chain(NULL_FIELDS) {
        let value = &fields[*name]["value"];
        if !value.is_null() {
            echo.insert((*name).to_string(), value.clone());
        }
    }
    super::WriteBase {
        document,
        change_tag: r["recordChangeTag"].as_str().unwrap_or_default().to_string(),
        created_ms: fields["CreationDate"]["value"].as_i64(),
        folder_id: fields["Folder"]["value"]["recordName"]
            .as_str()
            .unwrap_or(DEFAULT_FOLDER)
            .to_string(),
        title_field: decode_text_field(&fields["TitleEncrypted"]).unwrap_or_default(),
        locked,
        echo,
    }
}

/// A CloudKit timestamp as the string shape `notes.date` holds on every
/// backend. See [`decode_date_field`] for why it must not be a second format.
pub fn apple_date_from_ms(ms: i64) -> Option<String> {
    let utc = chrono::DateTime::from_timestamp_millis(ms)?;
    Some(crate::mime822::format_apple_date(utc.with_timezone(&chrono::Local)))
}

/// What one `records/modify` reply says about the record it was asked to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedRecord {
    pub record_name: String,
    /// The NEW `recordChangeTag` — the note's `version` from here on, and the
    /// baseline the next write's optimistic lock is checked against.
    pub change_tag: String,
    /// `ModificationDate` as the server stamped it, epoch ms.
    pub modified_ms: Option<i64>,
}

/// Maps CloudKit's own error vocabulary onto the shared retry policy's.
///
/// **A write failure arrives inside an HTTP 200**, the same way a rejected
/// `syncToken` does (`zone_error`) — so a transport that only classifies
/// statuses reports every one of these as a success. That is the shape this
/// function exists for.
///
/// `CONFLICT` is the one that matters: it is the optimistic lock firing, and
/// it must reach the reconciler as [`TransportError::Conflict`] so the note
/// becomes a keep-both conflict copy rather than an overwrite. Anything
/// unrecognised is `Permanent` — a write is not a read, and retrying an
/// unrecognised refusal every five seconds is gotcha #14's 5,816 attempts.
pub fn classify_modify_error(code: &str, reason: &str) -> TransportError {
    match code {
        "CONFLICT" => TransportError::Conflict { remote_etag: None },
        "AUTHENTICATION_REQUIRED" | "AUTHENTICATION_FAILED" | "ACCESS_DENIED" => {
            TransportError::Auth
        }
        "NOT_FOUND" | "UNKNOWN_ITEM" => TransportError::NotFound,
        "THROTTLED" => TransportError::RateLimited { retry_after: None },
        "TRY_AGAIN_LATER" | "INTERNAL_ERROR" | "SERVICE_UNAVAILABLE" | "ZONE_BUSY" => {
            TransportError::Transient { source: anyhow::anyhow!("CloudKit: {code}: {reason}") }
        }
        _ => TransportError::Permanent { source: anyhow::anyhow!("CloudKit: {code}: {reason}") },
    }
}

/// Reads a `records/modify` reply.
///
/// Checks for an error **before** looking for the saved record, for the same
/// reason `fetch_zone_page` checks `zone_error` first: a refused write carries
/// no usable record, so reading the record first turns a refusal into a
/// perfectly ordinary "nothing came back" and loses the reason.
pub fn decode_modify_reply(body: &Value) -> Result<SavedRecord, TransportError> {
    // Top level first — a request the server refused outright never reaches
    // the per-record array at all.
    if let Some(code) = body["serverErrorCode"].as_str() {
        return Err(classify_modify_error(code, body["reason"].as_str().unwrap_or("no reason given")));
    }
    let Some(record) = body["records"].as_array().and_then(|r| r.first()) else {
        return Err(TransportError::Permanent {
            source: anyhow::anyhow!("CloudKit accepted the write but returned no record"),
        });
    };
    if let Some(code) = record["serverErrorCode"].as_str() {
        return Err(classify_modify_error(
            code,
            record["reason"].as_str().unwrap_or("no reason given"),
        ));
    }
    let Some(name) = record_name(record) else {
        return Err(TransportError::Permanent {
            source: anyhow::anyhow!("CloudKit returned a record with no recordName"),
        });
    };
    Ok(SavedRecord {
        record_name: name.to_string(),
        change_tag: record["recordChangeTag"].as_str().unwrap_or_default().to_string(),
        modified_ms: record["fields"]["ModificationDate"]["value"].as_i64(),
    })
}

/// The `records/modify` URL for one account's partition.
pub fn modify_url(ck_host: &str, dsid: &str, client: &crate::icloud_auth::ClientConfig) -> String {
    format!(
        "{ck_host}{DATABASE_PATH}/records/modify\
         ?ckjsBuildVersion={CKJS_BUILD_VERSION}&ckjsVersion={CKJS_VERSION}\
         &clientId={}&clientBuildNumber={}&clientMasteringNumber={}&dsid={dsid}",
        client.client_id, client.client_build_number, client.client_mastering_number
    )
}

/// Posts one `records/modify` and reports what came back, **including the
/// server's own copy of the record it saved**.
///
/// That second half is not a convenience. This backend has no per-record
/// endpoint, so the only other way to see what a field holds after a write is
/// to walk the whole zone — thirty pages and about forty seconds on a real
/// account. Diagnostics that write a field and then check it use this instead,
/// and stop hammering `changes/zone` to answer a question the reply already
/// answered.
pub async fn post_modify_raw(
    http: &reqwest::Client,
    ck_host: &str,
    dsid: &str,
    client: &crate::icloud_auth::ClientConfig,
    cookie_header: &str,
    body: &Value,
) -> Result<(SavedRecord, Value), TransportError> {
    let resp = http
        .post(modify_url(ck_host, dsid, client))
        .header("Cookie", cookie_header)
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.icloud.com")
        .header("Referer", crate::icloud_auth::ICLOUD_URL)
        .header("Accept", "application/json")
        .json(body)
        .send()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        return Err(classify_status(status.as_u16(), retry_after));
    }
    let reply: Value = resp
        .json()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;
    let saved = decode_modify_reply(&reply)?;
    Ok((saved, reply["records"][0].clone()))
}

/// [`post_modify_raw`] with the server's own copy of the record dropped.
///
/// The record it discards is what a caller would otherwise spend a second
/// request (`records/lookup`, or a whole-zone re-walk) to see — so the
/// verbose form exists for the diagnostics that need to check a field they
/// just wrote for free, off the reply this call already made.
pub async fn post_modify(
    http: &reqwest::Client,
    ck_host: &str,
    dsid: &str,
    client: &crate::icloud_auth::ClientConfig,
    cookie_header: &str,
    body: &Value,
) -> Result<SavedRecord, TransportError> {
    post_modify_raw(http, ck_host, dsid, client, cookie_header, body).await.map(|(s, _)| s)
}

/// The `records/lookup` URL for one account's partition.
///
/// **This is the per-record endpoint the rest of this module's comments say
/// doesn't exist.** It does — icloud-md uses it to backfill shared-database
/// note bodies that `changes/zone` omits (`databaseClient.ts:lookupRecords`).
/// Jodd never called it before 2026-08-25: every read here goes through
/// `changes/zone`, a change feed, and every "confirm what we just wrote"
/// check either reads `records/modify`'s own reply or re-walks the whole
/// zone. Measured live the same day: a note moved and a note trashed both
/// read back correctly from `records/modify`'s reply AND from a subsequent
/// `changes/zone` walk, while Apple Notes.app and icloud.com still showed
/// the old state for both — raising the question this endpoint exists to
/// answer directly instead of by inference: does a point lookup see what the
/// feed sees, or what Apple's own clients see? `icloud_debug_record_lookup`
/// is the diagnostic that asks it.
pub fn lookup_url(ck_host: &str, dsid: &str, client: &crate::icloud_auth::ClientConfig) -> String {
    format!(
        "{ck_host}{DATABASE_PATH}/records/lookup\
         ?ckjsBuildVersion={CKJS_BUILD_VERSION}&ckjsVersion={CKJS_VERSION}\
         &clientId={}&clientBuildNumber={}&clientMasteringNumber={}&dsid={dsid}",
        client.client_id, client.client_build_number, client.client_mastering_number
    )
}

/// The body of a `records/lookup` for one or more records by name, in this
/// account's `Notes` zone. Shape matches icloud-md's own
/// (`{ records: [{recordName}, ...], zoneID }`) — no `operations` wrapper,
/// unlike `records/modify`; this is a read, not a write.
pub fn lookup_records_body(record_names: &[String]) -> Value {
    json!({
        "records": record_names.iter().map(|n| json!({ "recordName": n })).collect::<Vec<_>>(),
        "zoneID": { "zoneName": ZONE_NAME },
    })
}

/// Posts one `records/lookup` and returns whatever records came back, raw —
/// the caller decodes with the same `decode_note`/`is_deleted_record`/
/// `modification_ms` helpers a `changes/zone` page uses, so a lookup and a
/// feed page read identically once decoded and only their freshness can
/// differ.
///
/// A `recordName` the server has no record for is simply absent from the
/// reply's `records` array (CloudKit's `records/lookup` does not error per
/// missing name) — the caller must treat a name that doesn't come back as
/// "not found", not as a request failure.
pub async fn post_lookup(
    http: &reqwest::Client,
    ck_host: &str,
    dsid: &str,
    client: &crate::icloud_auth::ClientConfig,
    cookie_header: &str,
    record_names: &[String],
) -> Result<Vec<Value>, TransportError> {
    let resp = http
        .post(lookup_url(ck_host, dsid, client))
        .header("Cookie", cookie_header)
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.icloud.com")
        .header("Referer", crate::icloud_auth::ICLOUD_URL)
        .header("Accept", "application/json")
        .json(&lookup_records_body(record_names))
        .send()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        return Err(classify_status(status.as_u16(), retry_after));
    }
    let reply: Value = resp
        .json()
        .await
        .map_err(|e| TransportError::Transient { source: anyhow::anyhow!(e) })?;
    if let Some(code) = reply["serverErrorCode"].as_str() {
        return Err(classify_modify_error(code, reply["reason"].as_str().unwrap_or("no reason given")));
    }
    Ok(reply["records"].as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    // ── fixtures ────────────────────────────────────────────────────────
    //
    // Shaped like a real `changes/zone` response, down to the
    // `{"value": …}` field wrapper CloudKit puts around every value. Building
    // these by hand rather than pasting a captured response is deliberate:
    // a capture from one account carries that account's content, and these
    // have to be readable by whoever debugs them next.

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    }

    /// Title/snippet must derive from the DISPLAY text when the note carries
    /// inline objects — a raw U+FFFC in TitleEncrypted is a replacement
    /// glyph in Apple's own list view (the review's finding 2).
    #[test]
    fn title_and_snippet_derive_from_display_text_when_present() {
        let mut w = a_write();
        w.text = "tag \u{FFFC} first\nsecond".into();
        w.display_text = Some("tag #work first\nsecond".into());
        let body = modify_note_body(&w);
        let fields = &body["operations"][0]["record"]["fields"];
        let title_b64 = fields["TitleEncrypted"]["value"].as_str().unwrap();
        let title = String::from_utf8(
            base64::engine::general_purpose::STANDARD.decode(title_b64).unwrap(),
        )
        .unwrap();
        assert_eq!(title, "tag #work first");
        assert!(!title.contains('\u{FFFC}'));
    }

    /// A stale copy of a renamed tag arriving on a later page must not win —
    /// ModificationDate decides, same rule as the note dedup (gotcha #22).
    #[test]
    fn a_stale_inline_attachment_copy_never_beats_a_newer_one() {
        let rec = |ms: i64, alt: &str| {
            json!({ "recordName": "tag-1", "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64(alt) },
                "ModificationDate": { "value": ms },
            }})
        };
        // Newer first, stale second: the stale copy must not replace it.
        let refs = collect_inline_refs(&[rec(2_000, "#new"), rec(1_000, "#old")]);
        assert_eq!(refs["tag-1"].alt_text, "#new");
        // And in feed order: newer later also wins.
        let refs = collect_inline_refs(&[rec(1_000, "#old"), rec(2_000, "#new")]);
        assert_eq!(refs["tag-1"].alt_text, "#new");
    }

    #[test]
    fn inline_attachment_records_map_record_name_to_uti_and_alt_text() {
        let records = vec![
            json!({ "recordName": "tag-1", "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64("#work") },
            }}),
            // A second arrival of the same record on a later page must not
            // duplicate (gotcha #22).
            json!({ "recordName": "tag-1", "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
                "AltTextEncrypted": { "value": b64("#work") },
            }}),
            // A tombstoned record is skipped.
            json!({ "recordName": "tag-2", "recordType": "InlineAttachment",
                    "deleted": true, "fields": {} }),
            // A record missing its text renders nothing useful — skipped.
            json!({ "recordName": "tag-3", "recordType": "InlineAttachment", "fields": {
                "UTIEncrypted": { "value": b64("com.apple.notes.inlinetextattachment.hashtag") },
            }}),
            // Other record types are ignored.
            json!({ "recordName": "n1", "recordType": "Note", "fields": {} }),
        ];
        let refs = collect_inline_refs(&records);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs["tag-1"],
            InlineRef {
                type_uti: "com.apple.notes.inlinetextattachment.hashtag".into(),
                alt_text: "#work".into(),
            }
        );
    }

    /// A real note body: the protobuf nesting, gzip-compressed, base64'd —
    /// the same path a live record takes. gzip because that is what Notes.app
    /// writes, and 774 of 776 measured notes were gzip (gotcha #20).
    fn note_body(text: &str) -> String {
        use flate2::write::GzEncoder;
        use std::io::Write;
        use super::super::gen::{topotext, versioned_document};

        let inner = topotext::String { string: text.to_string(), ..Default::default() };
        let doc = versioned_document::Document {
            serialization_version: Some(1),
            version: vec![versioned_document::Version {
                serialization_version: Some(1),
                minimum_supported_version: Some(1),
                data: Some(inner.encode_to_vec()),
            }],
        };
        let mut e = GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&doc.encode_to_vec()).unwrap();
        base64::engine::general_purpose::STANDARD.encode(e.finish().unwrap())
    }

    fn folder(name: &str, title: &str, parent: Option<&str>) -> Value {
        let mut f = json!({ "TitleEncrypted": { "value": b64(title) } });
        if let Some(p) = parent {
            f["ParentFolder"] = json!({ "value": { "recordName": p } });
        }
        json!({ "recordName": name, "recordType": "Folder", "fields": f })
    }

    fn note(name: &str, title: &str, text: &str, folder_id: &str) -> Value {
        json!({
            "recordName": name,
            "recordType": "Note",
            "recordChangeTag": "3ab",
            "fields": {
                "TitleEncrypted": { "value": b64(title) },
                "TextDataEncrypted": { "value": note_body(text) },
                "Folder": { "value": { "recordName": folder_id } },
                "CreationDate": { "value": 1_600_000_000_000i64 },
                "ModificationDate": { "value": 1_700_000_000_000i64 },
            }
        })
    }

    fn paths(records: &[Value]) -> HashMap<String, String> {
        build_folder_paths(&folder_records(records))
    }

    // ── Component G: the folder tree ────────────────────────────────────

    #[test]
    fn the_default_folder_is_the_root_every_other_backend_calls_notes() {
        // This is what makes gotcha #9's five hardcoded "Notes" literals
        // correct on this backend instead of a limitation.
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None)];
        assert_eq!(paths(&recs).get(DEFAULT_FOLDER).unwrap(), "Notes");
    }

    #[test]
    fn nesting_is_real_and_five_levels_deep_works() {
        // Measured on a live account: 102 folders, 5 levels. Unlike Microsoft,
        // where the parent/child shape is unrecoverable at any price.
        let recs = vec![
            folder(DEFAULT_FOLDER, "Notes", None),
            folder("a", "Work", Some(DEFAULT_FOLDER)),
            folder("b", "2026", Some("a")),
            folder("c", "Q3", Some("b")),
            folder("d", "ATLAS", Some("c")),
        ];
        let p = paths(&recs);
        assert_eq!(p.get("a").unwrap(), "Notes/Work");
        assert_eq!(p.get("d").unwrap(), "Notes/Work/2026/Q3/ATLAS");
    }

    #[test]
    fn a_slash_in_a_folder_title_cannot_forge_a_path_segment() {
        // Measured: at least one real folder has a `/` in its name. Left
        // alone, "Q1/Q2" would read as a folder Q1 containing Q2, and two
        // unrelated trees could collide on one path.
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), folder("a", "Q1/Q2", Some(DEFAULT_FOLDER))];
        let path = paths(&recs).get("a").unwrap().clone();
        assert_eq!(path, "Notes/Q1\u{2215}Q2");
        assert_eq!(path.matches('/').count(), 1, "exactly one real separator: {path}");
    }

    #[test]
    fn the_trash_folder_and_everything_under_it_is_excluded() {
        let recs = vec![
            folder(DEFAULT_FOLDER, "Notes", None),
            folder(TRASH_FOLDER, "Recently Deleted", None),
            folder("a", "Old", Some(TRASH_FOLDER)),
        ];
        let p = paths(&recs);
        assert!(!p.contains_key(TRASH_FOLDER));
        assert!(!p.contains_key("a"), "a folder inside Trash must not appear in the tree");
    }

    #[test]
    fn a_cyclic_parent_chain_terminates_instead_of_spinning() {
        // The failure this cap exists for is not a wrong path — it is a hang,
        // which from outside looks like the sync never finishing.
        let recs = vec![
            folder(DEFAULT_FOLDER, "Notes", None),
            folder("a", "A", Some("b")),
            folder("b", "B", Some("a")),
        ];
        let p = paths(&recs);
        assert!(p.contains_key("a"), "a cyclic folder is kept, not dropped");
        assert!(p.get("a").unwrap().starts_with("Notes"));
    }

    #[test]
    fn a_folder_whose_parent_is_not_in_this_page_is_filed_under_the_root() {
        // Paging means a child can arrive before its parent. Dropping it would
        // take its notes with it; the next sync repairs the path.
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), folder("a", "Orphan", Some("missing"))];
        assert_eq!(paths(&recs).get("a").unwrap(), "Notes/Orphan");
    }

    #[test]
    fn an_empty_title_becomes_a_named_segment_rather_than_collapsing() {
        // An empty segment would make the folder share its parent's path, so
        // two siblings would silently become one.
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), folder("a", "   ", Some(DEFAULT_FOLDER))];
        assert_eq!(paths(&recs).get("a").unwrap(), "Notes/Untitled");
    }

    #[test]
    fn a_deleted_folder_is_not_in_the_tree() {
        let mut f = folder("a", "Gone", Some(DEFAULT_FOLDER));
        f["fields"]["Deleted"] = json!({ "value": 1 });
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), f];
        assert!(!paths(&recs).contains_key("a"));
    }

    #[test]
    fn thai_folder_titles_survive_the_path_join() {
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), folder("a", "บันทึกงาน", Some(DEFAULT_FOLDER))];
        assert_eq!(paths(&recs).get("a").unwrap(), "Notes/บันทึกงาน");
    }

    // ── Component D: note mapping ───────────────────────────────────────

    fn decode_one(r: &Value, p: &HashMap<String, String>) -> Note {
        match decode_note(r, p) {
            Decoded::Note(n) => *n,
            Decoded::Trashed(_) => panic!("expected a live note, got a trashed one"),
            Decoded::Skipped { record_name, reason } => {
                panic!("expected a note, got skip for {record_name}: {reason:?}")
            }
        }
    }

    /// A note in Recently Deleted is **recoverable**, which is a different fact
    /// from a tombstone — and M1 collapsed the two because neither appears in a
    /// listing and the difference bought nothing. It buys a restore button.
    #[test]
    fn a_note_in_the_trash_is_recoverable_and_a_tombstone_is_not() {
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None)];
        let p = paths(&recs);

        let r = note("n1", "Gone", "Gone\nbody", TRASH_FOLDER);
        match decode_note(&r, &p) {
            Decoded::Trashed(n) => {
                assert_eq!(n.title, "Gone");
                assert_eq!(
                    n.body_html, "<div>body</div>",
                    "decoded in full — the trash view previews it"
                );
            }
            other => panic!("a trashed note must be recoverable, got {other:?}"),
        }

        // `Deleted == 1` is a tombstone and stays one, wherever it is filed.
        let mut r = note("n2", "Really gone", "Really gone", DEFAULT_FOLDER);
        r["fields"]["Deleted"] = json!({ "value": 1 });
        match decode_note(&r, &p) {
            Decoded::Skipped { reason: SkipReason::Deleted, .. } => {}
            other => panic!("a tombstone is not recoverable, got {other:?}"),
        }
    }

    #[test]
    fn a_note_maps_onto_every_neutral_field() {
        let recs = vec![folder(DEFAULT_FOLDER, "Notes", None), folder("f1", "Work", Some(DEFAULT_FOLDER))];
        let p = paths(&recs);
        let r = note("f8bf619a-1b84-40eb-932d-6318ee9aeeb4", "Meeting", "Meeting\nfirst point", "f1");

        let n = decode_one(&r, &p);
        assert_eq!(n.id, "f8bf619a-1b84-40eb-932d-6318ee9aeeb4");
        assert_eq!(n.title, "Meeting");
        assert_eq!(n.label, "Notes/Work");
        assert_eq!(n.version, "3ab", "recordChangeTag is a real optimistic-lock token");
        assert_eq!(n.body_html, "<div>first point</div>", "the title line is cut");
        assert!(n.x_mail_created_date.is_some());
        assert!(n.attachments.is_empty(), "M1 does not fetch attachments");
    }

    #[test]
    fn the_uuid_is_the_record_name_verbatim_and_stays_lowercase() {
        // gotcha #18: a recordName parses as a UUID, so the shared save path
        // used to uppercase it into an id naming nothing. Pinned here as well
        // as at canonical_uuid_for, because this is where it enters the cache.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let name = "f8bf619a-1b84-40eb-932d-6318ee9aeeb4";
        let n = decode_one(&note(name, "t", "t", DEFAULT_FOLDER), &p);
        assert_eq!(n.uuid, name);
        assert_eq!(n.uuid, n.uuid.to_lowercase());
    }

    #[test]
    fn apples_own_pin_is_read() {
        // The first backend where notes.pinned can reflect Apple's pin rather
        // than being purely Jodd-local.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let mut r = note("n1", "t", "t", DEFAULT_FOLDER);
        assert!(!decode_one(&r, &p).pinned, "absent IsPinned is not pinned");

        r["fields"]["IsPinned"] = json!({ "value": 1 });
        assert!(decode_one(&r, &p).pinned, "CloudKit's integer form");

        r["fields"]["IsPinned"] = json!({ "value": true });
        assert!(decode_one(&r, &p).pinned, "and its boolean form");
    }

    #[test]
    fn a_deleted_note_is_skipped() {
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let mut r = note("n1", "t", "t", DEFAULT_FOLDER);
        r["fields"]["Deleted"] = json!({ "value": 1 });
        assert!(matches!(
            decode_note(&r, &p),
            Decoded::Skipped { reason: SkipReason::Deleted, .. }
        ));
    }

    #[test]
    fn a_note_in_the_trash_never_comes_back_as_a_live_one() {
        // The count Apple shows never includes Recently Deleted, so a trashed
        // note must not reach a listing under some path — it goes to the trash
        // view instead, which is a different thing from being skipped.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let r = note("n1", "t", "t", TRASH_FOLDER);
        assert!(matches!(decode_note(&r, &p), Decoded::Trashed(_)));
    }

    #[test]
    fn an_undecodable_body_carries_the_decode_error_split_intact() {
        // The ADP verdict is built on this split and must never re-derive
        // compression magic of its own. Unreadable is the ADP shape;
        // Malformed is a bug or a schema change and must not count toward it.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let mut r = note("n1", "t", "t", DEFAULT_FOLDER);
        r["fields"]["TextDataEncrypted"] = json!({ "value": b64("not a compressed stream at all") });
        match decode_note(&r, &p) {
            Decoded::Skipped { reason: SkipReason::Undecodable(e), .. } => {
                assert!(matches!(e, super::super::doc::DecodeError::Unreadable(_)), "got {e:?}")
            }
            other => panic!("expected Undecodable, got {other:?}"),
        }
    }

    #[test]
    fn a_body_that_is_not_even_base64_is_incomplete_not_undecodable() {
        // The distinction matters because Undecodable feeds the ADP verdict.
        // A malformed response accusing the user's account of being
        // end-to-end encrypted is a false accusation they cannot act on.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let mut r = note("n1", "t", "t", DEFAULT_FOLDER);
        r["fields"]["TextDataEncrypted"] = json!({ "value": "!!! not base64 !!!" });
        assert!(matches!(
            decode_note(&r, &p),
            Decoded::Skipped { reason: SkipReason::Incomplete(_), .. }
        ));
    }

    #[test]
    fn a_record_with_no_body_field_is_incomplete() {
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let mut r = note("n1", "t", "t", DEFAULT_FOLDER);
        r["fields"]["TextDataEncrypted"] = json!(null);
        assert!(matches!(
            decode_note(&r, &p),
            Decoded::Skipped { reason: SkipReason::Incomplete(_), .. }
        ));
    }

    #[test]
    fn a_note_whose_folder_is_not_in_this_page_lands_at_the_root_not_nowhere() {
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let n = decode_one(&note("n1", "t", "t", "unknown-folder"), &p);
        assert_eq!(n.label, ROOT_PATH);
    }

    #[test]
    fn a_note_that_is_only_a_title_decodes_to_an_empty_body() {
        // 41 of 776 measured notes. Correct, not corruption — and it is also
        // gotcha #17's damage signature, so damage detection must not confuse
        // the two.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let n = decode_one(&note("n1", "Just a title", "Just a title", DEFAULT_FOLDER), &p);
        assert_eq!(n.body_html, "");
        assert_eq!(n.title, "Just a title");
    }

    #[test]
    fn a_truncated_title_still_has_its_line_cut_from_the_body() {
        // gotcha #21: TitleEncrypted is a lossy derivation, so the cut is by
        // POSITION and the title only verifies. 215 of 776 first lines differ.
        let p = paths(&[folder(DEFAULT_FOLDER, "Notes", None)]);
        let line = "A very long first line that Apple decided to shorten when it built the title field";
        let title = format!("{}\u{2026}", &line[..65]);
        let r = note("n1", &title, &format!("{line}\nbody below"), DEFAULT_FOLDER);
        assert_eq!(decode_one(&r, &p).body_html, "<div>body below</div>");
    }

    #[test]
    fn dates_use_the_same_format_every_other_backend_stores() {
        // notes.date is sorted and compared across backends; a second format
        // in that column makes an iCloud note sort against a Gmail note by luck.
        let got = decode_date_field(&json!({ "value": 1_700_000_000_000i64 })).unwrap();
        let want = crate::mime822::format_apple_date(
            chrono::DateTime::from_timestamp_millis(1_700_000_000_000)
                .unwrap()
                .with_timezone(&chrono::Local),
        );
        assert_eq!(got, want);
        assert!(decode_date_field(&json!({ "value": null })).is_none());
    }

    #[test]
    fn a_base64_text_field_is_plain_text_not_a_cipher() {
        // "Encrypted" is Apple's at-rest naming. Reading it literally is how
        // someone concludes this backend needs a key it does not have.
        assert_eq!(decode_text_field(&json!({ "value": b64("บันทึก") })).unwrap(), "บันทึก");
        assert!(decode_text_field(&json!({})).is_none());
    }

    // ── the zone envelope and request ───────────────────────────────────

    #[test]
    fn a_zone_page_yields_its_records_token_and_more_flag() {
        let body = json!({
            "zones": [{
                "records": [note("n1", "t", "t", DEFAULT_FOLDER)],
                "syncToken": "TOKEN123",
                "moreComing": true
            }]
        });
        let page = decode_zone_page(&body);
        assert_eq!(page.records.len(), 1);
        assert_eq!(page.sync_token.as_deref(), Some("TOKEN123"));
        assert!(page.more_coming);
    }

    #[test]
    fn an_empty_or_unexpected_zone_response_decodes_to_nothing_rather_than_panicking() {
        let page = decode_zone_page(&json!({}));
        assert!(page.records.is_empty());
        assert!(page.sync_token.is_none());
        assert!(!page.more_coming, "no moreComing must not read as more coming");
    }

    #[test]
    fn the_request_carries_a_sync_token_only_when_resuming() {
        let fresh = zone_request_body(None);
        assert!(fresh["zones"][0]["syncToken"].is_null(), "a first sync sends no token");
        assert_eq!(fresh["zones"][0]["zoneID"]["zoneName"], "Notes");
        assert_eq!(fresh["zones"][0]["reverse"], json!(true));

        let resumed = zone_request_body(Some("TOKEN123"));
        assert_eq!(resumed["zones"][0]["syncToken"], "TOKEN123");
    }

    #[test]
    fn the_request_asks_for_what_apples_own_client_asks_for() {
        // Deliberate: this is a private API, and a request whose shape does not
        // match the only client Apple expects is a difference a server may act
        // on. Narrowing to the five keys M1 reads would be the tidy mistake.
        let keys = zone_request_body(None)["zones"][0]["desiredKeys"].clone();
        let keys = keys.as_array().unwrap();
        assert!(keys.iter().any(|k| k == "TextDataEncrypted"));
        assert!(keys.iter().any(|k| k == "MergeableDataEncrypted"), "unused in M1, sent anyway");
        assert!(keys.len() > 20, "the full web-client set, not a subset: {}", keys.len());
    }

    #[test]
    fn the_url_carries_the_per_account_partition_and_every_client_parameter() {
        let client = crate::icloud_auth::ClientConfig {
            client_build_number: "2628Build44".into(),
            client_mastering_number: "2628B36".into(),
            client_id: "ABC".into(),
        };
        let url = zone_url("https://p149-ckdatabasews.icloud.com:443", "12345", &client);
        assert!(url.starts_with("https://p149-ckdatabasews.icloud.com:443/database/1/com.apple.notes/"));
        assert!(url.contains("dsid=12345"));
        assert!(url.contains("clientBuildNumber=2628Build44"));
        assert!(url.contains("ckjsVersion="), "the one pair that cannot be read from the session");
    }

    #[test]
    fn a_dead_session_is_auth_not_transient() {
        // 421 is CloudKit's "this session is over". The jar rotates on its own,
        // so retrying with the same cookies can only fail — the revival path is
        // what fixes it, and calling this Transient would hide that behind a
        // backoff loop forever.
        assert!(matches!(classify_status(421, None), TransportError::Auth));
        assert!(matches!(classify_status(401, None), TransportError::Auth));
        assert!(matches!(classify_status(429, None), TransportError::RateLimited { .. }));
        assert!(matches!(classify_status(503, None), TransportError::Transient { .. }));
        assert!(matches!(classify_status(404, None), TransportError::NotFound));
        assert!(matches!(classify_status(400, None), TransportError::Permanent { .. }));
    }

    #[tokio::test]
    async fn fetch_zone_page_posts_the_session_and_decodes_the_reply() {
        let mut server = mockito::Server::new_async().await;
        let m = server
            .mock("POST", mockito::Matcher::Regex(".*changes/zone.*".into()))
            .match_header("Cookie", mockito::Matcher::Regex("X-APPLE-WEBAUTH-TOKEN".into()))
            .match_body(mockito::Matcher::PartialJson(json!({ "zones": [{ "zoneID": { "zoneName": "Notes" } }] })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "zones": [{ "records": [], "syncToken": "T1", "moreComing": false }] }).to_string())
            .create_async()
            .await;

        let client = crate::icloud_auth::ClientConfig {
            client_build_number: "b".into(),
            client_mastering_number: "m".into(),
            client_id: "c".into(),
        };
        let reply = fetch_zone_page(
            &reqwest::Client::new(),
            &server.url(),
            "12345",
            &client,
            "X-APPLE-WEBAUTH-TOKEN=x",
            None,
        )
        .await
        .expect("a 200 must decode");

        m.assert_async().await;
        match reply {
            ZoneReply::Page(p) => assert_eq!(p.sync_token.as_deref(), Some("T1")),
            other => panic!("expected a page, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rejected_sync_token_arrives_inside_a_200_and_is_not_an_error() {
        // The whole reason this case exists: nothing about the transport
        // failed, so a TransportError would be a lie — and treating it as one
        // aborts a sync that a single from-scratch walk would have completed.
        let mut server = mockito::Server::new_async().await;
        let _m = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({ "zones": [{ "serverErrorCode": "BAD_REQUEST", "reason": "invalid sync token" }] })
                    .to_string(),
            )
            .create_async()
            .await;

        let client = crate::icloud_auth::ClientConfig {
            client_build_number: "b".into(),
            client_mastering_number: "m".into(),
            client_id: "c".into(),
        };
        let reply = fetch_zone_page(&reqwest::Client::new(), &server.url(), "1", &client, "a=b", Some("STALE"))
            .await
            .expect("a zone error is still HTTP 200");
        match reply {
            ZoneReply::TokenRejected(reason) => {
                assert!(reason.contains("BAD_REQUEST"), "the log must say why: {reason}");
                assert!(reason.contains("invalid sync token"), "including the server's reason: {reason}");
            }
            other => panic!("expected TokenRejected, got {other:?}"),
        }
    }

    #[test]
    fn an_ordinary_page_carries_no_zone_error() {
        assert!(zone_error(&json!({ "zones": [{ "records": [], "moreComing": false }] })).is_none());
        assert!(zone_error(&json!({})).is_none());
    }

    #[test]
    fn a_zone_error_without_a_reason_still_reports_its_code() {
        let got = zone_error(&json!({ "zones": [{ "serverErrorCode": "INTERNAL_ERROR" }] })).unwrap();
        assert!(got.contains("INTERNAL_ERROR"), "got {got}");
    }

    #[tokio::test]
    async fn an_expired_session_surfaces_as_auth_so_the_revival_path_can_run() {
        let mut server = mockito::Server::new_async().await;
        let _m = server.mock("POST", mockito::Matcher::Any).with_status(421).create_async().await;

        let client = crate::icloud_auth::ClientConfig {
            client_build_number: "b".into(),
            client_mastering_number: "m".into(),
            client_id: "c".into(),
        };
        let err = fetch_zone_page(&reqwest::Client::new(), &server.url(), "1", &client, "a=b", None)
            .await
            .expect_err("421 must not be a success");
        assert!(matches!(err, TransportError::Auth), "got {err:?}");
    }

    #[test]
    fn the_pin_is_read_from_the_per_user_record_not_the_note() {
        // Measured shape: IsPinned appears on zero Note records and the zone
        // carries a separate *_UserSpecific record per note the user has
        // touched. A pin read off the note itself is always false.
        let recs = vec![
            json!({
                "recordName": "us1",
                "recordType": "Note_UserSpecific",
                "fields": {
                    "Note": { "value": { "recordName": "n1" } },
                    "IsPinned": { "value": 1 },
                }
            }),
            // A per-user record that exists for some other reason (last
            // viewed, say) and is not a pin.
            json!({
                "recordName": "us2",
                "recordType": "Note_UserSpecific",
                "fields": {
                    "Note": { "value": { "recordName": "n2" } },
                    "IsPinned": { "value": 0 },
                }
            }),
            // A locked note can be pinned too, and its per-user record is a
            // different type.
            json!({
                "recordName": "us3",
                "recordType": "PasswordProtectedNote_UserSpecific",
                "fields": {
                    "Note": { "value": { "recordName": "n3" } },
                    "IsPinned": { "value": true },
                }
            }),
            // No way back to a note. Counted, never silently dropped — if
            // Apple's field names differ this is the number that says so.
            json!({
                "recordName": "us4",
                "recordType": "Note_UserSpecific",
                "fields": { "IsPinned": { "value": 1 } }
            }),
            // Not a per-user record at all.
            json!({ "recordName": "n1", "recordType": "Note", "fields": {} }),
        ];

        let scan = collect_pins(&recs);
        assert_eq!(scan.seen, 4, "only the per-user records");
        assert_eq!(scan.deleted, 0, "none of these are tombstones");
        assert_eq!(scan.unjoinable, 1);
        assert!(scan.pinned.contains("n1"), "numeric 1 is pinned");
        assert!(!scan.pinned.contains("n2"), "numeric 0 is not");
        assert!(scan.pinned.contains("n3"), "a JSON bool works too");
        assert_eq!(scan.pinned.len(), 2);
    }


    #[test]
    fn a_tombstoned_per_user_record_does_not_pin_the_note() {
        // The per-user record has its own `Deleted` field, and a dead one
        // still carries whatever `IsPinned` it had when it died. Reading it
        // pins notes the user has unpinned: measured 2026-08-23, the account
        // reported 17 pinned where Apple showed 2.
        let recs = vec![
            json!({
                "recordName": "us1",
                "recordType": "Note_UserSpecific",
                "fields": {
                    "Note": { "value": { "recordName": "n1" } },
                    "IsPinned": { "value": 1 },
                    "Deleted": { "value": 1 },
                }
            }),
            json!({
                "recordName": "us2",
                "recordType": "Note_UserSpecific",
                "fields": {
                    "Note": { "value": { "recordName": "n2" } },
                    "IsPinned": { "value": 1 },
                    "Deleted": { "value": 0 },
                }
            }),
        ];

        let scan = collect_pins(&recs);
        assert_eq!(scan.deleted, 1);
        assert!(!scan.pinned.contains("n1"), "a tombstone's pin is stale state");
        assert!(scan.pinned.contains("n2"));
        assert_eq!(scan.pinned.len(), 1);
    }


    // ── M2: records/modify ──────────────────────────────────────────────

    fn a_write() -> NoteWrite {
        NoteWrite {
            display_text: None,
            record_name: "f8bf619a-1b84-40eb-932d-6318ee9aeeb4".into(),
            change_tag: Some("tag7".into()),
            text: "Groceries\nmilk and eggs".into(),
            document: vec![0x1f, 0x8b, 0x01, 0x02],
            folder: DEFAULT_FOLDER.into(),
            created_ms: Some(1_600_000_000_000),
            modified_ms: 1_700_000_000_000,
            echo: serde_json::Map::new(),
        }
    }

    /// The optimistic lock is the whole safety story for a note edited on two
    /// devices at once, and it only exists if the tag actually goes out with
    /// an operation the server re-checks.
    #[test]
    fn an_update_carries_the_change_tag_and_is_never_a_force() {
        let b = modify_note_body(&a_write());
        let op = &b["operations"][0];
        assert_eq!(op["operationType"], json!("update"));
        assert_eq!(op["record"]["recordChangeTag"], json!("tag7"));
        assert!(
            !op["operationType"].as_str().unwrap().contains("force"),
            "a force* operation discards the other device's edit with nothing to say so"
        );
    }

    #[test]
    fn a_create_sends_no_tag_and_asks_for_a_create() {
        let mut w = a_write();
        w.change_tag = None;
        let b = modify_note_body(&w);
        assert_eq!(b["operations"][0]["operationType"], json!("create"));
        assert!(b["operations"][0]["record"]["recordChangeTag"].is_null());
    }

    /// `TitleEncrypted` is base64 of PLAIN TEXT — "Encrypted" describes
    /// Apple's server-side at-rest encryption, not anything a client does
    /// (gotcha #20's naming trap). Writing a cipher here would produce a note
    /// whose title is gibberish on every Apple device.
    #[test]
    fn the_title_goes_out_as_base64_of_plain_text() {
        let b = modify_note_body(&a_write());
        let v = b["operations"][0]["record"]["fields"]["TitleEncrypted"]["value"].as_str().unwrap();
        assert_eq!(
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(v).unwrap()).unwrap(),
            "Groceries"
        );
    }

    #[test]
    fn the_document_goes_out_as_base64_bytes_and_the_folder_as_a_reference() {
        let b = modify_note_body(&a_write());
        let f = &b["operations"][0]["record"]["fields"];
        assert_eq!(f["TextDataEncrypted"]["type"], json!("BYTES"));
        assert_eq!(
            f["TextDataEncrypted"]["value"],
            json!(base64::engine::general_purpose::STANDARD.encode([0x1f, 0x8b, 0x01, 0x02]))
        );
        assert_eq!(f["Folder"]["type"], json!("REFERENCE"));
        assert_eq!(f["Folder"]["value"]["recordName"], json!(DEFAULT_FOLDER));
    }

    /// An edit must not rewrite the note's creation time — Apple sorts on it,
    /// and a note that jumps to the top of "Date Created" on every save is a
    /// visible corruption of the user's own ordering.
    #[test]
    fn an_edit_carries_the_creation_date_it_was_given_and_omits_it_when_there_is_none() {
        let f = &modify_note_body(&a_write())["operations"][0]["record"]["fields"];
        assert_eq!(f["CreationDate"]["value"], json!(1_600_000_000_000i64));

        let mut w = a_write();
        w.created_ms = None;
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        assert!(f["CreationDate"].is_null(), "no invented creation date");
    }

    /// The 76-unit word-boundary truncation the captured client applies —
    /// icloud-md's `deriveNoteTitle`, whose limit was reverse-engineered from
    /// a capture (a 255-char first line cut to 66 chars at the last space
    /// before 76).
    #[test]
    fn the_title_is_derived_from_the_first_line_with_the_captured_truncation() {
        assert_eq!(derive_note_title("short title\nbody"), "short title");
        // 8-char words: "wordwor " * N — the last space at or before 76.
        let long = "abcdefg ".repeat(12); // 96 units, spaces at 7,15,...,71,79
        let line = format!("{long}tail\nbody");
        assert_eq!(derive_note_title(&line), &line[..71], "cut at the last space before 76");
        // No space at all: a hard cut at 76 units.
        let unbroken: String = "x".repeat(100);
        assert_eq!(derive_note_title(&unbroken), &unbroken[..76]);
    }

    /// The snippet is the text past the (possibly truncated) title, leading
    /// whitespace stripped, first line only — and the captured client stores
    /// a literal placeholder when there is nothing, which is exactly the
    /// "No additional text" every single-line note shows in Apple's list.
    #[test]
    fn the_snippet_is_derived_and_single_line_notes_get_the_captured_placeholder() {
        assert_eq!(derive_note_snippet("title\nsecond line\nthird"), "second line");
        assert_eq!(derive_note_snippet("title only"), "No additional text");
        assert_eq!(derive_note_snippet("title\n\n\nlate body"), "late body");
        let f = &modify_note_body(&a_write())["operations"][0]["record"]["fields"];
        let v = f["SnippetEncrypted"]["value"].as_str().unwrap();
        assert_eq!(
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(v).unwrap()).unwrap(),
            "milk and eggs"
        );
    }

    /// The echoed fields — icloud-md's `ECHOED_FIELDS`, copied back verbatim
    /// on every captured update. `ReplicaIDToNotesVersionDataEncrypted` is
    /// the load-bearing one: a per-replica version map Apple's merge
    /// machinery consults, and dropping it is the leading suspect for the
    /// 2026-08-26 duplicated-note merge fallback.
    #[test]
    fn an_update_echoes_the_captured_field_set_verbatim() {
        let mut w = a_write();
        w.echo.insert("ReplicaIDToNotesVersionDataEncrypted".into(), json!("b64blob=="));
        w.echo.insert("ReplicaIDToUserIDEncrypted".into(), json!("b64user=="));
        w.echo.insert("MinimumSupportedNotesVersion".into(), json!(0));
        w.echo.insert("Deleted".into(), json!(0));
        w.echo.insert("PaperStyleType".into(), json!(2));
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        assert_eq!(f["ReplicaIDToNotesVersionDataEncrypted"]["value"], json!("b64blob=="));
        assert_eq!(f["ReplicaIDToUserIDEncrypted"]["value"], json!("b64user=="));
        assert_eq!(f["MinimumSupportedNotesVersion"]["value"], json!(0));
        assert_eq!(f["Deleted"]["value"], json!(0));
        assert_eq!(f["PaperStyleType"]["value"], json!(2));
        // Absent from the echo → absent from the write (echo-if-present).
        assert!(f["AttachmentViewType"].is_null());
    }

    /// The placeholder trio goes out as literal nulls on an update — the
    /// captured client sends `{"value": null}` for a plain note — and as
    /// literal `{}` on a create.
    #[test]
    fn the_attachment_trio_is_null_on_update_and_empty_on_create() {
        let f = &modify_note_body(&a_write())["operations"][0]["record"]["fields"];
        for name in ["FirstAttachmentThumbnail", "FirstAttachmentUTIEncrypted", "TextDataAsset"] {
            assert!(!f[name].is_null(), "{name} must be sent");
            assert!(f[name]["value"].is_null(), "{name} must carry a null value");
        }
        let mut w = a_write();
        w.change_tag = None;
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        for name in ["FirstAttachmentThumbnail", "FirstAttachmentUTIEncrypted", "TextDataAsset"] {
            assert_eq!(f[name], json!({}), "{name} must be a literal empty object on a create");
        }
    }

    /// An update that does NOT move the note echoes the folder trio verbatim
    /// (whatever zone identity the read returned included); one that DOES
    /// move writes the relocation trio fresh. The distinction is what keeps
    /// an ordinary edit from restamping `FoldersModificationDate`.
    #[test]
    fn an_update_echoes_the_folder_when_staying_and_relocates_when_moving() {
        let mut w = a_write();
        w.echo.insert("Folder".into(), json!({"recordName": DEFAULT_FOLDER, "action": "VALIDATE", "zoneID": {"zoneName": "Notes", "ownerRecordName": "_abc"}}));
        w.echo.insert("Folders".into(), json!([{"recordName": DEFAULT_FOLDER, "action": "VALIDATE"}]));
        w.echo.insert("FoldersModificationDate".into(), json!(1_650_000_000_000i64));
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        assert_eq!(
            f["Folder"]["value"]["zoneID"]["ownerRecordName"],
            json!("_abc"),
            "an unmoved folder is echoed verbatim, zone identity included"
        );
        assert_eq!(f["FoldersModificationDate"]["value"], json!(1_650_000_000_000i64));

        w.folder = "f9".into();
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!("f9"));
        assert_eq!(f["Folders"]["value"][0]["recordName"], json!("f9"));
        assert_eq!(f["FoldersModificationDate"]["value"], json!(1_700_000_000_000i64));
    }

    /// The captured first-ever save (`buildNoteCreateFields`) carries no
    /// `FoldersModificationDate` — a create is not a relocation.
    #[test]
    fn a_create_writes_both_folder_fields_but_no_folders_modification_date() {
        let mut w = a_write();
        w.change_tag = None;
        let f = &modify_note_body(&w)["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!(DEFAULT_FOLDER));
        assert_eq!(f["Folders"]["value"][0]["recordName"], json!(DEFAULT_FOLDER));
        assert!(f["FoldersModificationDate"].is_null());
        let v = f["SnippetEncrypted"]["value"].as_str().unwrap();
        assert_eq!(
            String::from_utf8(base64::engine::general_purpose::STANDARD.decode(v).unwrap()).unwrap(),
            "milk and eggs"
        );
    }

    /// `write_base` collects the echo set off a record and `echo_after_write`
    /// keeps it honest across Jodd's own write — the cached base a SECOND
    /// edit builds from must describe what the server now holds.
    #[test]
    fn the_echo_set_survives_a_read_and_jodds_own_write() {
        let mut rec = note("A1", "t", "t\nbody", DEFAULT_FOLDER);
        rec["fields"]["ReplicaIDToNotesVersionDataEncrypted"] = json!({"value": "blob==", "type": "ENCRYPTED_BYTES"});
        rec["fields"]["PaperStyleType"] = json!({"value": 1, "type": "INT64"});
        let base = write_base(&rec, false);
        assert_eq!(base.echo["ReplicaIDToNotesVersionDataEncrypted"], json!("blob=="));
        assert_eq!(base.echo["PaperStyleType"], json!(1));

        let mut w = a_write();
        w.echo = base.echo.clone();
        let after = echo_after_write(&w);
        assert_eq!(
            after["ReplicaIDToNotesVersionDataEncrypted"],
            json!("blob=="),
            "an echoed field is unchanged by an update that echoed it"
        );
    }

    /// A move is a field write on the same record — no separate endpoint, and
    /// no content field touched. It now writes THREE folder fields together
    /// (`Folder`, `Folders`, `FoldersModificationDate`) — matching Apple's own
    /// client (icloud-md's HAR-captured `buildNoteRelocationFields`) instead
    /// of `Folder` alone, which is what this backend wrote until 2026-08-25
    /// and which a live incident showed Apple's own clients can go on
    /// ignoring for 18+ hours.
    #[test]
    fn a_move_writes_folder_and_folders_together() {
        let b = move_note_body("n1", Some("tag1"), "f9", 1_700_000_000_000);
        let f = &b["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!("f9"));
        assert_eq!(f["Folder"]["value"]["action"], json!("VALIDATE"));
        assert_eq!(f["Folders"]["type"], json!("REFERENCE_LIST"));
        assert_eq!(f["Folders"]["value"][0]["recordName"], json!("f9"));
        assert_eq!(f["Folders"]["value"][0]["action"], json!("VALIDATE"));
        assert_eq!(f["FoldersModificationDate"]["value"], json!(1_700_000_000_000i64));
        assert!(f["TextDataEncrypted"].is_null(), "a move must not rewrite the body");
        assert!(f["TitleEncrypted"].is_null(), "a move must not rewrite the title");
    }

    /// Delete files the note in Apple's own Recently Deleted rather than
    /// tombstoning it: recoverable beats irreversible when both look the same
    /// from here.
    #[test]
    fn a_delete_files_the_note_in_the_trash_folder_rather_than_tombstoning_it() {
        let b = delete_note_body("n1", Some("tag1"), 1_700_000_000_000);
        let f = &b["operations"][0]["record"]["fields"];
        assert_eq!(f["Folder"]["value"]["recordName"], json!(TRASH_FOLDER));
        assert_eq!(f["Folders"]["value"][0]["recordName"], json!(TRASH_FOLDER));
        assert!(f["Deleted"].is_null(), "a tombstone has no way back; the Trash does");
    }

    /// `Folders` is a reference LIST on the wire and CloudKit renders a
    /// one-element list and a lone reference differently. Reading only the
    /// array shape would report "absent" for the single-entry case, which is
    /// the case the trash question turns on.
    #[test]
    fn folders_plural_reads_both_a_list_and_a_lone_reference() {
        let list = json!({ "value": [
            { "recordName": "f1" }, { "recordName": TRASH_FOLDER },
        ]});
        assert_eq!(
            folders_plural(&list),
            Some(vec!["f1".to_string(), TRASH_FOLDER.to_string()])
        );
        let lone = json!({ "value": { "recordName": "f1" } });
        assert_eq!(folders_plural(&lone), Some(vec!["f1".to_string()]));
        assert_eq!(folders_plural(&json!({})), None);
    }

    /// The rule Apple's UI enforces, and the reason a path is enough: nothing
    /// can be a child of the default folder, so "absent parent" and "parented
    /// to the root" are not two readings of one path — the second never
    /// happens.
    #[test]
    fn a_path_places_a_folder_because_notes_cannot_have_children() {
        let folders = vec![
            crate::backend::RemoteFolder { id: "top".into(), path: "Notes/Top".into() },
            crate::backend::RemoteFolder { id: "mid".into(), path: "Notes/Top/Mid".into() },
        ];
        // A sibling of `Notes` carries no parent at all.
        assert_eq!(parent_for_path("Notes/Anything", &folders), Ok(None));
        // Anything deeper hangs off the folder above it.
        assert_eq!(parent_for_path("Notes/Top/New", &folders), Ok(Some("top".into())));
        assert_eq!(parent_for_path("Notes/Top/Mid/New", &folders), Ok(Some("mid".into())));
        // Never the root. CloudKit accepts that placement — Jodd made one and
        // Apple Notes displayed it inside `Notes` — but no Apple client makes
        // it, so this rule must not either.
        assert_ne!(parent_for_path("Notes/X", &folders), Ok(Some(DEFAULT_FOLDER.into())));
        // The root itself is not a place to create a folder.
        assert!(parent_for_path("Notes", &folders).is_err());
        // A missing intermediate is a refusal, not a silent reparent to the top.
        assert!(parent_for_path("Notes/Missing/New", &folders).is_err());
    }

    /// The relocation diagnostic reads its two fields off a `records/modify`
    /// reply when it can, and walks the zone only when it cannot — so
    /// "the field is absent" must be distinguishable from "the field is
    /// wrong". An option says that; a default would hide it.
    #[test]
    fn folder_and_document_is_none_when_the_reply_echoed_only_what_it_was_sent() {
        let full = json!({ "fields": {
            "Folder": folder_field("f9"),
            "TextDataEncrypted": bytes_field(&[7, 8, 9]),
        }});
        assert_eq!(folder_and_document(&full), Some(("f9".to_string(), vec![7, 8, 9])));

        // What a move's own request body looks like echoed back: a folder and
        // no document. Reading that as "the document is empty" would report
        // every move as having destroyed the note.
        let move_only = json!({ "fields": { "Folder": folder_field("f9") } });
        assert_eq!(folder_and_document(&move_only), None);
        assert_eq!(folder_and_document(&json!({})), None);
    }

    /// The undo for a destructive relocation. It must send the bytes it was
    /// given and NOTHING else — a restore that also restated the title or the
    /// folder would undo more than the damage.
    #[test]
    fn a_document_restore_writes_back_exactly_the_bytes_it_was_given() {
        let b = restore_document_body("n1", "tag1", &[1, 2, 3, 4]);
        let record = &b["operations"][0]["record"];
        assert_eq!(b["operations"][0]["operationType"], json!("update"));
        assert_eq!(record["recordChangeTag"], json!("tag1"));
        let f = &record["fields"];
        assert_eq!(
            f["TextDataEncrypted"]["value"],
            json!(base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4]))
        );
        assert!(f["TitleEncrypted"].is_null(), "a restore is not a retitle");
        assert!(f["Folder"].is_null(), "a restore is not a move");
        assert!(f["ModificationDate"].is_null(), "a restore is not an edit");
    }

    /// A folder carries no document, which is the whole reason folder writes
    /// are a separate question from the one the write census closed.
    #[test]
    fn a_folder_create_is_a_title_and_a_parent_and_nothing_else() {
        let b = create_folder_body("f1", "Scratch", Some(DEFAULT_FOLDER));
        let record = &b["operations"][0]["record"];
        assert_eq!(b["operations"][0]["operationType"], json!("create"));
        assert_eq!(record["recordType"], json!("Folder"));
        assert!(record["recordChangeTag"].is_null(), "a create has no lock to carry");
        let f = &record["fields"];
        assert_eq!(f["ParentFolder"]["value"]["recordName"], json!(DEFAULT_FOLDER));
        assert!(f["TextDataEncrypted"].is_null(), "a folder has no document");
        // Round-trips through the read path's own decoder, so a create and a
        // walk cannot disagree about how a title is stored.
        assert_eq!(decode_text_field(&f["TitleEncrypted"]).as_deref(), Some("Scratch"));
    }

    /// An ABSENT `ParentFolder` is a different placement from one naming the
    /// root — measured on 2026-08-24, when sending the root put the folder a
    /// level below where its sibling sits. Sending the field with a null, or
    /// defaulting to the root, would erase the distinction the option exists
    /// to carry.
    #[test]
    fn a_folder_create_with_no_parent_omits_the_field_entirely() {
        let b = create_folder_body("f1", "Scratch", None);
        let f = &b["operations"][0]["record"]["fields"];
        assert!(f["ParentFolder"].is_null());
        assert!(!f.as_object().unwrap().contains_key("ParentFolder"));
    }

    /// A rename must not restate the parent: every path under the folder is
    /// derived from that reference, so resending it is a chance to move a
    /// subtree by accident.
    #[test]
    fn a_folder_rename_writes_only_the_title() {
        let b = rename_folder_body("f1", "tag1", "Renamed");
        let record = &b["operations"][0]["record"];
        assert_eq!(record["recordChangeTag"], json!("tag1"));
        assert!(record["fields"]["ParentFolder"].is_null());
        assert_eq!(decode_text_field(&record["fields"]["TitleEncrypted"]).as_deref(), Some("Renamed"));
    }

    /// Deleting a FOLDER is a real delete, unlike deleting a note: Apple's
    /// Trash is itself a folder, so filing one inside it would nest a
    /// container every listing excludes.
    #[test]
    fn a_folder_delete_is_a_delete_and_carries_the_lock() {
        let b = delete_folder_body("f1", "tag1");
        assert_eq!(b["operations"][0]["operationType"], json!("delete"));
        assert_eq!(b["operations"][0]["record"]["recordChangeTag"], json!("tag1"));
        assert_ne!(
            b["operations"][0]["record"]["fields"]["Folder"]["value"]["recordName"],
            json!(TRASH_FOLDER),
            "a folder is not filed in the Trash"
        );
    }

    #[test]
    fn a_saved_record_yields_the_new_change_tag_and_the_servers_own_timestamp() {
        let reply = json!({
            "records": [{
                "recordName": "n1",
                "recordType": "Note",
                "recordChangeTag": "tag8",
                "fields": { "ModificationDate": { "value": 1_700_000_000_001i64 } }
            }]
        });
        assert_eq!(
            decode_modify_reply(&reply).unwrap(),
            SavedRecord {
                record_name: "n1".into(),
                change_tag: "tag8".into(),
                modified_ms: Some(1_700_000_000_001),
            }
        );
    }

    /// The headline failure mode, and it arrives inside an HTTP 200 — a
    /// transport that only classifies statuses reports it as a success and
    /// overwrites the other device's edit in the cache.
    #[test]
    fn a_stale_change_tag_comes_back_as_a_conflict_inside_a_200() {
        let reply = json!({
            "records": [{
                "recordName": "n1",
                "serverErrorCode": "CONFLICT",
                "reason": "record to insert already exists"
            }]
        });
        match decode_modify_reply(&reply) {
            Err(TransportError::Conflict { .. }) => {}
            other => panic!("a stale tag must reach the reconciler as a conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_request_refused_outright_is_read_from_the_top_level() {
        let reply = json!({ "serverErrorCode": "BAD_REQUEST", "reason": "malformed operation" });
        match decode_modify_reply(&reply) {
            Err(TransportError::Permanent { source }) => {
                assert!(source.to_string().contains("malformed operation"), "{source}");
            }
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    /// An unrecognised refusal is Permanent, not Transient. Gotcha #14's
    /// measurement is what settles this: a doomed request re-issued every five
    /// seconds ran 5,816 times while the editor said "Saved".
    #[test]
    fn an_unrecognised_error_code_stops_rather_than_retries_forever() {
        match classify_modify_error("SOMETHING_NEW", "who knows") {
            TransportError::Permanent { .. } => {}
            other => panic!("expected Permanent, got {other:?}"),
        }
    }

    #[test]
    fn a_session_that_died_mid_write_is_auth_and_a_busy_zone_is_transient() {
        assert!(matches!(classify_modify_error("ACCESS_DENIED", ""), TransportError::Auth));
        assert!(matches!(
            classify_modify_error("TRY_AGAIN_LATER", ""),
            TransportError::Transient { .. }
        ));
    }

    #[test]
    fn a_reply_with_no_record_at_all_is_a_refusal_not_a_success() {
        assert!(decode_modify_reply(&json!({ "records": [] })).is_err());
    }
}
