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

  it("lists Ask Jodd, Make a note with AI, Link a TV, Studio and Organize, in three groups", async () => {
    selectedFolder.set('Notes/Work');
    await openMenu();
    expect(aiButton().getAttribute('aria-expanded')).toBe('true');
    // No TV linked (the store's default): the TV row says where it goes.
    expect(menuLabels()).toEqual(['Ask Jodd…', 'Make a note with AI…', 'Link a TV…', 'Studio…', 'Organize…']);
    expect(menu()!.textContent).toContain('Ctrl+Shift+L');
    // Ask's own hotkey (appHotkeys.ts), in the same right-hand column.
    expect(itemByLabel(menu()!, 'Ask Jodd…')!.querySelector('.hint')!.textContent).toBe('Ctrl+Shift+J');
    // Ask | make a note (paste/links, TV, folder) | Organize.
    const rows = Array.from(menu()!.children).map((el) =>
      el.matches('[role="separator"]') ? '—' : el.querySelector('.label')?.textContent?.trim() ?? null,
    ).filter((x) => x !== null);
    expect(rows).toEqual(['Ask Jodd…', '—', 'Make a note with AI…', 'Link a TV…', 'Studio…', '—', 'Organize…']);
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
    // The folder is on its own line under the name, not in the shortcut
    // column (a folder called "VDO" there read as a shortcut).
    expect(item.querySelector('.sub')!.textContent).toBe('from Work');
    expect(item.querySelector('.hint')).toBeNull();
    // The line is cut short with an ellipsis; the tooltip has the whole name.
    expect(item.title).toContain('from the notes in Work');
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
    const item = itemByLabel(menu()!, 'Studio…')!;
    expect(item.disabled).toBe(true);
    expect(menu()!.querySelector('.menu-reason')!.textContent).toBe(AI_HIDDEN_HINT);
    // No "from Work" on a row that will not run on Work.
    expect(item.querySelector('.sub')).toBeNull();
  });

  it('Organize… opens Organize for the current account', async () => {
    await openMenu();
    // Organize's words (spec 2026-10-28 §16): "duplicates" are the extra
    // Gmail copies, a different, non-AI thing.
    expect(itemByLabel(menu()!, 'Organize…')!.getAttribute('title')).toBe('Find overlapping notes, suggest folders, and find secrets');
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
    // One rule between the two, none dangling for the hidden middle group.
    expect(menu()!.querySelectorAll('[role="separator"]').length).toBe(1);
  });
});

describe('phone AI sheet', () => {
  async function mountSheet(onorganize?: (accountId: string) => void, onclose: () => void = () => {}) {
    component = mount(AiActionsSheet, { target: host, props: { onclose, onorganize } });
    flushSync();
    await tick();
  }
  const sheetLabels = () => Array.from(document.querySelectorAll('.sheet-menu .label')).map((l) => l.textContent?.trim());

  // As PhoneFolders wires it: onorganize passed, so Organize is the last group.
  it('shows the same AI menu in three groups, with Organize and without the desktop hotkeys', async () => {
    isAndroid.set(true);
    await mountSheet(vi.fn());
    expect(document.querySelector('.sheet-menu')!.textContent).not.toContain('Shift+L');
    expect(document.querySelector('.sheet-menu')!.textContent).not.toContain('Shift+J');
    const rows = Array.from(document.querySelector('.sheet-menu')!.querySelectorAll('[role="separator"], .label'))
      .map((el) => (el.matches('[role="separator"]') ? '—' : el.textContent?.trim()));
    expect(rows).toEqual(['Ask Jodd…', '—', 'Make a note with AI…', 'Link a TV…', 'Studio…', '—', 'Organize…']);
  });

  // Spec 2026-10-28 §15: Organize was desktop only (curate spec D5) for a
  // reason that no longer held — the scan uses the same account provider
  // Ask does on Android, and its proposals stay on the device that made
  // them either way.
  it('offers Organize on Android, closing the sheet before it opens', async () => {
    isAndroid.set(true);
    const onorganize = vi.fn();
    const onclose = vi.fn();
    await mountSheet(onorganize, onclose);
    expect(sheetLabels()).toEqual(['Ask Jodd…', 'Make a note with AI…', 'Link a TV…', 'Studio…', 'Organize…']);
    itemByLabel(document.querySelector('.sheet-menu')!, 'Organize…')!.click();
    flushSync();
    expect(onorganize).toHaveBeenCalledWith(ACCOUNT.id);
    await vi.waitFor(() => expect(onclose).toHaveBeenCalled());
  });

  // As the phone wires it (PhoneFolders passes onorganize), and as the
  // desktop popover gates it: Organize stays for a read-only account.
  it('keeps only Ask Jodd and Organize when the current account cannot write notes', async () => {
    isAndroid.set(true);
    capabilitiesByAccount.set({ [ACCOUNT.id]: { has_trash: true, writes: NONE } as never });
    await mountSheet(vi.fn());
    expect(sheetLabels()).toEqual(['Ask Jodd…', 'Organize…']);
    // One rule, between Ask and Organize.
    expect(document.querySelectorAll('.sheet-menu [role="separator"]')).toHaveLength(1);
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
