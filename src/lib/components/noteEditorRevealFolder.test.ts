// @vitest-environment jsdom
//
// The breadcrumb's folder button ("Reveal folder in sidebar") is navigation:
// it must leave a Smart Folder. NoteList shows a Smart Folder before it looks
// at selectedFolder, and the Sidebar lights no folder row while one is active
// (PR #140) — so setting selectedFolder alone did nothing visible. Opening a
// note from Unreviewed and clicking its folder is exactly how one gets there.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { accounts, currentAccount, selectedNote, selectedFolder, selectedSmartFolder, selectedTags, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = {
  id: 'gmail:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'gmail',
  status: 'active',
};

const NOTE: Note = {
  uuid: 'uuid-under-test',
  id: 'msg-1',
  account_id: ACCOUNT.id,
  title: 'An unreviewed note',
  body_html: '<p>body</p>',
  date: '2026-10-08T00:00:00Z',
  label: 'Notes',
} as Note;

describe('NoteEditor breadcrumb folder button', () => {
  beforeEach(() => {
    invoke.mockReset();
    // note_connections' caller destructures { outgoing, backlinks }.
    invoke.mockImplementation((cmd: string) => cmd === 'note_connections'
      ? Promise.resolve({ outgoing: [], backlinks: [] })
      : Promise.resolve([]));
    accounts.set([ACCOUNT]);
    currentAccount.set(ACCOUNT.id);
    notes.set([NOTE]);
    // The folder underneath the Smart Folder is the note's own folder — the
    // case where a bare `selectedFolder.set` is not even a store change.
    selectedFolder.set('Notes');
    selectedSmartFolder.set({ account: ACCOUNT.id, kind: 'unreviewed' });
    selectedTags.set(new Set());
    selectedNote.set(NOTE);
  });

  it('leaves a Smart Folder for the note\'s folder', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();
    flushSync();

    target.querySelector<HTMLButtonElement>('.ctx-folder')!.click();
    flushSync();

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes');

    unmount(host);
    target.remove();
  });

  // A note opened from a tag view: the folder button must leave the filter,
  // which NoteList shows ahead of the folder.
  it('leaves a tag filter for the note\'s folder', async () => {
    selectedSmartFolder.set(null);
    selectedTags.set(new Set(['trading']));
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();
    flushSync();

    target.querySelector<HTMLButtonElement>('.ctx-folder')!.click();
    flushSync();

    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes');

    unmount(host);
    target.remove();
  });
});
