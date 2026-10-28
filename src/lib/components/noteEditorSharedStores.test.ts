// @vitest-environment jsdom
//
// Live pass 2026-10-21 (Galaxy S23 FE): after a phone → tablet → phone
// rotation with a note open, the phone note header lost "Saved" and its ⋯
// until the note was reopened. App swaps the tablet layout for the phone one,
// so for a moment two NoteEditors exist: the incoming one publishes its save
// state, ⋯ menu and external-change banner into shared stores, then the
// outgoing one is destroyed — and its onDestroy used to null them
// unconditionally, wiping the newcomer's values.
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
import { editorSaveState, editorExternalChange } from '../stores/editorSaveState';
import { noteActionsMenu } from '../stores/noteActionsMenu';
import type { Note } from '../types';
import NoteEditor from './NoteEditor.svelte';
import LayoutSwapEditor from './__fixtures__/LayoutSwapEditor.svelte';
import { androidLayoutMode } from '../stores/viewport';
import type { Writable } from 'svelte/store';

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
  title: 'A note',
  body_html: '<p>body</p>',
  date: '2026-10-08T00:00:00Z',
  label: 'Notes',
} as Note;

async function mountEditor() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(NoteEditor, { target });
  flushSync();
  await tick();
  flushSync();
  return {
    destroy() {
      unmount(host);
      target.remove();
    },
  };
}

describe('NoteEditor shared stores survive a layout swap', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => cmd === 'note_connections'
      ? Promise.resolve({ outgoing: [], backlinks: [] })
      : Promise.resolve([]));
    accounts.set([ACCOUNT]);
    currentAccount.set(ACCOUNT.id);
    notes.set([NOTE]);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    selectedNote.set(NOTE);
  });

  it('the outgoing editor destroyed after the incoming one published leaves the incoming values', async () => {
    const outgoing = await mountEditor();
    const incoming = await mountEditor();
    const save = get(editorSaveState);
    const menu = get(noteActionsMenu);
    expect(save).not.toBeNull();
    expect(menu).not.toBeNull();

    outgoing.destroy();
    flushSync();

    // The phone header still reads the live editor's state and can open its ⋯.
    expect(get(editorSaveState)).toEqual(save);
    expect(get(noteActionsMenu)).not.toBeNull();

    incoming.destroy();
    flushSync();
    // The last editor gone: nothing left to show.
    expect(get(editorSaveState)).toBeNull();
    expect(get(noteActionsMenu)).toBeNull();
    expect(get(editorExternalChange)).toBeNull();
  });

  it('destroying the newer editor first falls back to the older one', async () => {
    const older = await mountEditor();
    const newer = await mountEditor();
    newer.destroy();
    flushSync();
    expect(get(editorSaveState)).not.toBeNull();
    expect(get(noteActionsMenu)).not.toBeNull();
    older.destroy();
    flushSync();
    expect(get(editorSaveState)).toBeNull();
    expect(get(noteActionsMenu)).toBeNull();
  });

  it('a real phone → tablet → phone swap (App\'s per-layout editors) leaves the phone header its state and ⋯', async () => {
    const layout = androidLayoutMode as unknown as Writable<string>;
    layout.set('phone');
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(LayoutSwapEditor, { target });
    flushSync();
    await tick();
    expect(get(noteActionsMenu)).not.toBeNull();
    layout.set('tablet');
    flushSync();
    await tick();
    // The toolbar shows ⋯ on the tablet: the header's is not published.
    expect(get(noteActionsMenu)).toBeNull();
    expect(get(editorSaveState)).not.toBeNull();
    layout.set('phone');
    flushSync();
    await tick();
    expect(get(editorSaveState)).not.toBeNull();
    expect(get(noteActionsMenu)).not.toBeNull();
    unmount(host);
    target.remove();
    layout.set('phone');
  });
});
