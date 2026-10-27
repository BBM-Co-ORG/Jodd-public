// @vitest-environment jsdom
//
// "Ask Jodd about this …" from the note, folder and account context menus
// (spec 2026-10-23 §6.2). Each item hands AskJoddModal a scope through
// openAskWith; the modal's own consumption of it is tested in
// AskJoddModal.test.ts. Disabled only for a folder hidden from AI — never for
// an account's AI data permission, whose consent step lives in the popup.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false), supportsIcloud: writable(true), isMacos: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));

import Sidebar from './Sidebar.svelte';
import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags,
  selectedSmartFolder, hydratedFolders,
} from '../stores/notes';
import { askModalOpen, pendingAskScope } from '../stores/ui';
import { hiddenFromAi, AI_HIDDEN_HINT } from '../stores/aiScope';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;

const ACCOUNT = {
  id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z',
  backend_kind: 'gmail', status: 'active',
};
const NOTE = {
  uuid: 'u-1', id: 'msg-1', account_id: ACCOUNT.id, title: 'Plan',
  body_html: '<p>x</p>', date: '2026-10-20T00:00:00Z', label: 'Notes/Work',
} as Note;

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders': return ['Notes', 'Notes/Work', 'Notes/Home'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default: return [];
  }
}

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;

const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
const itemByLabel = (root: ParentNode, label: string) =>
  Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find(
    (b) => b.querySelector('.label')?.textContent?.trim() === label,
  );
function contextMenuOn(el: Element) {
  el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  flushSync();
}

async function mountSidebar() {
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
}
const workRow = () => host.querySelector('.folder-item .folder-name[title="Work"]')!.closest('.folder-item')!;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(routeCommand(cmd)));
  notes.set([]);
  accounts.set([ACCOUNT as never]);
  currentAccount.set(ACCOUNT.id);
  selectedFolder.set('Notes');
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  hiddenFromAi.set({});
  pendingAskScope.set(null);
  askModalOpen.set(false);
  isAndroid.set(false);
  window.addEventListener('popstate', handlePopState);
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(async () => {
  if (component) unmount(component);
  component = undefined;
  host?.remove();
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('note context menu', () => {
  async function mountMenu(selection: Note[] = []) {
    component = mount(ContextMenuHost, { target: host, props: { note: NOTE, selection } });
    flushSync();
    await tick();
  }

  it('opens Ask Jodd on this note and closes the menu', async () => {
    await mountMenu();
    itemByLabel(host, 'Ask Jodd about this note')!.click();
    flushSync();
    expect(get(pendingAskScope)).toEqual({
      kind: 'note', account_id: ACCOUNT.id, uuid: 'u-1', title: 'Plan', links: false, tags: false, depth: 1,
    });
    expect(get(askModalOpen)).toBe(true);
    expect(itemByLabel(host, 'Ask Jodd about this note')).toBeUndefined();
  });

  it('is disabled, saying why, when the note is in a hidden folder', async () => {
    hiddenFromAi.set({ [ACCOUNT.id]: ['Notes/Work'] });
    await mountMenu();
    const item = itemByLabel(host, 'Ask Jodd about this note')!;
    expect(item.disabled).toBe(true);
    expect(item.title).toBe(AI_HIDDEN_HINT);
  });

  it('is absent for a multi-note selection', async () => {
    await mountMenu([NOTE, { ...NOTE, uuid: 'u-2' } as Note]);
    expect(itemByLabel(host, 'Ask Jodd about this note')).toBeUndefined();
  });
});

describe('folder context menu', () => {
  it('opens Ask Jodd on the folder and its subfolders', async () => {
    await mountSidebar();
    contextMenuOn(workRow());
    itemByLabel(host.querySelector('.folder-menu')!, 'Ask Jodd about this folder')!.click();
    flushSync();
    expect(get(pendingAskScope)).toEqual({ kind: 'folder', account_id: ACCOUNT.id, label: 'Notes/Work' });
    expect(get(askModalOpen)).toBe(true);
    expect(host.querySelector('.folder-menu')).toBeNull();
  });

  it('is disabled when the folder is hidden from AI', async () => {
    hiddenFromAi.set({ [ACCOUNT.id]: ['Notes/Work'] });
    await mountSidebar();
    contextMenuOn(workRow());
    const item = itemByLabel(host.querySelector('.folder-menu')!, 'Ask Jodd about this folder')!;
    expect(item.disabled).toBe(true);
    expect(item.title).toBe(AI_HIDDEN_HINT);
  });

  it('is in the phone folder sheet too', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    itemByLabel(sheet()!, 'Ask Jodd about this folder')!.click();
    flushSync();
    expect(get(pendingAskScope)).toEqual({ kind: 'folder', account_id: ACCOUNT.id, label: 'Notes/Work' });
  });
});

describe('account context menu', () => {
  it('right-clicking the account header offers Ask Jodd on the account', async () => {
    await mountSidebar();
    contextMenuOn(host.querySelector('.account-header')!);
    const menu = host.querySelector('.account-menu')!;
    expect(menu).not.toBeNull();
    itemByLabel(menu, 'Ask Jodd about this account')!.click();
    flushSync();
    expect(get(pendingAskScope)).toEqual({ kind: 'account', account_id: ACCOUNT.id });
    expect(get(askModalOpen)).toBe(true);
    expect(host.querySelector('.account-menu')).toBeNull();
  });

  it('Escape closes it', async () => {
    await mountSidebar();
    contextMenuOn(host.querySelector('.account-header')!);
    expect(host.querySelector('.account-menu')).not.toBeNull();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    flushSync();
    expect(host.querySelector('.account-menu')).toBeNull();
  });

  it('long-pressing the phone account label opens a sheet with the item', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(host.querySelector('.phone-account-label')!);
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    itemByLabel(sheet()!, 'Ask Jodd about this account')!.click();
    flushSync();
    expect(get(pendingAskScope)).toEqual({ kind: 'account', account_id: ACCOUNT.id });
  });
});
