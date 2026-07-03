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
