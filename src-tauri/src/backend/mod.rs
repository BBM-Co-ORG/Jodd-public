//! Backend-agnostic trait surface ("Vertical #0" seam). The shared core
//! (sync worker, conflict policy, cache) talks to a backend only through
//! these traits. Gmail is the first and only implementor today (static
//! dispatch); JMAP/Graph plug in later by implementing the same set.
//!
//! See docs/superpowers/specs/2026-06-16-architecture-principles-design.md
//! for the locked surface and rationale.

pub mod gmail;
pub mod deriver_applehtml;
pub mod localfs;
pub mod microsoft;

use std::collections::HashMap;
use std::time::Duration;
use async_trait::async_trait;

/// Opaque, vertical-owned sync position (Gmail historyId / JMAP state /
/// IMAP UIDVALIDITY+MODSEQ / mtime). The core persists and loops over it,
/// never inspects it. Gmail returns an inert cursor today (full-scan path).
#[derive(Clone, Debug, Default)]
pub struct SyncCursor(pub Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind { Upserted, Deleted }

#[derive(Clone, Debug)]
pub struct RemoteChange {
    pub remote_id: String,
    pub kind: ChangeKind,
    pub folder_hint: Option<String>,
}

#[derive(Debug, Default)]
pub struct ChangeSet {
    pub changes: Vec<RemoteChange>,
    pub next_cursor: SyncCursor,
    pub more: bool,
}

/// Result of a save. `remote_id` MAY DIFFER from any input id — Gmail re-mints
/// the message id on every content edit. The core re-points the cache `id`.
#[derive(Debug)]
pub struct SaveOutcome {
    pub remote_id: String,
    pub cursor_hint: Option<SyncCursor>,
}

/// Classified transport failure. The transport CLASSIFIES (reads HTTP status /
/// Retry-After); the shared worker owns the retry POLICY + backoff/jitter.
#[derive(Debug)]
pub enum TransportError {
    RateLimited { retry_after: Option<Duration> },
    Transient { source: anyhow::Error },
    Conflict { remote_etag: Option<String> },
    Auth,
    NotFound,
    Permanent { source: anyhow::Error },
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::RateLimited { retry_after } =>
                write!(f, "rate limited (retry_after={:?})", retry_after),
            TransportError::Transient { source } => write!(f, "transient: {}", source),
            TransportError::Conflict { remote_etag } =>
                write!(f, "conflict (etag={:?})", remote_etag),
            TransportError::Auth => write!(f, "auth"),
            TransportError::NotFound => write!(f, "not found"),
            TransportError::Permanent { source } => write!(f, "permanent: {}", source),
        }
    }
}
impl std::error::Error for TransportError {}

// ── Neutral note envelope types (shared by all verticals) ──

use serde::{Deserialize, Serialize};

