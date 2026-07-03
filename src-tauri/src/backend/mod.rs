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

/// Which Jodd-local metadata a sidecar carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarKind { Pin, Tags }

/// A discovered sidecar. `body` is present only for kinds that need it (Tags);
/// Pin is existence-only and leaves `body` None.
#[derive(Clone, Debug)]
pub struct SidecarRecord {
    pub id: String,
    pub note_uuid: String,
    pub kind: SidecarKind,
    pub body: Option<Vec<u8>>,
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
    async fn move_note(&self, remote_id: &str, add: &[String], remove: &[String]) -> Result<(), TransportError>;
}

#[async_trait]
pub trait MetadataSidecar: Send + Sync {
    /// `Ok(None)` = the sidecar store is not initialized on this backend (e.g. the
    /// meta-label/dir does not exist yet) → the caller MUST NOT prune local state.
    /// `Ok(Some(v))` = the store was enumerated (possibly empty) → the caller may
    /// prune local pins/tags to exactly `v`.
    async fn list_sidecars(&self, kind: SidecarKind) -> Result<Option<Vec<SidecarRecord>>, TransportError>;
    /// Create/replace a sidecar for `note_uuid`. `body` is an optional payload
    /// (Tags pass `{"tags":[...]}`; Pin may pass `{"pinned":true}` or None — Pin is
    /// existence-based, so impls MAY ignore the body value). Trashes `replace` if
    /// given (insert-then-trash). Returns the new sidecar id.
    async fn put_sidecar(&self, note_uuid: &str, kind: SidecarKind, body: Option<&[u8]>, replace: Option<&str>) -> Result<String, TransportError>;
    async fn remove_sidecar(&self, id: &str) -> Result<(), TransportError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderModel { SingleExclusive }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity { Full }

#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    pub folder_model: FolderModel,
    pub fidelity: Fidelity,
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
