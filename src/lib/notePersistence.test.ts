import { beforeEach, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';
import { notes, selectedNote } from './stores/notes';
import { persistenceByNote, recordLocalSave, refreshNotePersistence, persistenceLabel } from './notePersistence';
import { noteKey, clearNoteAliases } from './noteIdentity';
import type { Note } from './types';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const a: Note = { account_id: 'a', uuid: 'u', id: 'id', title: 'x', body_html: 'x', label: 'Notes', date: '', local_version: 2 };
const state = (sync_state: 'dirty' | 'clean', version = 2) => ({ account_id: 'a', uuid: 'u', local_version: version, sync_state, push_blocked_reason: null });
beforeEach(() => { clearNoteAliases(); persistenceByNote.set({}); notes.set([a]); selectedNote.set(a); invoke.mockReset(); });
it('offline save and transient failure remain pending, successful push becomes synced', async () => {
  recordLocalSave(a); invoke.mockRejectedValue(new Error('offline')); await refreshNotePersistence(a);
  expect(persistenceLabel(a, get(persistenceByNote)[noteKey(a)])).toContain('Sync pending');
  invoke.mockResolvedValue(state('dirty')); await refreshNotePersistence(a);
  expect(persistenceLabel(a, get(persistenceByNote)[noteKey(a)])).toContain('Sync pending');
  invoke.mockResolvedValue(state('clean')); await refreshNotePersistence(a);
  expect(persistenceLabel(a, get(persistenceByNote)[noteKey(a)])).toBe('Synced');
});
it('an earlier push/read cannot mark a newer saved edit synced', async () => {
  let resolve!: (s: unknown) => void;
  invoke.mockReturnValue(new Promise(r => { resolve = r; }));
  const read = refreshNotePersistence(a), newer = { ...a, local_version: 3 };
  recordLocalSave(newer); resolve(state('clean')); await read;
  expect(persistenceLabel(newer, get(persistenceByNote)[noteKey(newer)])).toContain('Sync pending');
  invoke.mockResolvedValue(state('clean')); await refreshNotePersistence(newer);
  expect(get(persistenceByNote)[noteKey(newer)].local_version).toBe(3);
});
it('reopen and local folder labels require persisted evidence', async () => {
  expect(persistenceLabel(a, undefined)).toContain('unavailable');
  invoke.mockResolvedValue(state('dirty')); await refreshNotePersistence(a);
  expect(persistenceLabel(a, get(persistenceByNote)[noteKey(a)], true)).toContain('Folder write pending');
  invoke.mockResolvedValue(state('clean')); await refreshNotePersistence(a);
  expect(persistenceLabel(a, get(persistenceByNote)[noteKey(a)], true)).toBe('Saved to local folder');
  expect(persistenceLabel({ ...a, local_version: 3 }, state('clean'))).not.toBe('Synced');
});
it('refuses the other account and out-of-order replies', async () => {
  let resolve!: (s: unknown) => void;
  invoke.mockReturnValueOnce(new Promise(r => { resolve = r; })).mockResolvedValueOnce(state('dirty'));
  const first = refreshNotePersistence(a); await refreshNotePersistence(a); resolve(state('clean')); await first;
  expect(get(persistenceByNote)[noteKey(a)].sync_state).toBe('dirty');
  invoke.mockResolvedValue({ ...state('clean'), account_id: 'b' }); await refreshNotePersistence(a);
  expect(get(persistenceByNote)[noteKey(a)].sync_state).toBe('dirty');
});
it('uses unambiguous tuple keys even when account and UUID contain separators', () => {
  expect(noteKey({ account_id: 'a:b', uuid: 'c' })).not.toBe(noteKey({ account_id: 'a', uuid: 'b:c' }));
});
