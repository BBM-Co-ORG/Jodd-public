// @vitest-environment jsdom
//
// Task 2.7, Sidebar half: on an Android phone the folder and tag long-press
// menus open as bottom sheets (Modal phone="sheet") with the same items in
// the same order; desktop and tablet keep the anchored .folder-menu.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync } from 'svelte';

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
import PhoneTagsPane from './PhoneTagsPane.svelte';
import { phonePaneAccount } from '../stores/phonePanes';
import type { Writable } from 'svelte/store';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { backLabel, handlePopState, initPhoneNavHistory, navigateToPane, overlayFromHistoryState, paneFromHistoryState } from '../stores/phoneNav';
import { get } from 'svelte/store';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags,
  selectedSmartFolder, hydratedFolders, setAccountNoteTags,
} from '../stores/notes';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const layout = layoutReadonly as unknown as Writable<string>;

const ACCOUNT = {
  id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z',
  backend_kind: 'gmail', status: 'active',
};

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes', 'Notes/Work', 'Notes/Home'];
    case 'list_folder_kinds':
      return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes':
      return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default:
      return [];
  }
}

const commandCalls = () => invoke.mock.calls.filter((c) => c[0] !== 'platform_name');

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;

const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
const labelOf = (b: Element) => b.querySelector('.label')?.textContent?.trim();
const buttonByLabel = (root: ParentNode, label: string) =>
  Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find((b) => labelOf(b) === label);
const click = (b: HTMLElement | undefined | null) => { expect(b).toBeTruthy(); b!.click(); flushSync(); };

async function mountSidebar() {
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
}

function contextMenuOn(el: Element) {
  el.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  flushSync();
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
  setAccountNoteTags(ACCOUNT.id, [{ uuid: 'u1', tag: 'groceries' }]);
  isAndroid.set(false);
  layout.set('phone');
  window.addEventListener('popstate', handlePopState);
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(async () => {
  if (component) unmount(component);
  component = undefined;
  host?.remove();
  // Unmounting a sheet pops its history entry asynchronously: let that land
  // before the next test starts, or its popstate leaks into it.
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('folder menu', () => {
  it('desktop still renders the anchored menu', async () => {
    await mountSidebar();
    contextMenuOn(workRow());
    expect(host.querySelector('.folder-menu')).not.toBeNull();
    expect(document.querySelector('dialog[data-presentation]')).toBeNull();
  });

  it('phone long-press opens a sheet with the same item labels as desktop', async () => {
    await mountSidebar();
    contextMenuOn(workRow());
    const desktop = Array.from(host.querySelectorAll('.folder-menu .item .label'))
      .filter((l) => !l.closest('.submenu'))
      .map((l) => l.textContent!.trim());
    expect(desktop).toEqual(['New note here', 'New sub-folder', 'Rename', 'Delete', 'Move to']);
    unmount(component!);
    component = undefined;
    host.replaceChildren();

    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(host.querySelector('.folder-menu')).toBeNull();
    // Same items; only Delete moves, to the end (ruling R17).
    const phone = Array.from(sheet()!.querySelectorAll('.sheet-menu .item .label')).map((l) => l.textContent!.trim());
    expect(phone).toEqual(['New note here', 'New sub-folder', 'Rename', 'Move to', 'Delete']);
    expect([...phone].sort()).toEqual([...desktop].sort());
    const rows = Array.from(sheet()!.querySelectorAll('.sheet-menu > .item'));
    expect(rows[rows.length - 1].classList.contains('danger')).toBe(true);
    expect(sheet()!.querySelector('h2')!.textContent).toBe('Work');
    expect(buttonByLabel(sheet()!, 'Delete')!.classList.contains('danger')).toBe(true);
  });

  it('the layout leaving phone while the sheet is open closes the menu', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    layout.set('tablet');
    await vi.waitFor(() => expect(sheet()).toBeNull());
    expect(host.querySelector('.folder-menu')).toBeNull();
  });

  it('Move to is a second level in the same sheet; picking a target moves and closes', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());

    click(buttonByLabel(sheet()!, 'Move to'));
    expect(document.querySelectorAll('dialog').length).toBe(1);
    expect(buttonByLabel(sheet()!, 'Back')).toBeTruthy();
    expect(buttonByLabel(sheet()!, 'Rename')).toBeUndefined();
    click(buttonByLabel(sheet()!, 'Back'));
    expect(buttonByLabel(sheet()!, 'Rename')).toBeTruthy();

    click(buttonByLabel(sheet()!, 'Move to'));
    click(buttonByLabel(sheet()!, 'Home'));
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(commandCalls().some((c) => /move_folder/.test(String(c[0])))).toBe(true));
  });

  it('Rename closes the sheet, then asks for the name', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Rename'));
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="alert"]')).not.toBeNull());
    expect(sheet()).toBeNull();
  });

  it('New note here closes the sheet into the list pane, then opens the note pane', async () => {
    isAndroid.set(true);
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());

    click(buttonByLabel(sheet()!, 'New note here'));

    await vi.waitFor(() => expect(sheet()).toBeNull());
    expect(overlayFromHistoryState(history.state)).toBeNull();
    expect(paneFromHistoryState(history.state)).toBe('list');
    expect(history.state.depth).toBe(1);
    // The list it closed into is titled by the folder (Task 4.2): the note
    // opened above it names it in its Back.
    navigateToPane('note');
    expect(get(backLabel)).toBe('Work');
  });

  it('a pointerdown on a sheet row does not close it before the tap lands', async () => {
    isAndroid.set(true);
    await mountSidebar();
    contextMenuOn(workRow());
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    buttonByLabel(sheet()!, 'Rename')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    flushSync();
    expect(sheet()).not.toBeNull();
  });
});

