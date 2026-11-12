import { invoke } from '@tauri-apps/api/core';
import { get, writable, type Writable } from 'svelte/store';
import type { Note } from '../types';
import { noteKey, sameNote, forwardNoteIdentity } from '../noteIdentity';
import { notes, selectedNote, selectedUuids, smartFolderNotes, folderSuggestions,
  folderSuggestionKey, noteTagsByAccount, recentlySavedUuids } from '../stores/notes';
export interface NotePersistence {
  account_id: string;
  uuid: string;
  local_version: number;
  sync_state: 'clean' | 'dirty' | 'pull_needed' | 'conflict' | 'deleted_pending';
  push_blocked_reason: string | null;
  push_blocked_by_remote?: boolean;
}
export const persistenceByNote = writable<Record<string, NotePersistence>>({});
export const pendingLocalEdits = writable<Record<string, { title: string; body_html: string }>>({});
export function acknowledgeLocalEdit(note: Note) {
  const key = noteKey(note), draft = get(pendingLocalEdits)[key];
  if (draft?.title === note.title && draft.body_html === note.body_html) pendingLocalEdits.update(m => { const next = { ...m }; delete next[key]; return next; });
}
export const savingNoteKeys = writable<Set<string>>(new Set());
const generations = new Map<string, number>();

export function rekeyNoteStores(accountId: string, oldUuid: string, uuid: string) {
  if (oldUuid === uuid) return;
  const old = { account_id: accountId, uuid: oldUuid }, next = { account_id: accountId, uuid };
  const oldKey = noteKey(old), newKey = noteKey(next);
  const suggestionKey = folderSuggestionKey(accountId, oldUuid);
  forwardNoteIdentity(accountId, oldUuid, uuid);
  const rekey = (n: Note) => sameNote(n, next) ? { ...n, uuid } : n;
  for (const store of [notes, smartFolderNotes]) store.update(ns => {
    const seen = new Set<string>();
    return ns.map(rekey).filter(n => { const k = noteKey(n); if (seen.has(k)) return false; seen.add(k); return true; });
  });
  selectedNote.update(n => n ? rekey(n) : n);
  selectedUuids.update(s => { if (!s.has(oldKey)) return s; const next = new Set(s); next.delete(oldKey); next.add(newKey); return next; });
  folderSuggestions.update(m => { const s = m[suggestionKey]; if (!s) return m; const n = { ...m, [folderSuggestionKey(accountId, uuid)]: s }; delete n[suggestionKey]; return n; });
  const moveMapValue = <T>(store: Writable<Map<string, Map<string, T>>>) => store.update(m => {
    const inner = m.get(accountId);
    if (inner?.has(oldUuid)) {
      if (!inner.has(uuid)) inner.set(uuid, inner.get(oldUuid)!);
      inner.delete(oldUuid);
    }
    return m;
  });
  moveMapValue(noteTagsByAccount);
  moveMapValue(recentlySavedUuids);
  pendingLocalEdits.update(m => { const d = m[oldKey]; if (!d) return m; const next = { ...m, [newKey]: d }; delete next[oldKey]; return next; });
  persistenceByNote.update(m => { const s = m[oldKey]; if (!s) return m; const next = { ...m, [newKey]: { ...s, uuid } }; delete next[oldKey]; return next; });
}

