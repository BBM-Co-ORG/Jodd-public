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
    local_version: note.local_version!, sync_state: 'dirty', push_blocked_reason: null } }));
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
    if (selected && sameNote(selected, state) && selected.push_blocked_reason !== state.push_blocked_reason)
      selectedNote.set({ ...selected, push_blocked_reason: state.push_blocked_reason });
    return state;
  } catch { /* Offline SQLite/IPC failure is unknown, never a sync success. */ }
}
export function persistenceLabel(note: Note, state: NotePersistence | undefined, localFolder = false): string {
  if (note.push_blocked_reason || state?.push_blocked_reason && state.local_version === note.local_version)
    return 'Saved on this device · Sync blocked';
  if (!state || state.local_version !== note.local_version) return 'Saved on this device · Sync status unavailable';
  if (state.sync_state === 'clean') return localFolder ? 'Saved to local folder' : 'Synced';
  return localFolder ? 'Saved on this device · Folder write pending' : 'Saved on this device · Sync pending';
}
