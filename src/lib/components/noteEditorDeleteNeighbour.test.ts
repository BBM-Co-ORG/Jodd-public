// @vitest-environment jsdom
//
// Deleting the open note while it has unsaved typing (desktop, spec §5.3):
// the editor swaps straight to the neighbour, and the swap's flush of the
// outgoing note's pending edit must NOT save the note being deleted — that
// would resurrect it. Mounted: the guard (`deletingNotes`) only matters on
// the editor's real render path.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';
import NoteList from './NoteList.svelte';
import * as stores from '../stores/notes';
import { deleteNoteOptimistic } from '../deleteNote';
import { persistenceByNote, pendingLocalEdits, savingNoteKeys } from '../notePersistence';
import { clearNoteAliases } from '../noteIdentity';

const ACCT = 'gmail:a@b.com';
const mk = (u: string, date: string): Note => ({
  uuid: u, id: `m-${u}`, account_id: ACCT, title: u.toUpperCase(), label: 'Notes',
  body_html: `<html><head></head><body><div>${u} body</div></body></html>`, date, local_version: 1,
} as Note);
const DOOMED = mk('doomed', '2026-10-09T00:00:00Z');
const NEXT = mk('next', '2026-10-08T00:00:00Z');

let host: HTMLElement;
let comps: ReturnType<typeof mount>[] = [];

async function settle() {
  await tick();
  await vi.advanceTimersByTimeAsync(0);
  flushSync();
  await tick();
}

beforeEach(() => {
  vi.useFakeTimers();
  clearNoteAliases();
  persistenceByNote.set({});
  pendingLocalEdits.set({});
  savingNoteKeys.set(new Set());
  Object.defineProperty(HTMLElement.prototype, 'innerText', { configurable: true, get() { return this.textContent ?? ''; } });
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(
    cmd === 'note_connections' ? { outgoing: [], backlinks: [] }
      : cmd === 'save_note' ? { id: 'saved', uuid: 'saved', local_version: 2 }
      : [],
  ));
  stores.accounts.set([]);
  stores.currentAccount.set(ACCT);
  stores.selectedFolder.set('Notes');
  stores.selectedSmartFolder.set(null);
  stores.selectedTags.set(new Set());
  stores.searchQuery.set('');
  stores.selectedUuids.set(new Set());
  stores.pendingDeletedNoteKeys.set(new Set());
  stores.isSaving.set(false);
  stores.notes.set([DOOMED, NEXT]);
  stores.selectedNote.set(DOOMED);
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  for (const c of comps) unmount(c);
  comps = [];
  host.remove();
  stores.selectedNote.set(null);
  vi.useRealTimers();
});

describe('deleting the open note with unsaved typing', () => {
  it('swaps to the neighbour and sends no save for the deleted note', async () => {
    comps = [mount(NoteList, { target: host }), mount(NoteEditor, { target: host })];
    await settle();
    const editor = host.querySelector('.editor-body') as HTMLElement;
    editor.focus();
    editor.innerHTML = '<div>doomed body</div><div>typed, not saved yet</div>';
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    await deleteNoteOptimistic(DOOMED, ACCT);
    await settle();
    // Past every autosave and flush timer.
    await vi.advanceTimersByTimeAsync(10_000);
    await settle();

    expect(get(stores.selectedNote)?.uuid).toBe('next');
    expect((host.querySelector('.editor-body') as HTMLElement).textContent).toBe('next body');
    expect(invoke).toHaveBeenCalledWith('delete_note', expect.objectContaining({ uuid: 'doomed' }));
    const saves = invoke.mock.calls.filter(([c]) => c === 'save_note');
    expect(saves.filter(([, a]) => (a as { existingUuid?: string }).existingUuid === 'doomed')).toEqual([]);
    expect(saves).toEqual([]);
  });
});
