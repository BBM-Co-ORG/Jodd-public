// @vitest-environment jsdom
//
// Live pass 2026-10-21 (S23 FE): the phone ⋯ note-actions sheet closed on a
// rotation round trip, where PR #211's check 2.5 expected it to stay open
// until Back. Decided: the close is intended. The sheet belongs to the phone
// layout's NoteEditor (its NoteMoreMenu), and App mounts one editor per
// layout, so a rotation replaces the editor that owns it — and the tablet
// layout has no header ⋯ to hang it on (its ⋯ is an anchored popover in the
// toolbar). It closes the way the tags sheet does (R26, #211 item 6).
//
// What must hold either way: the sheet's history entry goes with it, so the
// first Back after the round trip goes straight to the list.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, writable, type Writable } from 'svelte/store';

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
import { noteActionsMenu } from '../stores/noteActionsMenu';
import {
  activePane,
  depthFromHistoryState,
  handlePopState,
  initPhoneNavHistory,
  navigateToPane,
  overlayFromHistoryState,
} from '../stores/phoneNav';
import { androidLayoutMode } from '../stores/viewport';
import type { Note } from '../types';
import LayoutSwapEditor from './__fixtures__/LayoutSwapEditor.svelte';

const layout = androidLayoutMode as unknown as Writable<string>;

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' };
const NOTE: Note = {
  uuid: 'uuid-sheet-rotation',
  id: 'msg-1',
  account_id: ACCOUNT.id,
  title: 'A note',
  body_html: '<p>body</p>',
  date: '2026-10-08T00:00:00Z',
  label: 'Notes',
} as Note;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error('waitFor: condition did not become true in time');
    await new Promise((r) => setTimeout(r, 10));
  }
}

const openDialogs = () => document.querySelectorAll('dialog[open]').length;

describe('phone ⋯ sheet across a rotation round trip', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) =>
      cmd === 'note_connections' ? Promise.resolve({ outgoing: [], backlinks: [] }) : Promise.resolve([]),
    );
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    notes.set([NOTE]);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    selectedNote.set(NOTE);
    layout.set('phone');
    window.addEventListener('popstate', handlePopState);
  });
  afterEach(async () => {
    layout.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it('closes with its entry, and the first Back afterwards goes to the list', async () => {
    history.replaceState(null, '', location.href);
    initPhoneNavHistory('Notes'); // [folders, list]
    navigateToPane('note'); // [folders, list, note]
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(LayoutSwapEditor, { target });
    flushSync();
    await tick();

    const anchor = document.createElement('button');
    document.body.appendChild(anchor);
    get(noteActionsMenu)!.toggle(anchor);
    flushSync();
    await tick();
    expect(openDialogs()).toBe(1);
    expect(overlayFromHistoryState(history.state)).not.toBeNull();

    layout.set('tablet');
    flushSync();
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    layout.set('phone');
    flushSync();
    await tick();
    await waitFor(() => depthFromHistoryState(history.state) === 2);

    expect(openDialogs()).toBe(0);
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');

    history.back(); // Android Back
    await waitFor(() => get(activePane) === 'list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });

    unmount(host);
    target.remove();
    anchor.remove();
  });
});
