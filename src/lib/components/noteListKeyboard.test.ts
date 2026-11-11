// @vitest-environment jsdom
//
// The note list from the keyboard (2026-11-09 workspace review, audit P1).
// Arrows were caught at window level: pressed on a sidebar row or the scope
// select they swapped the open note, and the selection moved while focus
// stayed behind and nothing scrolled. Every row was also its own Tab stop.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import {
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders,
  selectedTags, selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount,
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteList from './NoteList.svelte';

const ACCOUNT = 'localfs:vault';
const note = (k: number) => ({
  uuid: `u${k}`, id: `m${k}`, account_id: ACCOUNT, label: 'Notes', title: `Note ${k}`,
  body_html: `<div>Note ${k}</div>`, date: `2026-01-0${10 - k}T10:00:00Z`, pinned: false, local_version: 1,
}) as any;

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

async function settle() {
  for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}

beforeEach(async () => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  const ns = [note(1), note(2), note(3)];
  notes.set(ns);
  selectedNote.set(ns[1]);
  selectedFolder.set('Notes');
  currentAccount.set(ACCOUNT);
  accounts.set([]);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(NoteList, { target: host, props: { width: 240 } });
  flushSync();
  await settle();
});

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
  component = undefined;
});

const rows = () => Array.from(host.querySelectorAll<HTMLElement>('.note-btn'));
const rowFor = (uuid: string) => rows().find((r) => r.textContent?.includes(`Note ${uuid.slice(1)}`))!;

async function press(el: Element, key: string) {
  el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true }));
  await settle();
}

describe('NoteList from the keyboard', () => {
  it('has one Tab stop: the open note', () => {
    const stops = rows().filter((r) => r.getAttribute('tabindex') === '0');
    expect(stops).toEqual([rowFor('u2')]);
    expect(rowFor('u2').getAttribute('aria-current')).toBe('true');
  });

  it('arrows on a row open the next note and move focus with it', async () => {
    const row = rowFor('u2');
    row.focus();
    await press(row, 'ArrowDown');
    expect(get(selectedNote)?.uuid).toBe('u3');
    expect(document.activeElement).toBe(rowFor('u3'));
    await press(rowFor('u3'), 'ArrowUp');
    await press(rowFor('u2'), 'ArrowUp');
    expect(get(selectedNote)?.uuid).toBe('u1');
    expect(document.activeElement).toBe(rowFor('u1'));
  });

  it('Home and End jump to the first and last note', async () => {
    const row = rowFor('u2');
    row.focus();
    await press(row, 'End');
    expect(get(selectedNote)?.uuid).toBe('u3');
    await press(rowFor('u3'), 'Home');
    expect(get(selectedNote)?.uuid).toBe('u1');
  });

  it('arrows pressed outside the list leave the open note alone', async () => {
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    outside.focus();
    await press(outside, 'ArrowDown');
    expect(get(selectedNote)?.uuid).toBe('u2');
    outside.remove();
  });
});