export function recordLocalSave(note: Note) {
  if (!note.account_id || note.local_version === undefined) return;
  const key = noteKey(note);
  generations.set(key, (generations.get(key) ?? 0) + 1);
  persistenceByNote.update(m => ({ ...m, [key]: { account_id: note.account_id!, uuid: note.uuid,
    local_version: note.local_version!, sync_state: 'dirty', push_blocked_reason: null, push_blocked_by_remote: false } }));
}
/** SQLite-only read. Events are nudges, never evidence that the current edit synced. */
export async function refreshNotePersistence(note: Pick<Note, 'account_id' | 'uuid'>) {
  if (!note.account_id || !note.uuid || note.uuid.startsWith('tmp:')) return;
  const key = noteKey(note), seq = (generations.get(key) ?? 0) + 1;
  generations.set(key, seq);
  try {
    const state = await invoke<NotePersistence | null>('note_persistence', { accountId: note.account_id, uuid: note.uuid });
    if (generations.get(key) !== seq || noteKey(note) !== key) return;
    if (state === null) return null;
    if (!state || state.account_id !== note.account_id || !Number.isInteger(state.local_version)) return;
    const previous = get(persistenceByNote)[key];
    if (previous && previous.local_version > state.local_version) return;
    rekeyNoteStores(state.account_id, note.uuid, state.uuid);
    persistenceByNote.update(m => ({ ...m, [noteKey(state)]: state }));
    const selected = get(selectedNote);
    const byRemote = !!state.push_blocked_by_remote;
    if (selected && sameNote(selected, state) && (selected.push_blocked_reason !== state.push_blocked_reason
        || !!selected.push_blocked_by_remote !== byRemote))
      selectedNote.set({ ...selected, push_blocked_reason: state.push_blocked_reason, push_blocked_by_remote: byRemote });
    return state;
  } catch { /* Offline SQLite/IPC failure is unknown, never a sync success. */ }
}
/** A `push_blocked_reason` without the transport's `permanent: `
 * classification, which is for the log. This is the raw text: what the user
 * reads first is `reasonForUser`'s sentence. */
export function blockedReasonText(reason: string): string {
  return reason.replace(/^permanent:\s*/, '');
}

/** What a refusal's notice can offer. Each refusal class picks its own, so
 * no button is offered that cannot work for that refusal: a folder the
 * server refused gets "Move to Notes", never a copy into the same folder,
 * which the server would refuse too (re-critique after #292). */
export type RefusalAction = 'retry' | 'copy' | 'moveToRoot';

/** A refused push as the user reads it: a plain `sentence` that carries the
 * next step, the backend's own text as `detail` (behind "Details"), or null
 * when the sentence already is that text, and the `actions` that can help. */
export interface UserReason {
  sentence: string;
  detail: string | null;
  actions: RefusalAction[];
}

/** The folder every account has, where a note can always be filed (gotcha
 * #9: the folder side names it "Notes"; on Outlook it is the one folder Graph
 * can write by its well-known name, gotcha #12). */
export const ROOT_FOLDER = 'Notes';

const leafOf = (path: string) => path.split('/').pop() || path;

type Row = { test: RegExp; sentence: (m: RegExpMatchArray) => string | null; actions: RefusalAction[] };

// Each row: a refusal class the backends produce (sync_worker.rs,
// backend/*/), matched on its stable wording, the sentence that replaces it
// and the actions that can fix it. A sentence that returns null falls through
// to the next row. A refusal no row knows falls back to UNKNOWN_REFUSAL, with
// the raw text kept as the detail, never dropped.
const REMOTE_LOCKS: ReadonlyArray<Row> = [
  { test: /password-protected/i, sentence: () => 'This note is password-protected in Apple Notes. Edit it there.', actions: ['copy'] },
  { test: /attachment|table|inline tag/i, sentence: () => "This note has attachments, tables or tags Jodd can't write yet. Edit it in Apple Notes.", actions: ['copy'] },
  { test: /open it in Apple Notes/i, sentence: () => "Jodd can't keep this note's formatting exactly, so it won't overwrite it. Edit it in Apple Notes.", actions: ['copy'] },
];
const REFUSALS: ReadonlyArray<Row> = [
  { test: /attachment/i, sentence: () => "This account can't keep attachments in a note. Remove the attachment, then try again.", actions: ['retry'] },
  // Outlook can always write the root by its well-known name; any other
  // folder needs an id it may not have yet (gotcha #12).
  {
    test: /Exchange (?:folder )?id for '([^']+)'/i,
    sentence: (m) => (m[1] === ROOT_FOLDER ? null : `Outlook can't find the folder \u201c${leafOf(m[1])}\u201d yet. Move the note to Notes and it will sync.`),
    actions: ['moveToRoot'],
  },
  { test: /Exchange (?:folder )?id/i, sentence: () => "Outlook can't reach its Notes folder yet. Try again later, or copy your text to a new note.", actions: ['retry', 'copy'] },
  {
    test: /refused to create label '([^']+)'/i,
    sentence: (m) => `Gmail wouldn't create the folder \u201c${leafOf(m[1])}\u201d. Move the note to Notes and it will sync.`,
    actions: ['moveToRoot'],
  },
  { test: /permission denied|os error 13|read-only file system/i, sentence: () => "Jodd can't write to this folder on disk. Check that it still exists and can be written to, then try again.", actions: ['retry'] },
];
export const UNKNOWN_REFUSAL = 'The server refused this change.';
const UNKNOWN_REMOTE_LOCK = "Jodd can't write to this note. Edit it in Apple Notes.";
const IN_ROOT_ALREADY = 'Try again, or copy your text to a new note.';