/// A hydrated note as returned by fetch/list paths. Format-neutral; the Gmail
/// vertical populates it from MIME/JSON; a future LocalFS vertical would
/// populate it from the filesystem. The `attachments` field carries inline
/// binary parts (not serialized over IPC — too large).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: String,
    pub uuid: String,
    pub title: String,
    pub body_html: String,
    pub date: String,
    /// The vertical's answer to "has the remote copy changed?", compared by
    /// `reconcile_one`. NOT an id — Gmail happens to use its message id
    /// because it has no REPLACE and re-mints one on every edit, but that is
    /// a Gmail property, not a property of remotes. Exchange PATCHes in place
    /// and would look unchanged forever.
    ///
    /// Gmail: `id`. Microsoft: `lastModifiedDateTime`. LocalFs: `date`
    /// (the Date header, rewritten on every save).
    #[serde(default)]
    pub version: String,
    pub label: String,
    // Apple tracks original creation time separately from Date (last modified).
    // Preserve across edits so we don't reset the creation time on every save.
    #[serde(default)]
    pub x_mail_created_date: Option<String>,
    // Multi-account: which Gmail account this note belongs to.
    // Stamped by the Tauri command layer after fetch (gmail.rs is account-blind).
    #[serde(default)]
    pub account_id: Option<String>,
    // Jodd-local pin state. Never travels over the wire (Apple Notes stores
    // pin in iCloud metadata, which the email backend doesn't carry) — it's
    // populated from the SQLite cache by `CachedNote::to_frontend_note`.
    // For freshly-parsed wire-format notes (`parse_message`), default to false.
    #[serde(default)]
    pub pinned: bool,
    // Jodd's local edit-version for this note (`notes.local_version` in
    // SQLite) as of this response. The frontend threads this back into
    // `save_note`'s `expected_local_version` so a save can detect a
    // concurrent writer (another device's Jodd, or jodd-mcp) that landed
    // an edit after this note was loaded — see docs/superpowers/plans/
    // 2026-08-12-concurrent-local-writer-race.md. 0 for a note this device
    // has never locally edited (matches `CachedNote::from_remote`) or for a
    // struct built somewhere with no DB knowledge (a fresh Gmail/LocalFs
    // parse, not yet reconciled).
    #[serde(default)]
    pub local_version: i64,
    // Attachment parts (inline images, etc.) carried in the message's
    // multipart/related body. Populated by fetch_note; persisted to the
    // `attachments` table by reconcile_one so the save path can re-emit them
    // instead of stripping them (the data-loss bug). NOT serialized over IPC —
    // the bytes are large and the editor doesn't consume them yet.
    #[serde(skip)]
    pub attachments: Vec<Attachment>,
}

/// An attachment part extracted from a note's `multipart/related` body — an
/// inline image (`<object data="cid:…">` in the body refers to it via
/// `content_id`). `data` is the decoded bytes (stored as a SQLite BLOB).
/// `content_id` (angle brackets stripped) is stable across edits and MUST be
/// reused on write so the body's reference stays valid.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub content_id: String,
    pub mime_type: String,
    pub filename: Option<String>,
    pub x_apple_part_url: Option<String>,
    pub data: Vec<u8>,
}

/// Lightweight stub returned by `list_account_index` — just enough to drive
/// folder counts and a "loading X of Y" indicator without paying for a full
/// `messages.get` per row. Hydrated to a real `Note` later via the normal
/// list path (cache-aware) when the user focuses a folder.
#[derive(Serialize, Clone, Debug)]
pub struct MessageIndex {
    pub id: String,
    pub label: String,
}

/// Result of a note save — new remote id, preserved UUID, Date header written.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SavedNote {
    pub id: String,   // new Gmail message ID
    /// The vertical's `Note::version`-equivalent for the state just written —
    /// what `Db::mark_pushed` stamps into `remote_version` so the very next
    /// poll's `reconcile_one` compares like with like. NOT always `id`: Gmail
    /// has no REPLACE, so its new `id` IS the new version, but that's a Gmail
    /// property (see `Note::version`), not a general one.
    ///
    /// Gmail: same value as `id`. LocalFs: the `date` below (the Date header
    /// just written). Microsoft (Task 6): must be the PATCH response's
    /// `lastModifiedDateTime`, NOT `id` — Exchange PATCHes in place, so `id`
    /// doesn't move and would make every push look like a no-op version-wise.
    #[serde(default)]
    pub version: String,
    pub uuid: String, // X-Universally-Unique-Identifier (preserved or freshly generated)
    // Date header we put in the raw email (RFC 2822). The local cache must
    // mirror this — otherwise the next pull's dedupe-by-Date compares the
    // fresh remote against a stale cached date and gets the order wrong.
    pub date: String,
    // Body in EDITOR-VIEW form — what the user sees in the contenteditable.
    // This is the input we received (pre-inject_title), NOT the wire-format
    // bytes we sent to Gmail. Reason for the asymmetry: the pull path stores
    // post-strip_leading_title bodies. If push stored post-inject bodies the
    // cache would flip between "with title row" and "without title row"
    // depending on which side most recently touched it. Keeping the cache as
    // "editor-view" mirrors what fetch_note hands back, so list/dedupe/render
    // see one consistent shape regardless of origin.
    pub body_html: String,
    // The DB's `local_version` for this note immediately after this save
    // landed (see `Note::local_version` above for why). Only meaningful on
    // the value `save_note` (lib.rs) returns to the frontend — the
    // vertical-level `SavedNote` values built inside `save_note_full`
    // implementations (gmail::wire::save_note, localfs's push) are
    // discarded and reconstructed by `save_note` from the authoritative
    // `Db::get` read after the write, so `0` there is inert.
    pub local_version: i64,
}

