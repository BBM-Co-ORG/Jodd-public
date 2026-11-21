// @vitest-environment jsdom
//
// Live pass 2026-10-21 (S23 FE): after Back from a note to the list, the
// note's row sat under the search / refresh area with its title hidden. On
// the phone the list remounts on every return from a note; it now shows the
// open note's row whole inside its own scroller (the <ul>, which starts
// below the search bar and the refresh row) once it is laid out.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { writable, type Writable } from 'svelte/store';

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
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders,
  selectedTags, selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount,
} from '../stores/notes';
import { activePane } from '../stores/phoneNav';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import type { Note } from '../types';
import NoteList from './NoteList.svelte';

const layout = layoutRO as unknown as Writable<'phone' | 'tablet'>;
const ACCT = 'gmail:a@b.com';
const NOTES = Array.from({ length: 12 }, (_, i) => ({
  uuid: `u${i}`, id: `m${i}`, account_id: ACCT, title: `Note ${i}`, body_html: '<p>x</p>',
  date: `2026-10-${String(20 - i).padStart(2, '0')}T00:00:00Z`, label: 'Notes',
}) as Note);

// jsdom has no layout: the list box is 100–500, each row 60px tall, laid
// out from the list's content top minus its scrollTop.
const LIST_TOP = 100;
const LIST_BOTTOM = 500;
const ROW = 60;
const realRect = Element.prototype.getBoundingClientRect;
const scrollTops = new WeakMap<Element, number>();
const realScrollTop = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop')!;

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  Object.defineProperty(Element.prototype, 'scrollTop', {
    configurable: true,
    get(this: Element) { return scrollTops.get(this) ?? 0; },
    set(this: Element, v: number) { scrollTops.set(this, Math.max(0, v)); },
  });
  Element.prototype.getBoundingClientRect = function (this: Element) {
    const ul = this.closest('ul');
    const box = (top: number, bottom: number) => ({ top, bottom, left: 0, right: 360, width: 360, height: bottom - top, x: 0, y: top, toJSON() {} }) as DOMRect;
    if (this.tagName === 'UL') return box(LIST_TOP, LIST_BOTTOM);
    if (ul && this.classList.contains('note-item')) {
      const i = Array.from(ul.querySelectorAll('.note-item')).indexOf(this);
      const top = LIST_TOP + i * ROW - (scrollTops.get(ul) ?? 0);
      return box(top, top + ROW);
    }
    return realRect.call(this);
  };
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  layout.set('phone');
  activePane.set('list');
  notes.set(NOTES);
  selectedFolder.set('Notes');
  currentAccount.set(ACCT);
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
});

afterEach(() => {
  if (component) unmount(component);
  component = null;
  host.remove();
  Element.prototype.getBoundingClientRect = realRect;
  Object.defineProperty(Element.prototype, 'scrollTop', realScrollTop);
  selectedNote.set(null);
});

function rowBox(i: number) {
  return host.querySelectorAll('.note-item')[i].getBoundingClientRect();
}

describe('phone: the open note\'s row is shown whole on return to the list', () => {
  it('a row scrolled up under the top edge comes fully into view', async () => {
    selectedNote.set(NOTES[3]);
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    // Whatever left the list scrolled past the row (row 3 at 180–240).
    host.querySelector('ul')!.scrollTop = 200;
    await vi.waitFor(() => expect(rowBox(3).top).toBeGreaterThanOrEqual(LIST_TOP));
    expect(rowBox(3).bottom).toBeLessThanOrEqual(LIST_BOTTOM);
  });

  it('a row below the bottom edge is scrolled up into view', async () => {
    selectedNote.set(NOTES[10]); // 700–760, below the 500 edge
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    await vi.waitFor(() => expect(rowBox(10).bottom).toBeLessThanOrEqual(LIST_BOTTOM));
    expect(rowBox(10).top).toBeGreaterThanOrEqual(LIST_TOP);
  });

  it('a row already in view is left alone', async () => {
    selectedNote.set(NOTES[1]);
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    await new Promise((r) => setTimeout(r, 50));
    expect(host.querySelector('ul')!.scrollTop).toBe(0);
  });

  it('tablet: the list is not scrolled for it', async () => {
    layout.set('tablet');
    selectedNote.set(NOTES[10]);
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    await new Promise((r) => setTimeout(r, 50));
    expect(host.querySelector('ul')!.scrollTop).toBe(0);
  });
});
