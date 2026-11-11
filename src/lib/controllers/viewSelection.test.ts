// @vitest-environment jsdom
//
// The view-selection steps shared by the Sidebar and the phone panes: each is
// one synchronous store write set, with no pane navigation of its own.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async () => []) }));

import {
  currentAccount, searchQuery, selectedFolder, selectedNote, selectedSmartFolder, selectedTags,
  tagMatchMode, notes,
} from '../stores/notes';
import { activePane } from '../stores/phoneNav';
import { selectFolder, toggleTag, selectOnlyTags, selectSmartFolder, openNewNote } from './viewSelection';
import type { Note } from '../types';

const OPEN = { uuid: 'x', id: 'm', account_id: 'gmail:a', title: 'x', body_html: '', date: '', label: 'Notes' } as Note;

beforeEach(() => {
  currentAccount.set('gmail:a');
  searchQuery.set('milk');
  selectedFolder.set('Notes/Work');
  selectedSmartFolder.set(null);
  selectedTags.set(new Set(['old']));
  tagMatchMode.set('AND');
  selectedNote.set(OPEN);
  notes.set([]);
  activePane.set('folders');
});

describe('viewSelection', () => {
  it('selectFolder: account, exits search, leaves tags and Smart Folder, clears the note', () => {
    selectedSmartFolder.set({ account: 'gmail:a', kind: 'stale' });
    selectFolder('gmail:b', 'Notes/Home');
    expect(get(currentAccount)).toBe('gmail:b');
    expect(get(searchQuery)).toBe('');
    expect(get(selectedFolder)).toBe('Notes/Home');
    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedNote)).toBeNull();
  });

  it('toggleTag: toggles within the account, starts fresh in another, keeps the search', () => {
    toggleTag('gmail:a', 'new');
    expect([...get(selectedTags)]).toEqual(['old', 'new']);
    toggleTag('gmail:a', 'old');
    expect([...get(selectedTags)]).toEqual(['new']);
    toggleTag('gmail:b', 'other');
    expect([...get(selectedTags)]).toEqual(['other']);
    expect(get(searchQuery)).toBe('milk');
  });

  it('selectOnlyTags: exactly these tags in order, with the mode, search and Smart Folder left', () => {
    selectedSmartFolder.set({ account: 'gmail:a', kind: 'stale' });
    selectOnlyTags('gmail:a', ['b', 'a'], 'OR');
    expect([...get(selectedTags)]).toEqual(['b', 'a']);
    expect(get(tagMatchMode)).toBe('OR');
    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(searchQuery)).toBe('');
    expect(get(selectedNote)).toBeNull();
  });

  it('selectSmartFolder: leaves the folder value untouched', () => {
    selectSmartFolder('gmail:b', 'unreviewed');
    expect(get(selectedSmartFolder)).toEqual({ account: 'gmail:b', kind: 'unreviewed' });
    expect(get(selectedFolder)).toBe('Notes/Work');
    expect(get(selectedTags).size).toBe(0);
    expect(get(searchQuery)).toBe('');
  });

  it('openNewNote: a tmp note in that folder, opened, with the folder', () => {
    const n = openNewNote('gmail:b', 'Notes/Home');
    expect(n.uuid.startsWith('tmp:')).toBe(true);
    expect(n.label).toBe('Notes/Home');
    expect(get(currentAccount)).toBe('gmail:b');
    expect(get(selectedFolder)).toBe('Notes/Home');
    expect(get(notes)[0]).toBe(n);
    expect(get(selectedNote)).toBe(n);
  });

  it('none of them navigates panes', () => {
    selectFolder('gmail:a', 'Notes');
    toggleTag('gmail:a', 't');
    selectOnlyTags('gmail:a', ['t'], 'AND');
    selectSmartFolder('gmail:a', 'stale');
    openNewNote('gmail:a', 'Notes');
    expect(get(activePane)).toBe('folders');
  });
});
