// @vitest-environment jsdom
//
// Ruling R21 (spec 2026-10-20 §5.3, "Tablet" column): the portrait tablet
// drawer closes when the user picks a folder, a tag, a Smart Folder or a
// note in it — re-picking the view already shown included — and its history
// entry goes with it. tabletChrome.test.ts pins the frame's mechanics with a
// stub Sidebar; this file drives the REAL Sidebar's rows inside the drawer,
// one test per pick type, so a row that stops reporting its pick fails here.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import { get, writable, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(true),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('tablet'),
}));

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const A_TAGS = [{ uuid: 'u1', tag: 'study' }, { uuid: 'u2', tag: 'work' }];

const invoke = vi.fn(async (cmd: string): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return [A];
    case 'list_folders': return ['Notes', 'Notes/Projects', 'Notes/Work'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string) => invoke(cmd),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import TabletChrome from './TabletChrome.svelte';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import {
  accounts, currentAccount, notes, selectedFolder, selectedNote, selectedSmartFolder, selectedTags,
  searchQuery, noteIndex, hydratedFolders, capabilitiesByAccount, noteTagsByAccount, setAccountNoteTags,
} from '../stores/notes';

const layout = layoutReadonly as unknown as Writable<string>;

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

const drawer = () => host.querySelector('.tablet-drawer');
const overlay = () => overlayFromHistoryState(history.state);
const click = (el: Element | null | undefined) => {
  expect(el).toBeTruthy();
  (el as HTMLElement).click();
  flushSync();
};

async function openDrawer() {
  click(host.querySelector('.tablet-pane-header button[aria-label="Folders"]'));
  await vi.waitFor(() => expect(overlay()).toBe('drawer'));
  await vi.waitFor(() => expect(drawer()?.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
}

async function expectClosedByPick() {
  expect(drawer()).toBeNull();
  expect(host.querySelector('.tablet-drawer-backdrop')).toBeNull();
  await vi.waitFor(() => expect(overlay()).toBeNull());
}

const inDrawer = (sel: string) => drawer()?.querySelector(sel);

beforeEach(() => {
  history.replaceState(null, '');
  layout.set('tablet');
  accounts.set([A as never]);
  currentAccount.set(A.id);
  notes.set([]);
  selectedNote.set(null);
  selectedFolder.set('Notes/Projects');
  selectedSmartFolder.set(null);
  selectedTags.set(new Set());
  searchQuery.set('');
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  capabilitiesByAccount.set({});
  noteTagsByAccount.set(new Map());
  setAccountNoteTags(A.id, A_TAGS);
  window.addEventListener('popstate', handlePopState);
  host = document.createElement('div');
  document.body.appendChild(host);
  const children = createRawSnippet(() => ({ render: () => '<div class="pane-under-test"></div>' }));
  comp = mount(TabletChrome, { target: host, props: { children } });
  flushSync();
});

afterEach(async () => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
  await vi.waitFor(() => expect(overlay()).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('portrait tablet drawer closes on a pick', () => {
  it('a folder', async () => {
    await openDrawer();
    click(inDrawer('.folder-item .folder-name[title="Work"]')?.closest('.folder-item'));
    expect(get(selectedFolder)).toBe('Notes/Work');
    await expectClosedByPick();
  });

  it('the folder already shown (a re-pick changes no store)', async () => {
    await openDrawer();
    click(inDrawer('.folder-item .folder-name[title="Projects"]')?.closest('.folder-item'));
    expect(get(selectedFolder)).toBe('Notes/Projects');
    await expectClosedByPick();
  });

  it('All <account>', async () => {
    await openDrawer();
    click(inDrawer('.folder-item .folder-name[title="All a@b.com"]')?.closest('.folder-item'));
    expect(get(selectedFolder)).toBe('__ALL__');
    await expectClosedByPick();
  });

  it('a Smart Folder', async () => {
    await openDrawer();
    click(inDrawer('.folder-item[data-smart-folder="orphaned"]'));
    expect(get(selectedSmartFolder)).toEqual({ account: A.id, kind: 'orphaned' });
    await expectClosedByPick();
  });

  it('a tag', async () => {
    await openDrawer();
    click(inDrawer('.tags-header'));
    // Expanding the Tags section is not a pick.
    expect(drawer()).not.toBeNull();
    click(inDrawer('.tag-pill[title^="#study"]'));
    expect([...get(selectedTags)]).toEqual(['study']);
    await expectClosedByPick();
  });

  it('a note (New note here, from a folder\'s menu)', async () => {
    await openDrawer();
    const row = inDrawer('.folder-item .folder-name[title="Work"]')!.closest('.folder-item')!;
    row.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
    flushSync();
    // Opening the menu is not a pick either.
    expect(drawer()).not.toBeNull();
    const item = [...host.querySelectorAll<HTMLButtonElement>('.folder-menu button.item')]
      .find((b) => b.textContent?.includes('New note here'));
    click(item);
    expect(get(selectedNote)?.uuid.startsWith('tmp:')).toBe(true);
    await expectClosedByPick();
  });
});