/// A note sitting in Gmail Trash — Apple's "Recently Deleted" over this backend.
/// A trashed note keeps its original `Notes/*` label PLUS the TRASH label.
#[derive(Serialize, Clone, Debug)]
pub struct TrashedNote {
    pub id: String,
    pub uuid: String,
    pub title: String,
    pub date: String,
    pub label: String, // original Notes folder (best-effort from labelIds)
}

/// Observation summary from a single list_notes pass. Used by the frontend
/// to display an unobtrusive "N duplicates" indicator so the user has a
/// signal that cleanup_orphans is worth running.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct DedupSummary {
    /// Extra Gmail messages collapsed into their primary by uuid.
    pub collapsed: usize,
    /// How many distinct uuids had at least one duplicate.
    pub uuids_affected: usize,
}

/// Params for a save. Owned by the core (built from a cache row); the vertical
/// interprets them. `body_html` is editor-view (title not yet injected).
pub struct SaveOp<'a> {
    pub title: &'a str,
    pub body_html: &'a str,
    pub existing_remote_id: Option<&'a str>,
    pub existing_uuid: Option<&'a str>,
    pub existing_created_date: Option<&'a str>,
    pub label: &'a str,
}

/// A folder as the backend reports it (Gmail label, JMAP mailbox, …).
#[derive(Clone, Debug)]
pub struct RemoteFolder {
    pub id: String,
    pub path: String,
}

/// Which Jodd-local metadata a sidecar carries. Tags round-trip through the
/// note body as inline #hashtags instead (see `db::tags_from_body`), so the
/// only sidecar-backed kind is Pin, which has no body-visible equivalent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarKind { Pin }

/// A discovered sidecar. Pin is existence-only and leaves `body` None.
#[derive(Clone, Debug)]
pub struct SidecarRecord {
    pub id: String,
    pub note_uuid: String,
    pub kind: SidecarKind,
    pub body: Option<Vec<u8>>,
}

/// The note's own remote version/date after a write that was not itself the
/// primary content push — a sidecar write (`MetadataSidecar::put_sidecar`/
/// `remove_sidecar`) or an explicit `Transport::move_note` — for a backend
/// where that write lands on the note's remote object itself (Microsoft: the
/// pin is a named MAPI property on the note, and `move_note` PATCHes the
/// note's own `parentFolderId`) rather than a genuinely separate operation
/// (Gmail's meta-label sidecar message, its label-only `move_note`; LocalFs's
/// `.pin` file). A write that never touches the note's own remote object
/// always returns `None` — the caller must not stamp a note's
/// `remote_version`/`date` from one unless the vertical says so explicitly,
/// since assuming "unchanged" when it silently changed is the same class of
/// bug `SavedNote::version`'s own doc comment warns about for the content-push
/// path itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteNoteVersion {
    pub version: String,
    pub date: String,
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn changes_since(&self, cursor: Option<&SyncCursor>) -> Result<ChangeSet, TransportError>;
    async fn save(&self, op: SaveOp<'_>) -> Result<SaveOutcome, TransportError>;
    async fn delete(&self, remote_id: &str) -> Result<(), TransportError>;
    async fn list_folders(&self) -> Result<Vec<RemoteFolder>, TransportError>;
    async fn ensure_folder(&self, path: &str) -> Result<RemoteFolder, TransportError>;
    async fn create_folder(&self, name: &str) -> Result<RemoteFolder, TransportError>;
    async fn rename_folder(&self, id: &str, new_name: &str) -> Result<(), TransportError>;
    async fn delete_folder(&self, id: &str) -> Result<(), TransportError>;
    /// Returns the note's own resulting remote version/date if this move
    /// touched the note's own remote object rather than a genuinely separate
    /// relocation — see [`RemoteNoteVersion`]. `push_one_dirty` (lib.rs) is
    /// the caller that depends on this: on Microsoft, this PATCHes the same
    /// message an earlier content push in the same tick may have just PATCHed,
    /// so its response is the caller's only source of truth for the note's
    /// version after the move — discarding it left `notes.remote_version`
    /// stale (Microsoft fix 5).
    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<Option<RemoteNoteVersion>, TransportError>;
}

