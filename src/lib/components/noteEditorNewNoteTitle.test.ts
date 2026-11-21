// @vitest-environment jsdom
//
// Live pass 2026-10-21 (Galaxy S23 FE): New note in a folder, tap the title,
// type — the text was appended to the literal "New Note" ("New Live pass…").
// The default title is a stand-in, not the user's text. It stays the stored
// default (the untouched-draft guards and every backend's title round trip,
// gotchas #11 / #17 / #27, rely on a non-empty title), but while the note is
// an untouched draft, focusing the title selects it, so typing replaces it.
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

const DRAFT: Note = {
  uuid: 'tmp:draft-1', id: '', account_id: ACCOUNT.id, title: 'New Note',
  body_html: '', date: '2026-10-21T00:00:00Z', label: '_test_',
} as Note;
const SAVED: Note = {
  uuid: 'uuid-saved', id: 'msg-1', account_id: ACCOUNT.id, title: 'New Note',
  body_html: '<p>kept the default on purpose</p>', date: '2026-10-21T00:00:00Z', label: '_test_',
} as Note;

let target: HTMLElement;
let host: ReturnType<typeof mount> | null = null;

async function open(note: Note) {
  notes.set([note]);
  selectedNote.set(note);
  host = mount(NoteEditor, { target });
  flushSync();
  await tick();
  flushSync();
}
const titleInput = () => target.querySelector<HTMLInputElement>('.title-input')!;
function focusTitle() {
  const el = titleInput();
  el.focus();
  flushSync();
}

describe('the title of a new note', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => cmd === 'note_connections'
      ? Promise.resolve({ outgoing: [], backlinks: [] })
      : Promise.resolve([]));
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    selectedFolder.set('_test_');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    target = document.createElement('div');
    document.body.appendChild(target);
  });
  afterEach(() => {
    if (host) unmount(host);
    host = null;
    target.remove();
    selectedNote.set(null);
    notes.set([]);
  });

  it('focusing it selects the default, so typing replaces it', async () => {
    await open(DRAFT);
    const el = titleInput();
    expect(el.value).toBe('New Note');
    focusTitle();
    expect([el.selectionStart, el.selectionEnd]).toEqual([0, 'New Note'.length]);
  });

  it('a tap that places the caret after the focus still leaves it selected', async () => {
    await open(DRAFT);
    const el = titleInput();
    focusTitle();
    // A touch tap's click lands after focus and puts the caret where the
    // finger was.
    el.setSelectionRange(3, 3);
    el.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();
    expect([el.selectionStart, el.selectionEnd]).toEqual([0, 'New Note'.length]);
    // Only that first tap: the next one places the caret as usual.
    el.setSelectionRange(3, 3);
    el.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();
    expect([el.selectionStart, el.selectionEnd]).toEqual([3, 3]);
  });

  it('a title the user typed is never selected on focus', async () => {
    await open(DRAFT);
    const el = titleInput();
    el.value = 'Live pass';
    el.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    el.blur();
    el.setSelectionRange(9, 9);
    focusTitle();
    expect([el.selectionStart, el.selectionEnd]).toEqual([9, 9]);
  });

  it('a saved note titled "New Note" is the user\'s title: not selected', async () => {
    await open(SAVED);
    const el = titleInput();
    el.setSelectionRange(8, 8);
    focusTitle();
    expect([el.selectionStart, el.selectionEnd]).toEqual([8, 8]);
  });
});
