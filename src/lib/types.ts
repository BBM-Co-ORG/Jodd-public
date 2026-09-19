export interface Note {
  id: string;
  uuid: string;
  title: string;
  body_html: string;
  date: string;
  label: string;
  // Apple tracks original creation time separately from `date` (last modified).
  // Preserved across saves so Apple Notes doesn't see the creation time change.
  x_mail_created_date?: string | null;
  // Multi-account: which Gmail account this note belongs to.
  // Stamped by the Rust backend after fetch; required when saving/deleting.
  account_id?: string | null;
  // Jodd-local pin state. Doesn't round-trip to Apple Notes (no place to
  // store it in the email backend); driven entirely by the SQLite cache.
  // Notes with pinned=true sort to the top of NoteList regardless of date.
  pinned?: boolean;
  // Jodd's local edit-version for this note, as last observed by this
  // frontend (from a fetch, or updated after this device's own successful
  // save). Threaded back into `save_note`'s `expected_local_version` so the
  // backend can detect a concurrent writer that landed an edit since this
  // note was loaded. Optional/undefined for a note this frontend has never
  // received a version for (e.g. a brand-new unsaved draft).
  local_version?: number;
  // Why the sync worker gave up pushing this note, or absent/null when it is
  // syncing normally. Jodd-local, from the SQLite cache — like `pinned`, it
  // never travels over any backend's wire.
  //
  // Set only for a failure the backend called PERMANENT: retrying the same
  // push can never succeed, so the worker stops and records the reason
  // instead of re-issuing it every 5 seconds. The note still holds the user's
  // content and is still shown; what changes is that the editor must stop
  // claiming "Saved" for it.
  push_blocked_reason?: string | null;
}

// A note sitting in the backend's trash ("Recently Deleted"). Deliberately
// lighter than Note — no body_html, so listing trashed notes doesn't pay for
// a full fetch per row. Fetch the body on demand (get_trashed_note_preview)
// only when the user actually opens one to look at it.
export interface TrashedNote {
  id: string;
  uuid: string;
  title: string;
  date: string;
  label: string;
  /**
   * Is `label` the folder this note actually came from, or a fallback?
   *
   * Gmail and LocalFs answer true — a trashed Gmail message keeps its
   * `Notes/*` label, and LocalFs encodes the original relpath into the trash
   * filename. iCloud answers false: a trashed record's folder reference is
   * replaced by the Trash's, and nothing measured says where the original
   * went. The menu hides plain "Restore" when it is false, because a restore
   * that quietly files every note in the root is a silent reorganisation of
   * somebody's account.
   */
  original_known: boolean;
}

export interface Account {
  id: string;      // = email
  email: string;
  added_at: string; // ISO 8601
  // Backend kind: "gmail" (default) or "local_fs". Matches the Rust serde
  // snake_case serialization of BackendKind. Absent on accounts.json files
  // written before the LocalFS feature — treat absence as "gmail".
  backend_kind?: string;
  // Absolute path to the notes root for LocalFs accounts; null/absent for Gmail.
  root_dir?: string | null;
  // Lifecycle state: "active" (default), "draining" or "inactive". Matches the
  // Rust serde snake_case of AccountStatus. Absent on accounts.json files
  // written before the feature — treat absence as "active".
  status?: string;
  // A whole-account hard block a read discovered, already phrased for the
  // user — today only Advanced Data Protection on an iCloud account, where
  // note bodies are end-to-end encrypted and nothing can read them.
  //
  // Re-derived on every index pass, so it clears itself when the account
  // becomes readable again. Absent/null means nothing is wrong.
  blocked_reason?: string | null;
  // Set when the user asked to remove this account while it was still
  // `draining` — `remove_account` queues the request instead of performing
  // it immediately. `sync_worker_tick` finishes the real removal once the
  // account reaches `inactive` on its own; until then the row (and this
  // flag) stays put. Absent/false means no removal is queued.
  pending_removal?: boolean;
}

export interface Folder {
  id: string;
  name: string;
  path: string;
  count: number;
  // Folder kind, mirroring `folders.kind` in SQLite (migration #9). Drives
  // the Sidebar Folders/Workflows group split (Task 16). 'user' for genuine
  // user-created folders, 'system_workflow' for Jodd-managed workflow
  // outputs (e.g. Notes/Lessons), 'smart_query' reserved for future
  // smart/dynamic folders. Most code paths can treat absence as 'user'.
  kind?: 'user' | 'system_workflow' | 'smart_query';
}

// Lightweight stub for the per-account message index. Returned by
// `index_account` — gives us folder counts and "loaded X of Y" before any
// bodies are fetched. Hydrated to a full Note later via list_notes_in_folder.
export interface MessageIndex {
  id: string;
  label: string;
}

// Per-account observation from the most recent list_notes pass. Drives the
// sidebar's "N duplicate(s)" pill — non-alarming hint that cleanup_orphans
// is worth running. Counts come from Gmail-side duplicates that the in-memory
// dedup quietly collapsed.
export interface DedupSummary {
  collapsed: number;
  uuids_affected: number;
}

// One version of a note (either the keeper or an orphan). Returned by
// preview_orphans so the user can see exactly what's about to be trashed
// before confirming.
export interface OrphanVersion {
  id: string;          // Gmail message id
  title: string;
  date: string;        // RFC 2822 string from the message Date header
  body_preview: string; // HTML stripped, first ~200 chars
  label: string;
}

// Group of versions sharing one X-UUID: the keeper plus the orphans the
// user can choose to trash.
export interface OrphanGroup {
  uuid: string;
  keeper: OrphanVersion;
  orphans: OrphanVersion[];
}

// URL ingest (docs/superpowers/specs/2026-09-15-url-ingest-design.md).
// Field names are the Rust serde output of `analyze_ingest_sources` /
// `ingest::run::IngestProgress` — snake_case on purpose.
export type IngestSourceKind = 'web' | 'youtube' | 'unsupported';

export interface IngestSource {
  url: string;
  kind: IngestSourceKind;
  supported: boolean;
  reason: string | null;
  duplicate_owner: { uuid: string; title: string } | null;
}

export interface IngestAnalysis {
  sources: IngestSource[];
  mostly_urls: boolean;
  context_text: string;
  context_chars: number;
}

export type IngestStage = 'fetching' | 'summarizing' | 'synthesizing' | 'writing' | 'done';

export interface IngestProgress {
  stage: IngestStage;
  index: number;
  total: number;
  url_host: string | null;
}

export interface ExtractedNote {
  uuid: string;
  label: string;
}

// The three source-to-note workflows added by roadmap #2 (`run_llm_workflow`
// / `append_llm_workflow_note`), alongside the pre-existing Extract (which
// stays its own dedicated `extract_note` / `append_extract_note` commands —
// not part of this set). Matches the Rust serde snake_case of
// `llm::provider::WorkflowKind`; do not use any other casing/spelling.
export type WorkflowKind = 'summarize' | 'action_items' | 'expand_bullets';