#[async_trait]
pub trait MetadataSidecar: Send + Sync {
    /// `Ok(None)` = the sidecar store is not initialized on this backend (e.g. the
    /// meta-label/dir does not exist yet) → the caller MUST NOT prune local state.
    /// `Ok(Some(v))` = the store was enumerated (possibly empty) → the caller may
    /// prune local pins/tags to exactly `v`.
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Option<Vec<SidecarRecord>>, TransportError>;
    /// Create/replace a sidecar for `note_uuid`. `body` is an optional payload —
    /// Pin may pass `{"pinned":true}` or None, since Pin is existence-based and
    /// impls MAY ignore the body value. Trashes `replace` if given
    /// (insert-then-trash). Returns the new sidecar id, and the note's own
    /// resulting remote version/date if this write also touched the note's
    /// remote object (see [`RemoteNoteVersion`]).
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<(String, Option<RemoteNoteVersion>), TransportError>;
    /// Returns the note's own resulting remote version/date if removing the
    /// sidecar also touched the note's remote object — see
    /// [`RemoteNoteVersion`].
    async fn remove_sidecar(&self, id: &str) -> Result<Option<RemoteNoteVersion>, TransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderModel { SingleExclusive }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity { Full }

/// How a content push (`NoteStore::save_note_full`) relocates a note when
/// its folder changed, and what a `NotFound` on that push means. The two
/// facts travel together because they share one root cause — whether the
/// backend's save is REPLACE-shaped or PATCH-shaped — and letting a future
/// backend declare them separately would just recreate the chance of
/// forgetting one, the exact gap this enum replaces (`push_one_dirty` used
/// to answer both questions with two separate `backend_kind == Microsoft`
/// checks in shared sync code, bypassing `Capabilities` entirely).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveSemantics {
    /// A content push relocates the note as an intrinsic part of itself —
    /// Gmail inserts a fresh message under the new label and trashes the
    /// old one; LocalFs writes into the new folder's directory and removes
    /// the old file. No separate `Transport::move_note` call is needed for
    /// an ordinary label change, and a `NotFound` on an update push is NOT
    /// trustworthy evidence the note is gone — Gmail's `classify_str`
    /// (`backend/gmail/transport.rs`) substring-matches ANY HTTP 404 (rate
    /// limiting, a malformed body, an account restriction) onto `NotFound`,
    /// with the old message left untouched.
    RelocatesOnContentPush,
    /// A content push PATCHes the existing object in place and never touches
    /// its folder — `push_one_dirty` (lib.rs) must issue an explicit
    /// `Transport::move_note` after the content push when the label changed.
    /// Because PATCH targets a specific existing id, a `NotFound` on an
    /// UPDATE (not a create) IS trustworthy evidence the remote object no
    /// longer exists.
    InPlaceUpdateNeedsExplicitMove,
}