/** `label` is the note's folder: "Move to Notes" is offered only from
 * another folder; a note already in Notes is offered Try again and a copy. */
export function reasonForUser(reason: string, byRemote = false, label?: string | null): UserReason {
  const text = blockedReasonText(reason).trim();
  const inRoot = !label || label === ROOT_FOLDER;
  // A folder hold is written for the user already: it names the folder and
  // the remedy (sync_worker.rs FOLDER_WAIT_PREFIX). Its own note can always
  // leave that folder for Notes.
  if (text.startsWith('Not sent: folder ')) {
    return { sentence: text, detail: null, actions: inRoot ? ['retry'] : ['moveToRoot'] };
  }
  for (const row of byRemote ? REMOTE_LOCKS : REFUSALS) {
    const m = text.match(row.test);
    if (!m) continue;
    const sentence = row.sentence(m);
    if (sentence === null) continue;
    if (row.actions.includes('moveToRoot') && inRoot) {
      return { sentence: `${sentence.replace(/ Move the note to Notes and it will sync\.$/, '')} ${IN_ROOT_ALREADY}`, detail: text || null, actions: ['retry', 'copy'] };
    }
    return { sentence, detail: text || null, actions: [...row.actions] };
  }
  return byRemote
    ? { sentence: UNKNOWN_REMOTE_LOCK, detail: text || null, actions: ['copy'] }
    : { sentence: UNKNOWN_REFUSAL, detail: text || null, actions: ['retry', 'copy'] };
}
/**
 * The permanent refusal of this note's push (gotcha #14), from either place
 * it reaches the frontend: the note row's own `push_blocked_reason`, or the
 * persisted state of the CURRENT local version (how the worker's refusal
 * arrives before the row is refreshed). Null when the push is not refused.
 * Every surface that says "blocked" — the toolbar save state, the metadata
 * line's status, the persistence label — reads this one rule.
 */
export function syncBlock(note: Note, state: NotePersistence | undefined): { reason: string; byRemote: boolean } | null {
  if (note.push_blocked_reason) return { reason: note.push_blocked_reason, byRemote: !!note.push_blocked_by_remote };
  if (state?.push_blocked_reason && state.local_version === note.local_version)
    return { reason: state.push_blocked_reason, byRemote: !!state.push_blocked_by_remote };
  return null;
}
export function isSyncBlocked(note: Note, state: NotePersistence | undefined): boolean {
  return syncBlock(note, state) !== null;
}
/**
 * Where the saved note stands with its remote, as a short `word` (the
 * toolbar and the phone header) and the full `sentence` (its title and
 * accessible name). One set of branches for both, so the word can never
 * claim more than the sentence: "Saved" used to cover both synced and
 * "saved on this device · sync pending".
 *
 * A refused push reads "On this device" here, not "Not synced": the blocked
 * state and its Try again live in one place, the editor's refused-push notice
 * (the One Voice Rule, editor/editorStatus.ts).
 */
export function persistenceStatus(note: Note, state: NotePersistence | undefined, localFolder = false): { word: string; sentence: string } {
  if (isSyncBlocked(note, state)) return { word: 'On this device', sentence: 'Saved on this device · Sync blocked' };
  if (!state || state.local_version !== note.local_version) return { word: 'On this device', sentence: 'Saved on this device · Sync status unavailable' };
  if (state.sync_state === 'clean') return localFolder ? { word: 'Saved to folder', sentence: 'Saved to local folder' } : { word: 'Synced', sentence: 'Synced' };
  return localFolder
    ? { word: 'Write pending', sentence: 'Saved on this device · Folder write pending' }
    : { word: 'Sync pending', sentence: 'Saved on this device · Sync pending' };
}
export function persistenceLabel(note: Note, state: NotePersistence | undefined, localFolder = false): string {
  return persistenceStatus(note, state, localFolder).sentence;
}
