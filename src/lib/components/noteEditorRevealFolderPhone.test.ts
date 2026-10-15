// @vitest-environment jsdom
//
// Phone layout: the breadcrumb's folder button must land on the list pane.
// App.svelte's folder watcher moves panes only when selectedFolder CHANGES —
// leaving a Smart Folder for the folder already underneath it changes nothing
// there, so the user stayed on the note pane with the list behind it.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, writable } from 'svelte/store';

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
import { activePane } from '../stores/phoneNav';
import type { Note } from '../types';
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

describe('NoteEditor breadcrumb folder button — phone', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => cmd === 'note_connections'
      ? Promise.resolve({ outgoing: [], backlinks: [] })
      : Promise.resolve([]));
    accounts.set([ACCOUNT]);
    currentAccount.set(ACCOUNT.id);
    notes.set([NOTE]);
    selectedFolder.set('Notes');
    selectedSmartFolder.set({ account: ACCOUNT.id, kind: 'unreviewed' });
    selectedTags.set(new Set());
    selectedNote.set(NOTE);
    history.replaceState({ pane: 'note', depth: 2 }, '', location.href);
    activePane.set('note');
  });

  it('shows the list pane when leaving a Smart Folder for the folder underneath', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();
    flushSync();

    target.querySelector<HTMLButtonElement>('.ctx-folder')!.click();
    flushSync();

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(activePane)).toBe('list');

    unmount(host);
    target.remove();
  });
});