#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub folder_model: FolderModel,
    pub fidelity: Fidelity,
    /// See [`SaveSemantics`].
    pub save_semantics: SaveSemantics,
    /// Can this backend show the user a recoverable-deletions view?
    ///
    /// Gmail trashes rather than permanently deletes, and LocalFs moves the
    /// `.eml` into its own `trash_dir()`; both back a real
    /// `NoteStore::list_trashed`, so both are `true`.
    ///
    /// Microsoft is `false`, and this is now backed by direct evidence, not
    /// merely the absence of a visible trash. Measured 2026-08-14 (M2, live
    /// account): a note deleted on the Mac vanished from Jodd on the next
    /// poll, and the mailbox afterwards showed 0 `IPM.StickyNote` items with
    /// `parentFolderId` equal to Deleted Items' id, the note absent from the
    /// scan entirely, and `GET /me/mailFolders/deleteditems/messages`
    /// returning 0 items. So an Apple-side delete does not soft-delete into
    /// Deleted Items the way an ordinary mail delete does — there is
    /// genuinely nothing to restore from. `DELETE /me/messages/{id}` (Jodd's
    /// own delete path, `microsoft/transport.rs::delete` → `wire::
    /// delete_message`) returns `204` and the note leaves Notes.app within
    /// ~60s; Graph's own `DELETE` is documented as a soft delete to Deleted
    /// Items, but that is beside the point given the measurement above. This
    /// is why `needsPermanentDeleteConfirm` (notes.ts) confirms on this
    /// backend rather than leaning on a trash that does not exist — never
    /// offer an undo or a "Recently Deleted" view here.
    ///
    /// The UI must hide "Recently Deleted" on a backend that reports `false`
    /// rather than showing an always-empty view that reads as a sync bug.
    pub has_trash: bool,
    /// What this backend can be written to. Replaces a single `can_write`,
    /// which could not express a backend whose write support differs by
    /// area. Gmail and LocalFs write everywhere (`Writes::ALL`). Microsoft
    /// writes notes and sidecars but never folders — `folders: false` there
    /// is a permanent limitation, not a pending milestone: neither Graph
    /// folder-creation surface can set the `IPF.StickyNote` container class,
    /// and the class is immutable after creation (see the Microsoft arm of
    /// `for_backend` below). Sidecars are `true` for Microsoft;
    /// `SidecarKind` now has exactly one variant, `Pin` — the separate
    /// tags-sidecar mechanism this comment used to gate (M2/M4) was later
    /// removed as dead weight, since tags round-trip via body-derived
    /// `#hashtags` on every backend instead.
    ///
    /// This is NOT cosmetic — it is the guard that keeps an account out of a
    /// state it cannot leave. A `pin_dirty` row whose push errors forever
    /// keeps `db::has_pending_pushes` true, so the account never finishes
    /// Draining, and `remove_account` refuses a Draining account (gotcha #2):
    /// the user is pinned at "finishing sync — 1 left" with no way out. The
    /// refusal must happen at the command layer, BEFORE anything reaches
    /// SQLite.
    pub writes: Writes,
}

/// Per-area write permissions. Sidecar dirt is the dangerous kind: a failed
/// note push retries harmlessly, but sidecar dirt is what
/// `has_pending_pushes` reads, and that is wired to account lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Writes {
    /// create / edit / delete / move a note
    pub notes: bool,
    /// create / rename / delete a folder
    pub folders: bool,
    /// pin — the only sidecar-backed kind (`SidecarKind::Pin`). Tags are
    /// not a sidecar: they round-trip via body-derived `#hashtags` on every
    /// backend.
    pub sidecars: bool,
}

/// Which area a guarded command writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write { Notes, Folders, Sidecars }

impl Writes {
    pub const ALL: Writes = Writes { notes: true, folders: true, sidecars: true };
    pub fn allows(&self, w: Write) -> bool {
        match w {
            Write::Notes => self.notes,
            Write::Folders => self.folders,
            Write::Sidecars => self.sidecars,
        }
    }
}

