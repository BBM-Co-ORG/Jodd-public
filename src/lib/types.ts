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

export interface Account {
  id: string;      // = email
  email: string;
  added_at: string; // ISO 8601
}

export interface Folder {
  id: string;
  name: string;
  path: string;
  count: number;
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
