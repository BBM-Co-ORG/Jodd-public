// @vitest-environment jsdom
//
// A saved view in the middle pane (spec 2026-11-19-properties-tasks-views
// §6.5, §8.3, §8.5): the query's order is kept, a tasks view's rows carry
// their count and earliest due, a view that does not parse shows its errors
// and no list, and notices sit above the rows.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import {
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders, selectedTags,
  selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount, smartFolderNotes,
} from '../stores/notes';
import { openViewState, viewsByAccount } from '../views/viewsStore';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteList from './NoteList.svelte';

const A = 'gmail:a@b.c';
const note = (uuid: string, date: string, pinned = false): Note =>
  ({ id: `m-${uuid}`, uuid, title: `Note ${uuid}`, body_html: '<div>x</div>', date, label: 'Notes', account_id: A, pinned }) as Note;

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  notes.set([]);
  selectedNote.set(null);
  selectedFolder.set('Notes');
  currentAccount.set(A);
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  viewsByAccount.set({ [A]: [{ uuid: 'v1', name: 'Due soon', label: 'Notes/__Views__', lines: ['Show: tasks'], errors: [] }] });
  selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
  openViewState.set(null);
  smartFolderNotes.set([]);
  selectedSmartFolder.set(null);
});

function mountList() {
  component = mount(NoteList, { target: host, props: { width: 240 } });
  flushSync();
}

const titles = () => Array.from(host.querySelectorAll('.note-item .note-title')).map((e) => e.textContent?.trim());

describe('NoteList: a saved view', () => {
  it('keeps the query\'s order (no pinned-first, no newest-first) and shows each note\'s tasks and earliest due', () => {
    // Older first, and the pinned one last: a view sorted by due says so.
    smartFolderNotes.set([note('b', '2026-01-01T00:00:00Z'), note('a', '2026-11-01T00:00:00Z', true)]);
    openViewState.set({
      key: `${A}|v1`, errors: null, notices: [], show: 'tasks',
      meta: new Map([['b', { taskCount: 2, earliestDue: '2020-01-01' }], ['a', { taskCount: 1, earliestDue: null }]]),
    });
    mountList();
    expect(titles()).toEqual(['Note b', 'Note a']);
    expect(host.querySelector('.list-section')).toBeNull();
    const metas = Array.from(host.querySelectorAll('.view-tasks')).map((e) => e.textContent?.trim());
    expect(metas[0]).toMatch(/^2 tasks · Overdue, /);
    expect(metas[1]).toBe('1 task');
    expect(host.querySelector('.view-due')?.getAttribute('data-due')).toBe('overdue');
    expect(host.querySelector('.list-header h2, .list-title')?.textContent ?? '').not.toContain('[object');
  });

  it('a view that does not parse shows its errors and no list', () => {
    smartFolderNotes.set([note('stale', '2026-11-01T00:00:00Z')]);
    openViewState.set({
      key: `${A}|v1`, errors: [{ line: 1, key: 'due', message: '`Due: soon` is not a date' }], notices: [], show: 'notes', meta: new Map(),
    });
    mountList();
    const box = host.querySelector('.view-errors');
    expect(box?.textContent).toContain('Line 2');
    expect(box?.textContent).toContain('`Due: soon` is not a date');
    expect(host.querySelectorAll('.note-item')).toHaveLength(0);
  });

  it('shows the result\'s notices above the rows', () => {
    smartFolderNotes.set([]);
    openViewState.set({ key: `${A}|v1`, errors: null, notices: ['no note has the property `colour`'], show: 'notes', meta: new Map() });
    mountList();
    expect(host.querySelector('.view-notices')?.textContent).toContain('no note has the property `colour`');
    expect(host.querySelector('.empty-state')?.textContent?.trim()).toBe('No notes match this view.');
  });

  it('another view\'s state is not shown while this one runs', () => {
    smartFolderNotes.set([]);
    openViewState.set({ key: `${A}|other`, errors: [{ line: -1, key: '', message: 'boom' }], notices: [], show: 'notes', meta: new Map() });
    mountList();
    expect(host.querySelector('.view-errors')).toBeNull();
    expect(host.querySelector('.empty-state')?.textContent?.trim()).toBe('');
  });
});