impl Capabilities {
    /// Single source of truth for what a backend can offer, derived from
    /// `BackendKind` alone — no token, no vertical construction, no network
    /// call. Each vertical's `new()` calls this rather than building the
    /// struct inline, and the `backend_capabilities` Tauri command (lib.rs)
    /// uses it directly from the account's `backend_kind`.
    ///
    /// Deliberately NOT routed through `vertical_for`: for a Microsoft
    /// account that would fetch a token on every call (a network round trip
    /// on plain account-switch navigation — the local-first doctrine calls
    /// that a bug), and `vertical_for` refuses `Inactive` accounts outright,
    /// which would leave the UI with no capabilities to render at all.
    pub fn for_backend(kind: crate::accounts::BackendKind) -> Capabilities {
        use crate::accounts::BackendKind;
        match kind {
            // Gmail Trash is Apple's "Recently Deleted" over this backend.
            BackendKind::Gmail => Capabilities {
                folder_model: FolderModel::SingleExclusive,
                fidelity: Fidelity::Full,
                save_semantics: SaveSemantics::RelocatesOnContentPush,
                has_trash: true,
                writes: Writes::ALL,
            },
            // LocalFs keeps a real `trash_dir()` that `list_trashed` walks.
            BackendKind::LocalFs => Capabilities {
                folder_model: FolderModel::SingleExclusive,
                fidelity: Fidelity::Full,
                save_semantics: SaveSemantics::RelocatesOnContentPush,
                has_trash: true,
                writes: Writes::ALL,
            },
            // See the `has_trash` doc comment above: absence of evidence,
            // not evidence of absence.
            BackendKind::Microsoft => Capabilities {
                folder_model: FolderModel::SingleExclusive,
                fidelity: Fidelity::Full,
                // PATCH updates in place and never touches parentFolderId —
                // see `SaveSemantics::InPlaceUpdateNeedsExplicitMove`'s doc
                // comment. `push_one_dirty` (lib.rs) reads this rather than
                // checking `backend_kind == Microsoft` directly.
                save_semantics: SaveSemantics::InPlaceUpdateNeedsExplicitMove,
                has_trash: false,
                // Notes: on, confirmed live 2026-08-15 (`ms_write_probe`
                // against kaiwan.h@live.com) — create/patch/retitle/move/
                // delete all returned 2xx and the retitled note plus the
                // empty-title note both rendered correctly in Notes.app.
                //
                // Folders: off, and this is a measured negative, not caution.
                // Two folders created via Graph were still absent from
                // Notes.app 21 hours later and after a forced resync — not
                // sync lag, because two other folders created BY HAND in
                // Notes.app were visible the whole time, and notes Graph
                // wrote INTO those hand-made folders reached Apple fine. So
                // the variable is the folder's provenance, not who writes
                // the note. The mechanism: Graph's documented folder-write
                // surface is `POST /me/mailFolders` and
                // `PATCH /me/mailFolders/{id}`; the call Jodd needs to nest
                // under Notes, `POST /me/mailFolders/{id}/childFolders`, is
                // not in that list, and it shows: `childFolders` returns 201
                // but silently drops the `PR_CONTAINER_CLASS` extended
                // property that would mark the folder as a Notes container.
                // Trying to fix that after the fact confirms the class is
                // immutable — `PATCH /me/mailFolders/{id}` with
                // `PR_CONTAINER_CLASS` answers **500 `ErrorObjectTypeChanged`**,
                // *"Operation would change object type, which is not
                // permitted."* And the documented creation path,
                // `POST /me/mailFolders`, does work (201) but creates at
                // mailbox root, not nested under Notes, and still echoes no
                // extended properties back. Every layer is silent except
                // that one `PATCH` — `GET` 404s on a genuine Notes-tree
                // folder either way, so nothing else can even ask.
                //
                // M3 (2026-08-15, live, `ms_folder_move_probe.py`) ran the
                // one avenue left untried: create at root WITH the class set,
                // then `POST /me/mailFolders/{id}/move` under Notes. Both a
                // classed and an unclassed folder moved successfully (2xx)
                // and got a witness note, but neither ever appeared in
                // Notes.app — confirmed on Mac AND iPhone, with Outlook's own
                // Notes UI showing both as real children of Notes the whole
                // time. Unlike a `childFolders`-created folder, these DON'T
                // 404 on `GET` (see CLAUDE.md gotcha #12's M3 addendum), and
                // reading them back found the mechanism directly:
                // `PR_CONTAINER_CLASS` came back `IPF.Note` on both, even the
                // one that requested `IPF.StickyNote` at creation — so
                // `POST /me/mailFolders` drops the class exactly like
                // `childFolders` does. Combined with the immutability finding
                // above, no sequence of Graph calls can ever produce a folder
                // classed `IPF.StickyNote`. Folder writes are therefore a
                // **permanent** limitation of this backend, not deferred —
                // see CLAUDE.md's "Folder writes do not work" and the M2
                // spec's now-closed "Deferred to M3" section for the ledger.
                //
                // Sidecars: on. M4 (2026-08-16) does not put anything IN the
                // Notes tree at all — the earlier "any item there shows in
                // Notes.app as a stray note" concern assumed a sidecar
                // message, the Gmail shape. Instead pin state lives in a
                // GUID-named MAPI property (`wire::JODD_PIN_PROP`) written
                // directly onto the note's own message. Measured live
                // (`scripts/ms_named_property_probe.py`, 2026-08-15/16):
                // Graph round-trips it, it survives an ordinary content
                // PATCH, and a note carrying it renders completely normally
                // in Notes.app on Mac/iPhone and on outlook.live.com/mail/
                // notes — "Apple only reads fields it knows about," measured
                // rather than assumed. `SidecarKind` only has `Pin` now — a
                // separate tags-sidecar mechanism existed when the M4 design
                // spec was written but was removed as dead weight shortly
                // after (tags round-trip via body-derived #hashtags on every
                // backend), so there is nothing left to gate for tags here.
                writes: Writes { notes: true, folders: false, sidecars: true },
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind { AppleHtml }

#[derive(Clone, Debug)]
pub struct Edge { pub rel: String, pub target: String }

#[derive(Default, Debug)]
pub struct Derived {
    pub text: String,
    pub tags: Vec<String>,
    pub edges: Vec<Edge>,
}

/// Synchronous: local CPU/ms work (the data doctrine targets network latency,
/// not local compute). Links/edges/tags are body-resident and re-derived
/// everywhere — they are never carried in a sidecar.
pub trait Deriver: Send + Sync {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived;
}

pub trait Identity: Send + Sync {
    fn mint(&self) -> String;
}

/// Per-vertical note read/write orchestration. Each backend implements its own
/// strategy (Gmail dedups transient duplicates; LocalFS has one file per uuid so
/// it does not). Generic post-processing (sort, cache upsert, conflict, index,
/// prune) stays in the core, not here.
#[async_trait]
pub trait NoteStore: Send + Sync {
    async fn list_all_notes(&self, cache_by_id: &HashMap<String, Note>) -> Result<(Vec<Note>, DedupSummary), TransportError>;
    /// Returns the notes in `folder`. Returning an empty Vec is valid for a folder
    /// that exists locally but has no remote representation yet (do NOT return
    /// NotFound for an unknown folder — the caller relies on empty).
    async fn list_notes_in_folder(&self, folder: &str, cache_by_id: &HashMap<String, Note>) -> Result<Vec<Note>, TransportError>;
    async fn list_index(&self) -> Result<Vec<MessageIndex>, TransportError>;
    async fn fetch_note(&self, remote_id: &str) -> Result<Note, TransportError>;
    async fn save_note_full(&self, op: &SaveOp<'_>, attachments: &[Attachment]) -> Result<SavedNote, TransportError>;
    async fn find_ids_for_uuid(&self, uuid: &str) -> Result<Vec<String>, TransportError>;
    async fn list_trashed(&self) -> Result<Vec<TrashedNote>, TransportError>;
    async fn untrash(&self, remote_id: &str) -> Result<(), TransportError>;
}

pub trait Vertical: Transport + MetadataSidecar + NoteStore + Identity + Deriver + Send + Sync {
    fn backend_id(&self) -> &str;
    fn capabilities(&self) -> &Capabilities;
}
