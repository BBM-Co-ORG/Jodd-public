// @vitest-environment jsdom
//
// The list header names the view the list shows. Galaxy Tab S7 live pass of
// v0.35.2 (2026-10-21): with the _test_ folder selected, picking the tag
// #ai-agents in the drawer made the top bar read "#ai-agents" while the list
// header still said "_test_" — the header named the folder underneath the tag
// filter, not the view.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { writable, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(false),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('phone'),
}));

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import {
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders,
  selectedTags, selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount,
} from '../stores/notes';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as androidLayoutModeReadonly } from '../stores/viewport';
import type { Note } from '../types';
import NoteList from './NoteList.svelte';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const androidLayoutMode = androidLayoutModeReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

const ACCT = 'gmail:a@b.com';
const NOTE = {
  uuid: 'n1', id: 'm1', account_id: ACCT, title: 'One', body_html: '<p>x</p>',
  date: '2026-10-21T02:20:00Z', label: 'Notes/_test_',
} as Note;

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  isAndroid.set(false);
  androidLayoutMode.set('phone');
  notes.set([NOTE]);
  selectedFolder.set('Notes/_test_');
  currentAccount.set(ACCT);
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  selectedNote.set(null);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(NoteList, { target: host, props: { width: 360 } });
  flushSync();
});

afterEach(() => {
  unmount(component);
  host.remove();
  selectedTags.set(new Set());
});

const heading = () => host.querySelector('.list-header h2') as HTMLElement | null;

describe('the list header names the view', () => {
  it('a folder by its leaf', () => {
    expect(heading()?.textContent).toBe('_test_');
  });

  it('a tag view by its tag, not the folder underneath it', () => {
    selectedTags.set(new Set(['ai-agents']));
    flushSync();
    expect(heading()?.textContent).toBe('#ai-agents');
  });

  it('several tags in selection order, the form the top bar uses', () => {
    selectedTags.set(new Set(['ai-agents', 'llm']));
    flushSync();
    expect(heading()?.textContent).toBe('#ai-agents #llm');
  });
});

// Tab S7 (2026-10-21): the top bar read "☰ Notes" and the list header right
// under it "Notes ⟳ ＋". On tablet the bar names the view; the list header
// keeps its actions, and still says what a search found.
describe('tablet: the top bar names the view, the list header does not', () => {
  for (const mode of ['tablet', 'tabletWide'] as const) {
    it(`${mode}: no view name in the list header, ⟳ and + stay`, () => {
      isAndroid.set(true);
      androidLayoutMode.set(mode);
      flushSync();
      expect(heading()).toBeNull();
      expect(host.querySelector('.list-header')?.textContent).not.toContain('_test_');
      expect(host.querySelector('.list-header button[aria-label="Refresh notes"]')).not.toBeNull();
      expect(host.querySelector('.list-header button[aria-label="New note"]')).not.toBeNull();
    });
  }

  it('tablet: a search still shows its state in the list header', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('tablet');
    invoke.mockImplementation(async (cmd: string) => (cmd === 'search_notes' ? [NOTE] : []));
    searchQuery.set('one');
    flushSync();
    await vi.waitFor(() => expect(heading()?.textContent).toMatch(/Results: 1|Searching/));
    searchQuery.set('');
  });

  it('desktop keeps the view name: it has no top bar', () => {
    expect(heading()?.textContent).toBe('_test_');
  });
});
