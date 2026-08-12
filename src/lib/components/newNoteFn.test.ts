// @vitest-environment jsdom
//
// Contract test for the ⌘N wiring.
//
// ⌘N is handled in App.svelte's global keydown handler, but the note it
// creates depends on $selectedFolder / $currentAccount scoping that lives in
// NoteList. Rather than lift that logic up, NoteList publishes newNote() via
// the `newNoteFn` function-pointer store (the same idiom as `refreshNotes`,
// pointing the other way) and App calls it.
//
// That indirection is invisible at the call site: if NoteList ever stops
// setting the pointer — a refactor moving newNote(), or the top-level
// `newNoteFn.set(...)` being dropped — App's `$newNoteFn()` silently degrades
// to the default no-op and ⌘N does nothing, with no error anywhere. These
// tests pin the contract so that failure is loud.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { get } from 'svelte/store';
import {
  newNoteFn,
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
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteList from './NoteList.svelte';

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

function mountList() {
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(NoteList, { target: host, props: { width: 240 } });
  flushSync();
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  notes.set([]);
  selectedNote.set(null);
  selectedFolder.set('Notes/Personal');
  currentAccount.set('localfs:testvault');
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  newNoteFn.set(() => {});
});

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
  component = undefined;
});

describe('newNoteFn — the ⌘N function pointer', () => {
  it('is a no-op before NoteList mounts, so App can call it unguarded', () => {
    // App.svelte calls `$newNoteFn()` with no null check; the default must be
    // safely callable (e.g. ⌘N pressed on the auth screen, before any list).
    expect(() => get(newNoteFn)()).not.toThrow();
    expect(get(notes)).toHaveLength(0);
  });

  it('is set by NoteList on mount', () => {
    mountList();
    expect(get(newNoteFn)).not.toBe(undefined);
    // Must no longer be the inert default.
    const fn = get(newNoteFn);
    fn();
    flushSync();
    expect(get(notes)).toHaveLength(1);
  });

  it('creates the note in the selected folder + account and selects it', () => {
    mountList();
    get(newNoteFn)();
    flushSync();

    const created = get(notes)[0];
    expect(created.label).toBe('Notes/Personal');
    expect(created.account_id).toBe('localfs:testvault');
    expect(created.title).toBe('New Note');
    expect(created.uuid.startsWith('tmp:')).toBe(true);
    // The editor must open on the new note — otherwise ⌘N appears to do
    // nothing even though a row was added.
    expect(get(selectedNote)?.uuid).toBe(created.uuid);
  });

  it('maps the virtual __ALL__ folder onto the account Notes root', () => {
    // '__ALL__' is a UI sentinel, not a real Gmail label; saving against it
    // would fail. newNote() falls back to 'Notes'.
    selectedFolder.set('__ALL__');
    mountList();
    get(newNoteFn)();
    flushSync();
    expect(get(notes)[0].label).toBe('Notes');
  });

  it('prepends, so the new note is visible at the top of the list', () => {
    notes.set([
      { uuid: 'existing', id: 'm1', title: 'Older', body_html: '', date: '2026-01-01T00:00:00Z', label: 'Notes/Personal', account_id: 'localfs:testvault' } as never,
    ]);
    mountList();
    get(newNoteFn)();
    flushSync();
    expect(get(notes).map((n) => n.title)).toEqual(['New Note', 'Older']);
  });

  it('is reset to a no-op when NoteList unmounts', () => {
    mountList();
    unmount(component);
    component = undefined;
    flushSync();

    notes.set([]);
    // A pointer left behind would still close over the destroyed component's
    // stores; the reset must make it inert instead.
    expect(() => get(newNoteFn)()).not.toThrow();
    expect(get(notes)).toHaveLength(0);
  });
});
