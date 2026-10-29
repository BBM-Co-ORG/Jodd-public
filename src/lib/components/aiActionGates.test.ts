// @vitest-environment jsdom
//
// Every AI entry point applies one rule (controllers/aiActions.ts, spec
// 2026-10-28 §4): a write the account cannot make is hidden; a folder or note
// hidden from AI disables the entry and says why. Ask's own menu items are
// covered in askContextMenus.test.ts; this file covers the entries that make
// or change a note, including the two that skipped the rule before: Studio on
// a hidden folder, and Make a note with AI on a read-only account.
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
import AiActionsSheet from './AiActionsSheet.svelte';
import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags,
  selectedSmartFolder, hydratedFolders, capabilitiesByAccount,
} from '../stores/notes';
import { extractModalOpen, askModalOpen } from '../stores/ui';
import { hiddenFromAi, AI_HIDDEN_HINT } from '../stores/aiScope';
import { NO_FOLDER_HINT } from '../controllers/aiActions';
import { studioRequest } from '../studio';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;

const ACCOUNT = {
  id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z',
  backend_kind: 'gmail', status: 'active',
};
// A note with a Source block, so Redo key points is offered at all.
const NOTE = {
  uuid: 'u-1', id: 'msg-1', account_id: ACCOUNT.id, title: 'Plan',
  body_html: '<h2>Point</h2><details><summary>Source (verbatim)</summary><pre>src</pre></details>',
  date: '2026-10-20T00:00:00Z', label: 'Notes/Work',
} as Note;

const ALL = { notes: true, relocate: true, folders: true, sidecars: true };
const NONE = { notes: false, relocate: false, folders: false, sidecars: false };
let writes = ALL;

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders': return ['Notes', 'Notes/Work', 'Notes/Home'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities': return { has_trash: true, writes };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default: return [];
  }
}

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;

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
const folderMenu = () => host.querySelector('.folder-menu')!;

beforeEach(() => {
  writes = ALL;
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
  capabilitiesByAccount.set({});
  hiddenFromAi.set({});
  studioRequest.set(null);
  extractModalOpen.set(false);
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
  studioRequest.set(null);
  extractModalOpen.set(false);
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('folder menu: Generate with Studio…', () => {
  it('asks for Studio on the folder', async () => {
    await mountSidebar();
    contextMenuOn(workRow());
    const item = itemByLabel(folderMenu(), 'Generate with Studio…')!;
    expect(item.disabled).toBe(false);
    item.click();
    flushSync();
    expect(get(studioRequest)).toEqual({ accountId: ACCOUNT.id, label: 'Notes/Work' });
  });

  it('is disabled, saying why, on a folder hidden from AI', async () => {
    hiddenFromAi.set({ [ACCOUNT.id]: ['Notes/Work'] });
    await mountSidebar();
    contextMenuOn(workRow());
    const item = itemByLabel(folderMenu(), 'Generate with Studio…')!;
    expect(item.disabled).toBe(true);
    expect(item.title).toBe(AI_HIDDEN_HINT);
    // Said once, on a visible line, for both of the menu's AI items.
    expect(Array.from(folderMenu().querySelectorAll('.menu-reason')).map((p) => p.textContent)).toEqual([AI_HIDDEN_HINT]);
    item.click();
    flushSync();
    expect(get(studioRequest)).toBeNull();
  });

  it('is absent on an account that cannot write notes', async () => {
    writes = NONE;
    await mountSidebar();
    await vi.waitFor(() => expect(get(capabilitiesByAccount)[ACCOUNT.id]).toBeDefined());
    contextMenuOn(workRow());
    expect(itemByLabel(folderMenu(), 'Ask Jodd about this folder')).toBeDefined();
    expect(itemByLabel(folderMenu(), 'Generate with Studio…')).toBeUndefined();
  });
});

describe('account menu: Make a note with AI in this account…', () => {
  const accountMenu = () => host.querySelector('.account-menu')!;

  it('replaces the account header\'s old button', async () => {
    await mountSidebar();
    expect(host.querySelector('.account-ingest-btn')).toBeNull();
  });

  it('opens the dialog for that account and closes the menu', async () => {
    await mountSidebar();
    contextMenuOn(host.querySelector('.account-header')!);
    itemByLabel(accountMenu(), 'Make a note with AI in this account…')!.click();
    flushSync();
    expect(get(extractModalOpen)).toBe(true);
    expect(get(currentAccount)).toBe(ACCOUNT.id);
    expect(host.querySelector('.account-menu')).toBeNull();
  });

  it('is absent on an account that cannot write notes; Ask stays', async () => {
    writes = NONE;
    await mountSidebar();
    await vi.waitFor(() => expect(get(capabilitiesByAccount)[ACCOUNT.id]).toBeDefined());
    contextMenuOn(host.querySelector('.account-header')!);
    expect(itemByLabel(accountMenu(), 'Ask Jodd about this account')).toBeDefined();
    expect(itemByLabel(accountMenu(), 'Make a note with AI in this account…')).toBeUndefined();
  });
});

describe('the footer\'s AI menu (desktop)', () => {
  const aiButton = () => host.querySelector<HTMLButtonElement>('.footer-row button[aria-label="AI actions"]')!;
  const menu = () => document.querySelector('.ai-menu');
  const menuLabels = () => Array.from(menu()!.querySelectorAll('.label')).map((l) => l.textContent?.trim());
  async function openMenu() {
    await mountSidebar();
    aiButton().click();
    flushSync();
    await tick();
  }

  it('replaces the footer\'s Ask button, and the Views\' Organize row', async () => {
    await mountSidebar();
    expect(host.querySelector('.footer-row button[aria-label="Ask Jodd"]')).toBeNull();
    expect(host.querySelector('[data-curate="open"]')).toBeNull();
    expect(aiButton().getAttribute('aria-expanded')).toBe('false');
  });

  it('lists Ask Jodd, Make a note with AI, Studio and Organize', async () => {
    selectedFolder.set('Notes/Work');
    await openMenu();
    expect(aiButton().getAttribute('aria-expanded')).toBe('true');
    expect(menuLabels()).toEqual(['Ask Jodd…', 'Make a note with AI…', 'Studio…', 'Organize…']);
    expect(menu()!.textContent).toContain('Ctrl+Shift+L');
    // Focus moves into the menu, so a keyboard user reaches it next.
    expect(document.activeElement).toBe(itemByLabel(menu()!, 'Ask Jodd…'));
  });

  it('Ask Jodd… closes the menu and opens Ask', async () => {
    await openMenu();
    itemByLabel(menu()!, 'Ask Jodd…')!.click();
    flushSync();
    expect(get(askModalOpen)).toBe(true);
    expect(menu()).toBeNull();
  });

  it('Studio… runs on the folder the list shows', async () => {
    selectedFolder.set('Notes/Work');
    await openMenu();
    const item = itemByLabel(menu()!, 'Studio…')!;
    expect(item.disabled).toBe(false);
    expect(item.textContent).toContain('Work');
    item.click();
    flushSync();
    expect(get(studioRequest)).toEqual({ accountId: ACCOUNT.id, label: 'Notes/Work' });
    expect(menu()).toBeNull();
  });

  it('Studio… is disabled, saying why on a visible line, without a folder', async () => {
    selectedFolder.set('__ALL__');
    await openMenu();
    expect(itemByLabel(menu()!, 'Studio…')!.disabled).toBe(true);
    expect(menu()!.querySelector('.menu-reason')!.textContent).toBe(NO_FOLDER_HINT);
  });

  it('Studio… is disabled, saying why, on a folder hidden from AI', async () => {
    selectedFolder.set('Notes/Work');
    hiddenFromAi.set({ [ACCOUNT.id]: ['Notes/Work'] });
    await openMenu();
    expect(itemByLabel(menu()!, 'Studio…')!.disabled).toBe(true);
    expect(menu()!.querySelector('.menu-reason')!.textContent).toBe(AI_HIDDEN_HINT);
  });

  it('Organize… opens Organize for the current account', async () => {
    await openMenu();
    itemByLabel(menu()!, 'Organize…')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.body.textContent).toContain('Find problems'));
    expect(menu()).toBeNull();
  });

  it('a read-only account keeps only Ask Jodd and Organize', async () => {
    writes = NONE;
    await mountSidebar();
    await vi.waitFor(() => expect(get(capabilitiesByAccount)[ACCOUNT.id]).toBeDefined());
    aiButton().click();
    flushSync();
    await tick();
    expect(menuLabels()).toEqual(['Ask Jodd…', 'Organize…']);
  });
});

