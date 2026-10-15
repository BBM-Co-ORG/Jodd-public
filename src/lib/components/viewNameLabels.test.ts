// @vitest-environment jsdom
//
// The list header and the empty editor pane both name what the user is
// looking at. Measured live 2026-10-08: with a Smart Folder (Stale, Orphaned)
// selected, the header stayed on the last real folder ("Claude", from
// `Notes/__Claude__`) and the empty pane read "[object Object]" — it
// rendered `$selectedSmartFolder`, an `{ account, kind }` object, as text.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

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
  smartFolderNotes,
  searchQuery,
  selectedUuids,
  capabilitiesByAccount,
} from '../stores/notes';
import NoteList from './NoteList.svelte';
import NoteEditor from './NoteEditor.svelte';

const ACCT = 'gmail:a@b.com';

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  notes.set([]);
  selectedNote.set(null);
  selectedFolder.set('Notes/__Claude__');
  currentAccount.set(ACCT);
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  smartFolderNotes.set([]);
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  unmount(component);
  host.remove();
  selectedSmartFolder.set(null);
  selectedFolder.set('Notes');
});

describe('note list header', () => {
  const header = () => host.querySelector('.list-header h2')!.textContent!.trim();

  beforeEach(async () => {
    component = mount(NoteList, { target: host, props: { width: 360 } });
    await settle();
  });

  it('names a workflow folder by its leaf, markers stripped', () => {
    expect(header()).toBe('Claude');
  });

  it('names the Smart Folder, not the folder selected before it', async () => {
    selectedSmartFolder.set({ account: ACCT, kind: 'stale' });
    await settle();
    expect(header()).toBe('Stale');

    selectedSmartFolder.set({ account: ACCT, kind: 'orphaned' });
    await settle();
    expect(header()).toBe('Orphaned');
  });

  it('goes back to the folder when the Smart Folder is cleared', async () => {
    selectedSmartFolder.set({ account: ACCT, kind: 'stale' });
    await settle();
    selectedSmartFolder.set(null);
    await settle();
    expect(header()).toBe('Claude');
  });
});

describe('empty editor pane', () => {
  const context = () => host.querySelector('.empty-context')!.textContent!.trim();

  beforeEach(async () => {
    component = mount(NoteEditor, { target: host });
    await settle();
  });

  it('names the Smart Folder instead of printing the selection object', async () => {
    selectedSmartFolder.set({ account: ACCT, kind: 'orphaned' });
    await settle();
    expect(context()).toBe('Orphaned');
    expect(context()).not.toContain('[object');
  });

  // The full path, as before: it pairs with the editor's "Browsing …"
  // context notice (packageH.test.ts pins that).
  it('names a folder by its full path', () => {
    expect(context()).toBe('Notes/__Claude__');
  });

  it('names the All row instead of printing its sentinel', async () => {
    selectedFolder.set('__ALL__');
    await settle();
    expect(context()).not.toContain('__ALL__');
    expect(context()).toMatch(/^All /);
  });
});
