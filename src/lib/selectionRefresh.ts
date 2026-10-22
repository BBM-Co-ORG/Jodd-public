import { get } from 'svelte/store';
import { selectedNote } from './stores/notes';
import { sameNote } from './noteIdentity';
import type { Note } from './types';

/**
 * Whether a fresh read of the open note differs from the selection in anything
 * the editor shows or saves against. Field comparison, not reference equality:
 * every read returns a fresh object, and a spurious `selectedNote.set` re-runs
 * NoteEditor's render decision for nothing.
 */
export function selectionDiffers(updated: Note, cur: Note): boolean {
  return (
    updated.body_html !== cur.body_html ||
    updated.title !== cur.title ||
    updated.id !== cur.id ||
    updated.date !== cur.date ||
    updated.local_version !== cur.local_version ||
    updated.push_blocked_reason !== cur.push_blocked_reason ||
    !!updated.push_blocked_by_remote !== !!cur.push_blocked_by_remote
  );
}

/**
 * Whether a selection exists only in the editor's memory: a brand-new note
 * with a `tmp:` uuid (or none yet). It has no SQLite row — `tmp:` uuids never
 * reach Rust — so nothing read from SQLite or a listing can be newer than it;
 * the listing's copy of it is an OLDER snapshot of what the user typed.
 *
 * This is the marker, not `id`. `id` is the remote's: an agent's unpushed
 * create_note/remember is in SQLite with none, and nothing gives the open
 * selection one when the worker pushes it — gating on it left the old body on
 * screen until the note was re-selected (PR #141, then reconcileSelection).
 */
export function isInMemoryOnly(n: Pick<Note, 'uuid'>): boolean {
  return !n.uuid || n.uuid.startsWith('tmp:');
}

/**
 * Push a fresh read of the open note into the selection — the update half of
 * App.svelte's `reconcileSelection` and of NoteEditor's review re-read, so
 * every refresh reaches the editor by the same route: NoteEditor's render
 * decision then renders it, or holds it back behind the "edited on another
 * device" banner while there is unsaved work. That decision, not this gate,
 * is what protects unsaved typing in a persisted note.
 *
 * A read with a LOWER `local_version` than the selection's is dropped: it is
 * the note one local write ago — a listing read before a save committed,
 * landing after it. `upsert_from_remote` never moves `local_version` on an
 * existing row (it counts local writes), so a remote edit arrives at the
 * selection's own version and a conflict bumps it; neither is lower. Strictly
 * lower, never "not newer": equal with a different body IS the remote edit.
 */
export function refreshSelection(updated: Note): void {
  const cur = get(selectedNote);
  if (!cur || isInMemoryOnly(cur) || !sameNote(updated, cur)) return;
  if (updated.local_version != null && cur.local_version != null && updated.local_version < cur.local_version) return;
  if (selectionDiffers(updated, cur)) selectedNote.set(updated);
}
