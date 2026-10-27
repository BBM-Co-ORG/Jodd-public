// @vitest-environment jsdom
//
// The editor's empty state (spec 2026-10-20 §5.5), with the real NoteList
// beside it reporting what it shows: an empty folder names itself, says where
// new notes sync and offers New note with its shortcut (none in a read-only
// account, one sentence instead); a search with no results offers Clear
// search; a list that has notes but none open asks to select one.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import type { Note } from '../types';

let platform = 'windows';
let searchHits: Note[] = [];
const invoke = vi.fn(async (cmd: string): Promise<unknown> => {
  if (cmd === 'platform_name') return platform;
  if (cmd === 'search_notes') return searchHits;
  if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
  return [];
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (cmd: string) => invoke(cmd) }));

import NoteEditor from './NoteEditor.svelte';
import NoteList from './NoteList.svelte';
import {
  accounts, currentAccount, notes, selectedFolder, selectedNote, selectedSmartFolder, selectedTags,
  searchQuery, capabilitiesByAccount, smartFolderNotes, noteIndex, hydratedFolders,
} from '../stores/notes';

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', backend_kind: 'gmail', status: 'active', added_at: '' };
const OUTLOOK = { id: 'microsoft:c@d.com', email: 'c@d.com', backend_kind: 'microsoft', status: 'active', added_at: '' };
const NOTE = {
  uuid: 'u1', id: 'm1', account_id: GMAIL.id, title: 'Groceries', label: 'Notes/Home',
  body_html: '<html><head></head><body><div>Groceries</div></body></html>', date: '2026-10-01T00:00:00Z',
} as Note;

let host: HTMLElement;
let comps: ReturnType<typeof mount>[] = [];

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

async function show() {
  host = document.createElement('div');
  document.body.appendChild(host);
  comps = [mount(NoteList, { target: host }), mount(NoteEditor, { target: host })];
  await settle();
}

const pane = () => host.querySelector('.empty-editor') as HTMLElement;
const text = () => (pane()?.textContent ?? '').replace(/\s+/g, ' ').trim();
const button = (name: string) =>
  Array.from(pane().querySelectorAll('button')).find((b) => b.textContent?.includes(name)) as HTMLButtonElement | undefined;

beforeEach(() => {
  platform = 'windows';
  searchHits = [];
  accounts.set([GMAIL, OUTLOOK] as never);
  currentAccount.set(GMAIL.id);
  capabilitiesByAccount.set({});
  selectedFolder.set('Notes/Empty');
  selectedSmartFolder.set(null);
  smartFolderNotes.set([]);
  selectedTags.set(new Set());
  searchQuery.set('');
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  notes.set([NOTE]);
  selectedNote.set(null);
});

afterEach(() => {
  for (const c of comps) unmount(c);
  comps = [];
  host.remove();
  selectedNote.set(null);
});

describe('editor empty state', () => {
  it('an empty folder: its name, where notes sync, and New note with Ctrl+N', async () => {
    await show();
    expect(pane().querySelector('.empty-headline')?.textContent).toBe('No notes in Empty');
    expect(text()).toContain('Notes you add here sync to Gmail.');
    const btn = button('New note')!;
    expect(btn.textContent?.replace(/\s+/g, ' ').trim()).toBe('New note Ctrl+N');
    expect(btn.classList.contains('primary')).toBe(true);
    btn.click();
    flushSync();
    expect(get(selectedNote)?.uuid.startsWith('tmp:')).toBe(true);
    expect(get(selectedNote)?.label).toBe('Notes/Empty');
  });

  it('on macOS the hint is Cmd+N', async () => {
    platform = 'macos';
    await show();
    expect(button('New note')?.textContent?.replace(/\s+/g, ' ').trim()).toBe('New note Cmd+N');
  });

  it('a read-only account: no button, one sentence saying so', async () => {
    currentAccount.set(OUTLOOK.id);
    capabilitiesByAccount.set({ [OUTLOOK.id]: { has_trash: false, writes: { notes: false, relocate: false, folders: false, sidecars: false } } });
    await show();
    expect(pane().querySelector('.empty-headline')?.textContent).toBe('No notes in Empty');
    expect(text()).toContain('This Outlook account is read-only in Jodd.');
    expect(text()).not.toContain('Notes you add here');
    expect(pane().querySelector('button')).toBeNull();
  });

  it('an account not loaded yet: no guess at where notes sync, New note still offered', async () => {
    accounts.set([]);
    await show();
    expect(pane().querySelector('.empty-headline')?.textContent).toBe('No notes in Empty');
    expect(text()).not.toContain('sync to');
    expect(text()).not.toContain('Gmail');
    expect(button('New note')).toBeTruthy();
  });

  it('a search with no results: No notes match "q" and Clear search', async () => {
    selectedFolder.set('Notes/Home');
    await show();
    searchQuery.set('zzz');
    await vi.waitFor(async () => {
      await settle();
      expect(text()).toBe('No notes match "zzz" Clear search');
    });
    button('Clear search')!.click();
    flushSync();
    expect(get(searchQuery)).toBe('');
    await settle();
    expect(text()).toBe('Select a note Notes/Home');
  });

  it('a folder with notes but none open: Select a note and the view path', async () => {
    selectedFolder.set('Notes/Home');
    await show();
    expect(text()).toBe('Select a note Notes/Home');
  });

  it('an empty Smart Folder names itself and offers no New note', async () => {
    selectedSmartFolder.set({ account: GMAIL.id, kind: 'orphaned' });
    await show();
    expect(text()).toBe('No notes in Orphaned');
  });

  it('a folder still loading does not claim to be empty', async () => {
    noteIndex.set(new Map([[GMAIL.id, [{ id: 'x', label: 'Notes/Empty' }]]]));
    await show();
    expect(text()).toBe('Select a note Notes/Empty');
  });
});