describe('tag menu', () => {
  async function openTagMenu() {
    await mountSidebar();
    if (!host.querySelector('.tag-pill')) host.querySelector<HTMLElement>('.tags-header')!.click();
    flushSync();
    const pill = host.querySelector('.tag-pill');
    expect(pill, `tag pill not found: ${host.textContent}`).toBeTruthy();
    contextMenuOn(pill!);
  }

  it('desktop still renders the anchored menu', async () => {
    await openTagMenu();
    expect(host.querySelector('.folder-menu')).not.toBeNull();
    expect(document.querySelector('dialog[data-presentation]')).toBeNull();
  });

  // Task 4.3: on the phone the tags are the Tags pane's (PhoneTagsPane), not
  // the Sidebar's, and so is their long-press sheet.
  async function openPhoneTagMenu() {
    isAndroid.set(true);
    phonePaneAccount.set(ACCOUNT.id);
    component = mount(PhoneTagsPane, { target: host });
    flushSync();
    const row = host.querySelector('.tag-row[data-tag="groceries"]');
    expect(row, `tag row not found: ${host.textContent}`).toBeTruthy();
    contextMenuOn(row!);
  }

  it('a tablet tag menu does not survive rotating to the phone, nor come back after', async () => {
    isAndroid.set(true);
    layout.set('tablet');
    await openTagMenu();
    expect(host.querySelector('.folder-menu')).not.toBeNull();
    layout.set('phone');
    flushSync();
    expect(host.querySelector('.folder-menu')).toBeNull();
    expect(sheet()).toBeNull();
    layout.set('tablet');
    flushSync();
    await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
    expect(host.querySelector('.folder-menu')).toBeNull();
  });

  it('the phone Sidebar has no tag pills: the Tags pane holds them', async () => {
    isAndroid.set(true);
    await mountSidebar();
    expect(host.querySelector('.tag-pill')).toBeNull();
    expect(host.querySelector('.tags-header')).toBeNull();
    expect(host.querySelector('[data-row="tags"]')).not.toBeNull();
  });

  it('the layout leaving phone while the sheet is open closes the menu', async () => {
    await openPhoneTagMenu();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    layout.set('tablet');
    await vi.waitFor(() => expect(sheet()).toBeNull());
    expect(host.querySelector('.folder-menu')).toBeNull();
    layout.set('phone');
    flushSync();
    expect(sheet()).toBeNull();
  });

  it('phone opens a sheet titled #tag with Rename tag and Delete tag (danger last)', async () => {
    await openPhoneTagMenu();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheet()!.querySelector('h2')!.textContent).toBe('#groceries');
    const labels = Array.from(sheet()!.querySelectorAll('.sheet-menu .item .label')).map((l) => l.textContent!.trim());
    expect(labels).toEqual(['Rename tag', 'Delete tag']);
    expect(buttonByLabel(sheet()!, 'Delete tag')!.classList.contains('danger')).toBe(true);
  });
});

describe('sheet rows', () => {
  // Every sheet menu's rows come from one shared stylesheet (fix round 1 of
  // Task 4.3), loaded once by main.ts; the Sidebar keeps only its own row kinds.
  it('are touch-sized, from tokens only, in the shared sheet stylesheet', async () => {
    const { readFileSync } = await import('node:fs');
    const css = readFileSync('src/styles/sheet-menu.css', 'utf8');
    expect(css).toMatch(/\.sheet-menu \.item \{[^}]*min-height: var\(--touch-target\)/);
    expect(css).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
    expect(readFileSync('src/main.ts', 'utf8')).toMatch(/import '\.\/styles\/sheet-menu\.css'/);
    const src = readFileSync('src/lib/components/Sidebar.svelte', 'utf8');
    const block = src.slice(src.indexOf('Phone bottom sheets'), src.indexOf('</style>'));
    expect(block).not.toMatch(/\.sheet-menu \.item \{/);
    expect(block).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
  });
});
