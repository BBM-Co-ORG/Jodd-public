// @vitest-environment jsdom
//
// On the phone layout, App.svelte pushes the note pane only when $selectedNote
// changes to a DIFFERENT uuid. A note that is already selected — which is
// every note Extract or URL ingest just created, since both select it — could
// therefore never be opened by tapping it: the tap re-set the same uuid and
// nothing navigated. Measured on an Android phone, 2026-09-16.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
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

import {
  notes,
  selectedNote,
  selectedFolder,
  currentAccount,
  accounts,
  noteIndex,
  hydratedFolders,
  selectedTags,
  selectedSmartFolder,
  searchQuery,
  selectedUuids,
  capabilitiesByAccount,
} from '../stores/notes';
import { activePane } from '../stores/phoneNav';
import { androidLayoutMode as androidLayoutModeReadonly } from '../stores/viewport';
import type { Writable } from 'svelte/store';
import type { Note } from '../types';
import NoteList from './NoteList.svelte';

const androidLayoutMode = androidLayoutModeReadonly as unknown as Writable<'phone' | 'tablet'>;

const ACCT = 'gmail:a@b.com';
const NOTE = {
  uuid: 'ingested', id: 'm1', account_id: ACCT, title: 'Ingested', body_html: '<p>x</p>',
  date: '2026-09-16T02:20:00Z', label: 'Notes/Inbox',
} as Note;

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  androidLayoutMode.set('phone');
  history.replaceState({ pane: 'list', depth: 0 }, '', location.href);
  activePane.set('list');
  notes.set([NOTE]);
  selectedFolder.set('Notes/Inbox');
  currentAccount.set(ACCT);
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  selectedNote.set(NOTE);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(NoteList, { target: host, props: { width: 360 } });
  flushSync();
});

afterEach(() => {
  unmount(component);
  host.remove();
});

function tapNote() {
  (host.querySelector('.note-btn') as HTMLElement).click();
  flushSync();
}

describe('tapping a note on the phone layout', () => {
  it('opens a note that is already selected', () => {
    tapNote();
    expect(get(activePane)).toBe('note');
  });

  it('leaves the panes alone off the phone layout', () => {
    androidLayoutMode.set('tablet');
    tapNote();
    expect(get(activePane)).toBe('list');
  });
});
