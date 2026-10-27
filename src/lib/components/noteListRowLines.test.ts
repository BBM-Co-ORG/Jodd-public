// @vitest-environment jsdom
//
// A note-list row is two lines, as in Apple Notes: the title, then
// date · preview. The `#tag` chips that used to sit on a third line under
// each tagged row are gone (spec 2026-10-20 "Tag chips are removed from
// note-list rows"): tags are filtered from the sidebar and edited in the
// editor's tags popover. Tag filtering itself is unchanged.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import {
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders,
  selectedTags, selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount,
  noteTagsByAccount, setAccountNoteTags,
} from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteList from './NoteList.svelte';

const ACCT = 'gmail:a@b.com';
const note = (uuid: string): Note => ({
  uuid, id: `m-${uuid}`, account_id: ACCT, title: `Note ${uuid}`,
  body_html: '<div>Note</div><div>body text</div>', date: '2026-10-20T00:00:00Z', label: 'Notes',
} as Note);

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  accounts.set([{ id: ACCT, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail' } as never]);
  currentAccount.set(ACCT);
  selectedFolder.set('Notes');
  notes.set([note('u1'), note('u2')]);
  selectedNote.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  capabilitiesByAccount.set({});
  noteTagsByAccount.set(new Map());
  setAccountNoteTags(ACCT, [{ uuid: 'u1', tag: 'study' }, { uuid: 'u1', tag: 'work' }]);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(NoteList, { target: host, props: { width: 240 } });
  flushSync();
});

afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  host.remove();
});

describe('note-list rows', () => {
  it('a tagged row is title then date · preview, with no tag chips', () => {
    const rows = host.querySelectorAll('.note-item .note-btn');
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      expect(Array.from(row.children).map((c) => c.className.split(' ')[0])).toEqual(['note-title', 'note-meta']);
      expect(row.querySelector('button')).toBeNull();
      expect(row.textContent).not.toContain('#study');
    }
  });

  it('tag filtering still narrows the list', () => {
    selectedTags.set(new Set(['study']));
    flushSync();
    const titles = Array.from(host.querySelectorAll('.note-item .note-title')).map((t) => t.textContent!.trim());
    expect(titles).toEqual(['Note u1']);
  });
});
