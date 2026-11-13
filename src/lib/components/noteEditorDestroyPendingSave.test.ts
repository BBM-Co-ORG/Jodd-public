// @vitest-environment jsdom
//
// An editor destroyed with an autosave still pending (typing within 1.5 s of
// a rotation, which swaps App's per-layout editors, or of the phone's Back
// from the note pane) left its timer running. It fired later on the dead
// editor and saved that editor's body and title into `$selectedNote` — the
// note selected THEN, which may be another one. Found 2026-10-21: an earlier
// test's stray autosave landed inside tagsPopover.test.ts's rotation test.
// The pending edit is now saved at destroy, against the editor's own note.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(true),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('phone'),
}));

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { accounts, currentAccount, selectedNote, selectedFolder, selectedSmartFolder, selectedTags, notes } from '../stores/notes';
import type { Note } from '../types';
import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' };
const doc = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const A: Note = {
  uuid: 'uuid-a', id: 'msg-a', account_id: ACCOUNT.id, title: 'Note A',
  body_html: doc('<div>alpha text here</div>'), date: '2026-10-21T00:00:00Z', label: 'Notes', local_version: 1,
} as Note;
const B: Note = {
  uuid: 'uuid-b', id: 'msg-b', account_id: ACCOUNT.id, title: 'Note B',
  body_html: doc('<div>bravo text here</div>'), date: '2026-10-21T00:00:00Z', label: 'Notes', local_version: 1,
} as Note;

interface SaveArgs { existingUuid: string | null; title: string; bodyHtml: string }
const saves = () => invoke.mock.calls.filter(([c]) => c === 'save_note').map(([, a]) => a as SaveArgs);

describe('an editor destroyed with an autosave pending', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'save_note') return Promise.resolve({ id: `msg-${args.existingUuid}`, uuid: args.existingUuid, local_version: 2 });
      return Promise.resolve([]);
    });
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    notes.set([A, B]);
    selectedNote.set(A);
  });
  afterEach(() => {
    vi.useRealTimers();
    selectedNote.set(null);
    notes.set([]);
  });

  it('saves the pending edit to its own note at once, and never to the note selected later', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await vi.advanceTimersByTimeAsync(0);
    await tick();
    const editor = target.querySelector('.editor-body') as HTMLElement;
    editor.innerHTML = '<div>alpha text here, edited</div>';
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    // Destroyed before the 1.5 s debounce, then another note is opened.
    unmount(host);
    target.remove();
    selectedNote.set(B);
    await vi.advanceTimersByTimeAsync(2000);
    await tick();

    // Nothing of A's ever lands on B.
    expect(saves().filter((s) => s.existingUuid === 'uuid-b')).toEqual([]);
    // A's edit is not lost: saved against A.
    const forA = saves().filter((s) => s.existingUuid === 'uuid-a');
    expect(forA).toHaveLength(1);
    expect(forA[0].bodyHtml).toContain('alpha text here, edited');
    expect(forA[0].title).toBe('Note A');
  });

  it('a merely viewed note is not saved when its editor goes', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await vi.advanceTimersByTimeAsync(0);
    await tick();
    unmount(host);
    target.remove();
    await vi.advanceTimersByTimeAsync(2000);
    expect(saves()).toEqual([]);
  });
});
