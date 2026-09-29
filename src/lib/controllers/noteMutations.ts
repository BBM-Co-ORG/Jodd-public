import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import type { Note } from '../types';
import { noteKey, sameNote, canonicalNote } from '../noteIdentity';
import { notes, smartFolderNotes, selectedNote, selectedUuids, noteIndex, noteTagsByAccount,
  noteMutationRevision, pendingDeletedNoteKeys, getNoteTags, setNoteTags, indexRemoveOnDelete, error } from '../stores/notes';
export const deletingNotes = new Set<string>();
const deletionEpochs = new Map<string, number>();
export const deletionEpoch = (n: Note) => deletionEpochs.get(noteKey(n)) ?? 0;
export function beginNoteDeletion(n: Note) {
  const key = noteKey(n);
  deletionEpochs.set(key, deletionEpoch(n) + 1);
  deletingNotes.add(key);
}

/** Roll back only the deleted identity; never restore a whole stale list. */
export async function deleteNoteOptimistic(target: Note, accountId: string): Promise<boolean> {
  target = { ...target, account_id: accountId, uuid: canonicalNote({ ...target, account_id: accountId }).uuid };
  const key = noteKey(target);
  const snapshots = [notes, smartFolderNotes].map(store => ({ store, rows: get(store).filter(n => sameNote(n, target)) }));
  const selected = get(selectedNote);
  const wasSelected = sameNote(selected, target);
  const tags = getNoteTags(get(noteTagsByAccount), accountId, target.uuid);
  const currentId = get(notes).find(n => sameNote(n, target))?.id || target.id;
  const stub = get(noteIndex).get(accountId)?.find(n => n.id === currentId);
  const multiSelected = get(selectedUuids).has(key);
  beginNoteDeletion(target);
  pendingDeletedNoteKeys.update(s => new Set([...s, key]));
  snapshots.forEach(({ store }) => store.update(ns => ns.filter(n => !sameNote(n, target))));
  selectedUuids.update(s => { const n = new Set(s); n.delete(key); return n; });
  if (wasSelected) selectedNote.set(null);
  setNoteTags(accountId, target.uuid, []);
  indexRemoveOnDelete(accountId, currentId);
  try {
    if (target.uuid && !target.uuid.startsWith('tmp:')) await invoke('delete_note', { accountId, id: currentId, uuid: target.uuid });
    return true;
  } catch (e) {
    snapshots.forEach(({ store, rows }) => store.update(ns => ns.some(n => sameNote(n, target)) ? ns : [...ns, ...rows]));
    if (wasSelected && !get(selectedNote)) selectedNote.set(selected);
    if (multiSelected) selectedUuids.update(s => new Set([...s, key]));
    setNoteTags(accountId, target.uuid, tags);
    if (stub) noteIndex.update(m => { const rows = m.get(accountId); if (rows && !rows.some(n => n.id === stub.id)) m.set(accountId, [...rows, stub]); return m; });
    error.set(`Could not delete note: ${String(e)}`);
    return false;
  } finally {
    noteMutationRevision.update(n => n + 1);
    pendingDeletedNoteKeys.update(s => { const n = new Set(s); n.delete(key); return n; });
    // Allow the editor's selection reaction to finish before clearing the guard.
    queueMicrotask(() => deletingNotes.delete(key));
  }
}