describe('phone AI sheet', () => {
  async function mountSheet() {
    component = mount(AiActionsSheet, { target: host, props: { workflows: [], onpick: () => {}, onclose: () => {} } });
    flushSync();
    await tick();
  }
  const sheetLabels = () => Array.from(document.querySelectorAll('.sheet-menu .label')).map((l) => l.textContent?.trim());

  it('shows the same AI menu, without Organize (desktop only)', async () => {
    isAndroid.set(true);
    await mountSheet();
    expect(sheetLabels()).toEqual(['Ask Jodd…', 'Make a note with AI…', 'Studio…']);
    expect(document.querySelector('.sheet-menu')!.textContent).not.toContain('Shift+L');
  });

  it('keeps only Ask Jodd when the current account cannot write notes', async () => {
    capabilitiesByAccount.set({ [ACCOUNT.id]: { has_trash: true, writes: NONE } as never });
    await mountSheet();
    expect(sheetLabels()).toEqual(['Ask Jodd…']);
  });
});

describe('note context menu: AI items', () => {
  async function mountMenu() {
    component = mount(ContextMenuHost, { target: host, props: { note: NOTE } });
    flushSync();
    await tick();
  }
  const AI_ITEMS = ['Redo key points', 'Find related notes', 'Suggest a folder'];

  it('are enabled under their new names', async () => {
    await mountMenu();
    for (const label of AI_ITEMS) expect(itemByLabel(host, label)?.disabled, label).toBe(false);
    expect(host.querySelector('.menu-reason')).toBeNull();
  });

  it('are disabled, saying why, when the note is in a hidden folder', async () => {
    hiddenFromAi.set({ [ACCOUNT.id]: ['Notes/Work'] });
    await mountMenu();
    for (const label of AI_ITEMS) {
      const item = itemByLabel(host, label)!;
      expect(item.disabled, label).toBe(true);
      expect(item.title, label).toBe(AI_HIDDEN_HINT);
    }
    // One visible line for the group, not one per item.
    expect(Array.from(host.querySelectorAll('.menu-reason')).map((p) => p.textContent)).toEqual([AI_HIDDEN_HINT]);
  });

  it('are absent on an account that cannot write; Ask stays', async () => {
    capabilitiesByAccount.set({ [ACCOUNT.id]: { has_trash: true, writes: NONE } as never });
    await mountMenu();
    for (const label of AI_ITEMS) expect(itemByLabel(host, label), label).toBeUndefined();
    expect(itemByLabel(host, 'Ask Jodd about this note')).toBeDefined();
  });
});
