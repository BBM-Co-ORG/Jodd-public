// @vitest-environment jsdom
//
// Task 4.3 (spec 2026-10-20 §5.1 bottom bars, §5.2 Folders screen): on an
// Android phone the Folders pane is its own simplified screen — per account a
// tappable label ("All"), the folder tree, then Tags / Smart folders /
// Recently deleted rows — with a bottom bar (New folder · AI ✦ · New note).
// Tags and Smart folders are panes of their own between Folders and the list.
// Desktop and tablet keep the full Sidebar unchanged.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, writable, type Writable } from 'svelte/store';
import { readFileSync } from 'node:fs';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(true),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('phone'),
}));

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const B = { id: 'microsoft:kai@c.com', email: 'kai@c.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'microsoft', status: 'active' };
let accountList: unknown[] = [A];
let listing: unknown[] = [];
let hasTrash: Record<string, boolean> = {};
let pendingCount = { notes: 0, deletes: 0, pins: 0, folders: 0 };
let statusRefused = false;
const A_TAGS = [
  { uuid: 'u1', tag: 'study' }, { uuid: 'u1', tag: 'work' },
  { uuid: 'u2', tag: 'study' }, { uuid: 'u3', tag: 'work' },
];

const invoke = vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
  const a = (args ?? {}) as Record<string, unknown>;
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return accountList;
    case 'list_folders': return ['Notes', 'Notes/Projects', 'Notes/Work', 'Notes/__Claude__'];
    case 'list_folder_kinds': return [['Notes/__Claude__', 'system_workflow']];
    case 'backend_capabilities':
      return {
        has_trash: hasTrash[String(a.accountId)] ?? true,
        writes: { notes: true, relocate: true, folders: true, sidecars: true },
      };
    case 'count_pending_pushes': return pendingCount;
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_note_tags': return a.accountId === A.id ? A_TAGS : [];
    case 'create_folder': return { id: 'f', name: `Notes/${a.name}` };
    case 'set_account_status':
      if (statusRefused) throw new Error('refused');
      return null;
    case 'list_notes':
    case 'list_notes_in_folder':
    case 'list_cached_notes':
    case 'list_cached_notes_in_folder':
      return listing;
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async () => () => {},
  emit: async () => {},
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import Sidebar from './Sidebar.svelte';
import PhoneTagsPane from './PhoneTagsPane.svelte';
import PhoneSmartPane from './PhoneSmartPane.svelte';
import GeneralPage from './settings/GeneralPage.svelte';
import AccountPage from './settings/AccountPage.svelte';
import App from '../../App.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import {
  activePane, backLabel, handlePopState, initPhoneNavHistory, navigateToPane,
  overlayFromHistoryState,
} from '../stores/phoneNav';
import { phonePaneAccount } from '../stores/phonePanes';
import {
  accounts, currentAccount, isAuthenticated, notes, selectedFolder, selectedNote,
  selectedSmartFolder, selectedTags, tagMatchMode, searchQuery, noteIndex, hydratedFolders,
  setAccountNoteTags, capabilitiesByAccount, noteTagsByAccount,
} from '../stores/notes';
import { dupStats } from '../stores/dupStats';
import { STOP_WAITING_FAILED } from '../controllers/accountActions';
import { aboutModalOpen, askModalOpen, extractModalOpen, settingsRoute } from '../stores/ui';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const layout = layoutReadonly as unknown as Writable<string>;

const note = (uuid: string, label: string, account = A.id): Note => ({
  uuid, id: `m-${uuid}`, account_id: account, title: `Note ${uuid}`, body_html: '<p>x</p>',
  date: '2026-10-20T00:00:00Z', label,
} as Note);

async function settle(rounds = 10) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
const labelsIn = (root: ParentNode) =>
  Array.from(root.querySelectorAll('.sheet-menu .item .label')).map((l) => l.textContent!.trim());
const buttonByLabel = (root: ParentNode, label: string) =>
  Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find(
    (b) => (b.querySelector('.label')?.textContent ?? b.getAttribute('aria-label') ?? b.textContent)?.trim() === label,
  );
const bar = () => host.querySelector('.phone-bar');
const barButton = (label: string) => host.querySelector<HTMLButtonElement>(`.phone-bar button[aria-label="${label}"]`);

/** A fresh page, seeded and then sitting on the Folders root. */
async function atFolders() {
  history.replaceState(null, '');
  initPhoneNavHistory('Projects');
  navigateToPane('folders');
  await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
  await vi.waitFor(() => expect(history.state?.depth).toBe(0));
}

beforeEach(() => {
  invoke.mockClear();
  accountList = [A];
  listing = [];
  hasTrash = {};
  pendingCount = { notes: 0, deletes: 0, pins: 0, folders: 0 };
  statusRefused = false;
  isAndroid.set(true);
  layout.set('phone');
  isAuthenticated.set(false);
  accounts.set([A as never]);
  currentAccount.set(A.id);
  notes.set([]);
  selectedNote.set(null);
  selectedFolder.set('Notes/Projects');
  selectedSmartFolder.set(null);
  selectedTags.set(new Set());
  tagMatchMode.set('AND');
  searchQuery.set('');
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  capabilitiesByAccount.set({});
  dupStats.set({});
  settingsRoute.set(null);
  askModalOpen.set(false);
  extractModalOpen.set(false);
  aboutModalOpen.set(false);
  phonePaneAccount.set(null);
  noteTagsByAccount.set(new Map());
  setAccountNoteTags(A.id, A_TAGS);
  window.addEventListener('popstate', handlePopState);
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(async () => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
  isAuthenticated.set(false);
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

async function mountSidebar() {
  comp = mount(Sidebar, { target: host, props: { width: 360 } });
  flushSync();
  await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
}

// ─── The phone Folders screen ─────────────────────────────────────────────

describe('phone Folders screen', () => {
  it('phone hides Workflows, dup pill and footer', async () => {
    dupStats.set({ [A.id]: { collapsed: 2, uuids_affected: 1 } as never });
    await mountSidebar();
    await settle(2);
    // The workflow folder exists (it is in the AI sheet), but not in the tree.
    expect(host.querySelector('.folder-name[title="Claude"]')).toBeNull();
    expect(host.textContent).not.toContain('Workflows');
    expect(host.querySelector('.group-header')).toBeNull();
    expect(host.querySelector('.dup-pill')).toBeNull();
    expect(host.querySelector('.sidebar-footer')).toBeNull();
    expect(host.querySelector('.account-chip')).toBeNull();
    expect(host.querySelector('[aria-label="Ask Jodd"]')).toBeNull();
    // The "All <account>" row is gone: the account label opens it instead.
    expect(host.textContent).not.toContain('All a@b.com');
    // Views / Organize / tag pills live in their own panes now.
    expect(host.querySelector('[data-smart-folder]')).toBeNull();
    expect(host.querySelector('.tag-pill')).toBeNull();
  });

  it('each account block has Tags and Smart folders rows', async () => {
    accounts.set([A as never, B as never]);
    hasTrash = { [B.id]: false };
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    const [a, b] = Array.from(host.querySelectorAll<HTMLElement>('.account-section'));
    expect(a.dataset.accountId).toBe(A.id);
    expect(a.querySelector('.phone-account-label .phone-account-name')!.textContent!.trim()).toBe('a · Gmail');
    expect(b.querySelector('.phone-account-label .phone-account-name')!.textContent!.trim()).toBe('kai · Outlook');
    const rowsOf = (s: HTMLElement) =>
      Array.from(s.querySelectorAll<HTMLElement>('.phone-nav-row')).map((r) => r.dataset.row);
    // Tags, Smart folders, then Recently deleted only where the backend has trash.
    await vi.waitFor(() => expect(rowsOf(b)).toEqual(['tags', 'smart']));
    expect(rowsOf(a)).toEqual(['tags', 'smart', 'trash']);
    expect(a.querySelector('[data-row="tags"] .folder-count')!.textContent).toBe('2');
    expect(a.querySelector('[data-row="trash"]')!.textContent).toContain('Recently deleted');
    // Folder rows come first, with counts, at touch size (stylesheet check below).
    const firstNav = a.querySelector('.phone-nav-row')!;
    const lastFolder = Array.from(a.querySelectorAll('.folder-item')).pop()!;
    expect(lastFolder.compareDocumentPosition(firstNav) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  // The phone's account label stands in for desktop's "All <account>" row, so
  // it carries that row's count: every note in the account, from the cached
  // stores (live notes for a hydrated folder, the index otherwise) — the same
  // number, and no IPC behind it (local-first doctrine).
  it('the account label shows the account note count, like desktop "All"', async () => {
    accounts.set([A as never, B as never]);
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    const countOf = (i: number) =>
      host.querySelectorAll('.account-section')[i].querySelector('.phone-account-label .folder-count')?.textContent;
    expect(countOf(0)).toBe('0');
    // A not-yet-hydrated folder counts from the index; a hydrated one from
    // the live notes. Sub-folders count toward the account too.
    noteIndex.set(new Map([[A.id, [
      { id: 'i1', uuid: 'i1', label: 'Notes/Work' },
      { id: 'i2', uuid: 'i2', label: 'Notes/Work' },
      { id: 'i3', uuid: 'i3', label: 'Notes/Projects' },
    ] as never]]));
    hydratedFolders.set(new Map([[A.id, new Set(['Notes/Projects', 'Notes'])]]));
    const callsBefore = invoke.mock.calls.length;
    notes.set([
      note('n1', 'Notes'),
      note('n2', 'Notes/Projects'),
      note('n3', 'Notes/Projects'),
      note('n4', 'Notes/Projects/Sub'),
      note('m1', 'Notes', B.id),
    ]);
    flushSync();
    // Notes 1 + Projects 2 (live, hydrated) + Projects/Sub 1 + Work 2 (index).
    expect(countOf(0)).toBe('6');
    expect(countOf(1)).toBe('1');
    // Straight from the stores: nothing awaited to get the number.
    expect(invoke.mock.calls.slice(callsBefore).map(([c]) => c)).not.toContain('list_notes');
    // And it is desktop's number: the tablet's "All" row says the same.
    unmount(comp!);
    comp = undefined;
    layout.set('tablet');
    comp = mount(Sidebar, { target: host, props: { width: 200 } });
    flushSync();
    await vi.waitFor(() => expect(host.textContent).toContain('All a@b.com'));
    const allRow = Array.from(host.querySelectorAll('.folder-item'))
      .find((r) => r.querySelector('.folder-name')?.textContent?.trim() === 'All a@b.com')!;
    expect(allRow.querySelector('.folder-count')!.textContent).toBe('6');
  });

  it('the account label opens "All <account>" and pushes the list', async () => {
    await atFolders();
    await mountSidebar();
    host.querySelector<HTMLButtonElement>('.phone-account-label')!.click();
    flushSync();
    expect(get(selectedFolder)).toBe('__ALL__');
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('All a@b.com');
  });

  it('Tags and Smart folders rows push their panes for that account', async () => {
    accounts.set([A as never, B as never]);
    await atFolders();
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    const b = host.querySelectorAll<HTMLElement>('.account-section')[1];
    b.querySelector<HTMLElement>('[data-row="tags"]')!.click();
    flushSync();
    expect(get(activePane)).toBe('tags');
    expect(get(phonePaneAccount)).toBe(B.id);
    expect(history.state.depth).toBe(1);
    history.back();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    unmount(comp!);
    comp = undefined;
    await mountSidebar();
    host.querySelectorAll<HTMLElement>('.account-section')[0].querySelector<HTMLElement>('[data-row="smart"]')!.click();
    flushSync();
    expect(get(activePane)).toBe('smart');
    expect(get(phonePaneAccount)).toBe(A.id);
  });

  it('re-tapping the active folder still pushes the list (R21)', async () => {
    selectedFolder.set('Notes/Work');
    await atFolders();
    await mountSidebar();
    host.querySelector<HTMLElement>('.folder-item .folder-name[title="Work"]')!.click();
    flushSync();
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Work');
  });

  it('rows, labels and bar buttons are touch targets, styled from tokens', () => {
    const src = readFileSync('src/lib/components/sidebar/PhoneFolders.svelte', 'utf8');
    const block = src.slice(src.indexOf('Phone Folders screen'), src.indexOf('</style>'));
    expect(block.length).toBeGreaterThan(0);
    expect(block).toMatch(/\.folder-item,\s*\.phone-nav-row \{[^}]*min-height: var\(--touch-target\)/);
    expect(block).toMatch(/\.phone-nav-row[^{]*\{[^}]*min-height: var\(--touch-target\)/);
    expect(block).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
    const barSrc = readFileSync('src/lib/components/PhoneBar.svelte', 'utf8');
    const bar = barSrc.slice(barSrc.indexOf('<style>'));
    expect(bar).toMatch(/\.phone-bar :global\(button\)[^{]*\{[^}]*min-width: var\(--touch-target\)[^}]*min-height: var\(--touch-target\)/);
    expect(bar).toMatch(/\.phone-bar \{[^}]*padding-bottom: calc\(.*env\(safe-area-inset-bottom\)/);
    expect(bar).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
  });

  // The phone rows are the Sidebar's rows at touch size: the badges keep the
  // Sidebar's shapes (Task 4.3 review: the extraction had drifted them).
  it('count pill, blocked badge and expand toggle match the Sidebar\'s', () => {
    const src = readFileSync('src/lib/components/sidebar/PhoneFolders.svelte', 'utf8');
    const css = src.slice(src.indexOf('<style>'));
    const rule = (sel: string) => css.match(new RegExp(`\\n\\s*${sel.replace(/[.[\]']/g, '\\$&')}\\s*\\{([^}]*)\\}`))?.[1] ?? '';
    expect(rule('.folder-count')).toMatch(/border-radius:\s*var\(--radius-lg\)/);
    expect(rule('.folder-blocked-indicator')).toMatch(/width:\s*14px/);
    expect(rule('.folder-blocked-indicator')).toMatch(/height:\s*14px/);
    expect(rule('.folder-blocked-indicator')).toMatch(/margin-left:\s*var\(--space-1\)/);
    expect(rule(".expand-toggle[role='button']:hover")).toMatch(/background:\s*var\(--surface-active\)/);
    // --radius-lg is the Sidebar's 10px pill.
    expect(readFileSync('src/styles/tokens.css', 'utf8')).toMatch(/--radius-lg:\s*10px/);
    expect(readFileSync('src/lib/components/Sidebar.svelte', 'utf8'))
      .toMatch(/\.folder-count \{[^}]*border-radius: 10px/);
  });
});

// ─── The Folders bottom bar ───────────────────────────────────────────────

describe('Folders bottom bar', () => {
  it('has New folder, AI and New note, in that order', async () => {
    await mountSidebar();
    const labels = Array.from(bar()!.querySelectorAll('button')).map((b) => b.getAttribute('aria-label'));
    expect(labels).toEqual(['New folder', 'AI actions', 'New note']);
  });

  it('New folder asks for a name and creates a top-level folder', async () => {
    await mountSidebar();
    barButton('New folder')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="alert"] input')).not.toBeNull());
    const input = document.querySelector<HTMLInputElement>('dialog[data-presentation="alert"] input')!;
    expect(document.querySelector('dialog[data-presentation="alert"] h2')!.textContent).toBe('New folder name');
    input.value = 'Ideas';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    flushSync();
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('create_folder', { accountId: A.id, name: 'Ideas', parentPath: null }));
  });

  it('New folder names the target account when more than one is active', async () => {
    accounts.set([A as never, B as never]);
    currentAccount.set(B.id);
    await mountSidebar();
    barButton('New folder')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="alert"] h2')).not.toBeNull());
    expect(document.querySelector('dialog[data-presentation="alert"] h2')!.textContent).toBe('New folder in kai');
    buttonByLabel(document.querySelector('dialog[data-presentation="alert"]')!, 'Cancel')!.click();
    flushSync();
  });

  it('AI ✦ opens a sheet with Ask Jodd, Extract and the Workflows entries', async () => {
    await atFolders();
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(labelsIn(sheet()!)).toEqual(['Ask Jodd', 'Extract', 'Claude']);
    expect(sheet()!.textContent).toContain('Workflows');
    buttonByLabel(sheet()!, 'Ask Jodd')!.click();
    flushSync();
    expect(get(askModalOpen)).toBe(true);
    await vi.waitFor(() => expect(sheet()).toBeNull());
  });

  it('AI ✦ › Extract opens Extract', async () => {
    await atFolders();
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    buttonByLabel(sheet()!, 'Extract')!.click();
    flushSync();
    expect(get(extractModalOpen)).toBe(true);
    await vi.waitFor(() => expect(sheet()).toBeNull());
  });

  it('AI ✦ › a workflow folder opens its list, titled', async () => {
    await atFolders();
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());
    buttonByLabel(sheet()!, 'Claude')!.click();
    flushSync();
    expect(get(selectedFolder)).toBe('Notes/__Claude__');
    expect(get(activePane)).toBe('list');
    await vi.waitFor(() => expect(sheet()).toBeNull());
    expect(overlayFromHistoryState(history.state)).toBeNull();
    expect(history.state.depth).toBe(1);
    navigateToPane('note');
    expect(get(backLabel)).toBe('Claude');
  });

  it('rotating off the phone while the AI sheet is open closes it for good', async () => {
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    layout.set('tablet');
    await vi.waitFor(() => expect(sheet()).toBeNull());
    layout.set('phone');
    await settle(2);
    expect(sheet()).toBeNull();
  });
});

// ─── The Tags pane ────────────────────────────────────────────────────────

describe('Tags pane', () => {
  async function mountTags(account = A.id) {
    await atFolders();
    phonePaneAccount.set(account);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    await vi.waitFor(() => expect(host.querySelectorAll('.tag-row').length).toBe(2));
  }
  const row = (tag: string) => host.querySelector<HTMLButtonElement>(`.tag-row[data-tag="${tag}"]`)!;
  const selectBtn = () => host.querySelector<HTMLButtonElement>('.pane-header button.select-toggle')!;
  const showBtn = () => host.querySelector<HTMLButtonElement>('button.show-notes');

  it('lists the account\'s tags with counts under a Tags header', async () => {
    await mountTags();
    expect(host.querySelector('.pane-title')!.textContent).toBe('Tags');
    expect(host.querySelector('button.pane-back')!.textContent!.trim()).toBe('Folders');
    expect(row('study').textContent).toContain('#study');
    expect(row('study').querySelector('.tag-count')!.textContent).toBe('2');
  });

  it('tag tap selects one tag and pushes list titled #tag', async () => {
    selectedTags.set(new Set(['work']));
    searchQuery.set('milk');
    await mountTags();
    row('study').click();
    flushSync();
    expect([...get(selectedTags)]).toEqual(['study']);
    expect(get(searchQuery)).toBe('');
    expect(get(currentAccount)).toBe(A.id);
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('#study');
  });

  it('re-tapping the tag already shown still pushes the list (R21)', async () => {
    selectedTags.set(new Set(['study']));
    await mountTags();
    row('study').click();
    flushSync();
    expect(get(activePane)).toBe('list');
    expect(history.state.depth).toBe(2);
  });

  it('tapping a tag of another account switches to that account', async () => {
    accounts.set([A as never, B as never]);
    setAccountNoteTags(B.id, [{ uuid: 'b1', tag: 'home' }, { uuid: 'b2', tag: 'trip' }]);
    await mountTags(B.id);
    row('home').click();
    flushSync();
    expect(get(currentAccount)).toBe(B.id);
    expect([...get(selectedTags)]).toEqual(['home']);
  });

  it('Select mode toggles and shows AND/OR at 2', async () => {
    await mountTags();
    expect(selectBtn().textContent!.trim()).toBe('Select');
    selectBtn().click();
    flushSync();
    expect(selectBtn().textContent!.trim()).toBe('Done');
    // A tap in Select mode checks, it does not navigate.
    row('study').click();
    flushSync();
    expect(get(activePane)).toBe('tags');
    expect(row('study').getAttribute('aria-pressed')).toBe('true');
    expect(row('study').querySelector('.check')).not.toBeNull();
    expect(host.querySelector('.tag-mode')).toBeNull();
    expect(showBtn()!.textContent!.trim()).toBe('Show 2 notes');
    row('work').click();
    flushSync();
    expect(host.querySelector('.tag-mode')).not.toBeNull();
    // AND: only u1 carries both.
    expect(showBtn()!.textContent!.trim()).toBe('Show 1 note');
    buttonByLabel(host, 'OR')!.click();
    flushSync();
    expect(showBtn()!.textContent!.trim()).toBe('Show 3 notes');
    // Unchecking back to one hides the toggle again.
    row('work').click();
    flushSync();
    expect(host.querySelector('.tag-mode')).toBeNull();
    // Nothing reached the stores while selecting.
    expect(get(selectedTags).size).toBe(0);
    expect(get(tagMatchMode)).toBe('AND');
  });

  it('Show N notes applies the selection and pushes the list titled with every tag', async () => {
    await mountTags();
    selectBtn().click();
    flushSync();
    row('work').click();
    row('study').click();
    flushSync();
    buttonByLabel(host, 'OR')!.click();
    flushSync();
    showBtn()!.click();
    flushSync();
    expect([...get(selectedTags)]).toEqual(['work', 'study']);
    expect(get(tagMatchMode)).toBe('OR');
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('#work #study');
  });

  it('leaving Select mode without Show leaves the list\'s selection unchanged', async () => {
    selectedTags.set(new Set(['work']));
    await mountTags();
    selectBtn().click();
    flushSync();
    row('study').click();
    flushSync();
    selectBtn().click(); // Done
    flushSync();
    expect([...get(selectedTags)]).toEqual(['work']);
    expect(showBtn()).toBeNull();
    expect(row('study').getAttribute('aria-pressed')).toBeNull();
    // Re-entering starts from nothing checked.
    selectBtn().click();
    flushSync();
    expect(row('study').getAttribute('aria-pressed')).toBe('false');
  });

  it('Show is disabled with nothing checked', async () => {
    await mountTags();
    selectBtn().click();
    flushSync();
    expect(showBtn()!.disabled).toBe(true);
  });

  it('long-press opens the tag sheet with Rename tag and Delete tag', async () => {
    await mountTags();
    row('study').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheet()!.querySelector('h2')!.textContent).toBe('#study');
    expect(labelsIn(sheet()!)).toEqual(['Rename tag', 'Delete tag']);
    expect(buttonByLabel(sheet()!, 'Delete tag')!.classList.contains('danger')).toBe(true);
    buttonByLabel(sheet()!, 'Rename tag')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="alert"]')).not.toBeNull());
    expect(sheet()).toBeNull();
  });

  it('falls back to the current account when no Tags row named one', async () => {
    accounts.set([A as never, B as never]);
    setAccountNoteTags(B.id, [{ uuid: 'b1', tag: 'home' }]);
    currentAccount.set(B.id);
    await atFolders();
    phonePaneAccount.set(null);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    expect(Array.from(host.querySelectorAll<HTMLElement>('.tag-row')).map((r) => r.dataset.tag)).toEqual(['home']);
    row('home').click();
    flushSync();
    expect(get(currentAccount)).toBe(B.id);
    expect([...get(selectedTags)]).toEqual(['home']);
  });

  it('an account with no tags says so', async () => {
    accounts.set([A as never, B as never]);
    await atFolders();
    phonePaneAccount.set(B.id);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    expect(host.querySelectorAll('.tag-row').length).toBe(0);
    expect(host.textContent).toContain('No tags');
  });
});

// ─── The Smart folders pane ───────────────────────────────────────────────

describe('Smart folders pane', () => {
  async function mountSmart(account = A.id) {
    await atFolders();
    phonePaneAccount.set(account);
    navigateToPane('smart', 'Smart folders');
    comp = mount(PhoneSmartPane, { target: host });
    flushSync();
  }
  const smartRow = (kind: string) => host.querySelector<HTMLButtonElement>(`[data-smart-folder="${kind}"]`)!;

  it('lists the four Smart Folders under a Smart folders header', async () => {
    await mountSmart();
    expect(host.querySelector('.pane-title')!.textContent).toBe('Smart folders');
    expect(Array.from(host.querySelectorAll('[data-smart-folder]')).map((r) => r.textContent!.trim()))
      .toEqual(['Orphaned', 'Stale', 'Extracts', 'Unreviewed']);
  });

  it('a tap selects the Smart Folder and pushes the list titled by its name', async () => {
    selectedTags.set(new Set(['work']));
    await mountSmart();
    smartRow('stale').click();
    flushSync();
    expect(get(selectedSmartFolder)).toEqual({ account: A.id, kind: 'stale' });
    expect(get(selectedTags).size).toBe(0);
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Stale');
  });

  it('re-tapping the Smart Folder already shown still pushes the list (R21)', async () => {
    selectedSmartFolder.set({ account: A.id, kind: 'unreviewed' });
    await mountSmart();
    smartRow('unreviewed').click();
    flushSync();
    expect(get(activePane)).toBe('list');
    expect(history.state.depth).toBe(2);
  });

  it('falls back to the current account when no Smart folders row named one', async () => {
    accounts.set([A as never, B as never]);
    currentAccount.set(B.id);
    await atFolders();
    phonePaneAccount.set(null);
    navigateToPane('smart', 'Smart folders');
    comp = mount(PhoneSmartPane, { target: host });
    flushSync();
    expect(host.querySelector('.pane-subtitle')!.textContent).toBe('kai');
    smartRow('stale').click();
    flushSync();
    expect(get(selectedSmartFolder)).toEqual({ account: B.id, kind: 'stale' });
  });

  it('a Smart Folder of another account switches to it', async () => {
    accounts.set([A as never, B as never]);
    await mountSmart(B.id);
    smartRow('orphaned').click();
    flushSync();
    expect(get(currentAccount)).toBe(B.id);
    expect(get(selectedSmartFolder)).toEqual({ account: B.id, kind: 'orphaned' });
  });
});

// ─── Desktop is unchanged ─────────────────────────────────────────────────

describe('desktop and tablet', () => {
  async function headings() {
    comp = mount(Sidebar, { target: host, props: { width: 200 } });
    flushSync();
    await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
    await settle(2);
    // After the mount's own dup refresh, which would overwrite it.
    dupStats.set({ [A.id]: { collapsed: 2, uuids_affected: 1 } as never });
    flushSync();
    const viewsGroup = Array.from(host.querySelectorAll('.folder-group'))
      .find((g) => g.querySelector('.group-header')?.textContent?.trim() === 'Views');
    return {
      header: host.querySelector('.sidebar-header .app-title')?.textContent,
      groups: Array.from(host.querySelectorAll('.group-header')).map((h) => h.textContent!.trim()),
      all: host.textContent!.includes('All a@b.com'),
      views: Array.from(viewsGroup?.querySelectorAll('.folder-name') ?? []).map((r) => r.textContent!.trim()),
      tags: host.querySelector('.tags-header')?.textContent?.replace(/\s+/g, ' ').trim(),
      dup: host.querySelector('.dup-pill')?.textContent,
      footer: Array.from(host.querySelectorAll('.sidebar-footer .footer-row button')).map(
        (b) => b.getAttribute('aria-label') ?? b.textContent!.trim(),
      ),
      bar: host.querySelector('.phone-bar'),
      phoneLabel: host.querySelector('.phone-account-label'),
    };
  }

  it('desktop Sidebar unchanged', async () => {
    isAndroid.set(false);
    expect(await headings()).toEqual({
      header: 'Jodd',
      groups: ['Workflows', 'Views'],
      all: true,
      views: ['Orphaned', 'Stale', 'Extracts', 'Unreviewed', 'Organize', 'Recently Deleted'],
      tags: 'Tags 2',
      dup: '2 dup',
      footer: ['Ask Jodd', 'App settings', 'Jodd v0.0.0'],
      bar: null,
      phoneLabel: null,
    });
  });

  it('tablet keeps the full Sidebar too', async () => {
    layout.set('tablet');
    const h = await headings();
    expect(h.groups).toEqual(['Workflows', 'Views']);
    expect(h.all).toBe(true);
    expect(h.footer).toEqual(['Ask Jodd', 'App settings', 'Jodd v0.0.0']);
    expect(h.bar).toBeNull();
  });
});

// ─── Reachability of what the phone screen dropped ───────────────────────

describe('reachability', () => {
  it('About opens from Settings › General', async () => {
    comp = mount(GeneralPage, { target: host });
    flushSync();
    const about = buttonByLabel(host, 'About Jodd')!;
    expect(about).toBeTruthy();
    about.click();
    expect(get(aboutModalOpen)).toBe(true);
  });

  it('an account page offers the duplicate review the dup pill used to', async () => {
    dupStats.set({ [A.id]: { collapsed: 3, uuids_affected: 2 } as never });
    comp = mount(AccountPage, { target: host, props: { accountId: A.id, leave: async () => true } });
    flushSync();
    await vi.waitFor(() => expect(buttonByLabel(host, 'Review duplicates')).toBeTruthy());
    buttonByLabel(host, 'Review duplicates')!.click();
    flushSync();
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('preview_orphans', { accountId: A.id }));
  });

  it('no duplicate review without duplicates', async () => {
    comp = mount(AccountPage, { target: host, props: { accountId: A.id, leave: async () => true } });
    flushSync();
    await settle(2);
    expect(buttonByLabel(host, 'Review duplicates')).toBeUndefined();
  });

  it('a draining account page offers Stop waiting, confirming the pending count', async () => {
    const draining = { ...A, status: 'draining' };
    accounts.set([draining as never]);
    pendingCount = { notes: 2, deletes: 0, pins: 0, folders: 0 };
    comp = mount(AccountPage, { target: host, props: { accountId: A.id, leave: async () => true } });
    flushSync();
    await vi.waitFor(() => expect(buttonByLabel(host, 'Stop waiting')).toBeTruthy());
    buttonByLabel(host, 'Stop waiting')!.click();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="alert"]')).not.toBeNull());
    expect(document.querySelector('dialog[data-presentation="alert"]')!.textContent).toContain('2 edit(s)');
    buttonByLabel(document.querySelector('dialog[data-presentation="alert"]')!, 'OK')!.click();
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('set_account_status', expect.objectContaining({ accountId: A.id, status: 'inactive' })));
  });
  it('a refused Stop waiting says so on the account page', async () => {
    accounts.set([{ ...A, status: 'draining' } as never]);
    statusRefused = true;
    vi.spyOn(console, 'error').mockImplementation(() => {});
    comp = mount(AccountPage, { target: host, props: { accountId: A.id, leave: async () => true } });
    flushSync();
    await vi.waitFor(() => expect(buttonByLabel(host, 'Stop waiting')).toBeTruthy());
    buttonByLabel(host, 'Stop waiting')!.click();
    await vi.waitFor(() => expect(host.textContent).toContain(STOP_WAITING_FAILED));
    expect(get(accounts)[0].status).toBe('draining');
  });

  it('a refused Stop waiting in the desktop Sidebar says so too', async () => {
    isAndroid.set(false);
    accounts.set([A as never, { ...B, status: 'draining' } as never]);
    statusRefused = true;
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const alertSpy = vi.fn();
    vi.stubGlobal('alert', alertSpy);
    try {
      comp = mount(Sidebar, { target: host, props: { width: 200 } });
      flushSync();
      await settle(2);
      host.querySelector<HTMLButtonElement>('.account-chip')!.click();
      flushSync();
      host.querySelector<HTMLButtonElement>('.inactive-toggle')!.click();
      flushSync();
      buttonByLabel(host, 'Stop waiting')!.click();
      await vi.waitFor(() => expect(alertSpy).toHaveBeenCalledWith(STOP_WAITING_FAILED));
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

// ─── Wired into App's phone layout ────────────────────────────────────────

describe('App phone panes', () => {
  let app: ReturnType<typeof mount> | undefined;
  const header = () => host.querySelector('.pane-header');
  const title = () => header()?.querySelector('.pane-title')?.textContent ?? null;
  const back = () => header()?.querySelector<HTMLButtonElement>('button.pane-back') ?? null;

  beforeEach(() => {
    accounts.set([]);
    history.replaceState(null, '');
  });
  afterEach(() => {
    if (app) unmount(app);
    app = undefined;
  });

  async function mountApp() {
    app = mount(App, { target: host });
    await vi.waitFor(() => expect(get(accounts).length).toBeGreaterThan(0));
    await settle();
    await vi.waitFor(() => expect(header()).not.toBeNull());
  }
  async function toFolders() {
    back()!.click();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    await vi.waitFor(() => expect(host.querySelector('.phone-bar button[aria-label="New note"]')).not.toBeNull());
  }

  it('New note from Folders: back lands on that folder\'s list', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await toFolders();
    host.querySelector<HTMLButtonElement>('.phone-bar button[aria-label="New note"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('note'));
    const created = get(selectedNote)!;
    expect(created.uuid.startsWith('tmp:')).toBe(true);
    expect(created.label).toBe('Notes/Projects');
    expect(back()!.textContent!.trim()).toBe('Projects');
    expect(history.state.depth).toBe(2);
    back()!.click();
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    expect(title()).toBe('Projects');
    expect(back()!.textContent!.trim()).toBe('Folders');
  });

  it('New note from Folders after a Smart Folder files it in Notes', async () => {
    await mountApp();
    selectedSmartFolder.set({ account: A.id, kind: 'stale' });
    await settle(2);
    await toFolders();
    host.querySelector<HTMLButtonElement>('.phone-bar button[aria-label="New note"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('note'));
    expect(get(selectedNote)!.label).toBe('Notes');
    expect(get(selectedSmartFolder)).toBeNull();
    expect(back()!.textContent!.trim()).toBe('Notes');
  });

  it('renders the Tags and Smart folders panes, each between Folders and the list', async () => {
    await mountApp();
    await toFolders();
    host.querySelector<HTMLElement>('[data-row="tags"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Tags'));
    expect(host.querySelector('.note-editor')).toBeNull();
    host.querySelector<HTMLButtonElement>('.tag-row[data-tag="study"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('#study'));
    expect(back()!.textContent!.trim()).toBe('Tags');
    back()!.click();
    await vi.waitFor(() => expect(title()).toBe('Tags'));
    back()!.click();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    await vi.waitFor(() => expect(host.querySelector('[data-row="smart"]')).not.toBeNull());
    host.querySelector<HTMLElement>('[data-row="smart"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Smart folders'));
    host.querySelector<HTMLButtonElement>('[data-smart-folder="unreviewed"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Unreviewed'));
    expect(back()!.textContent!.trim()).toBe('Smart folders');
  });

  it('AI ✦ › Ask Jodd: the Ask modal\'s history entry survives the sheet\'s pop', async () => {
    await mountApp();
    await toFolders();
    host.querySelector<HTMLButtonElement>('.phone-bar button[aria-label="AI actions"]')!.click();
    flushSync();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());
    const sheetEntry = overlayFromHistoryState(history.state);
    buttonByLabel(sheet()!, 'Ask Jodd')!.click();
    flushSync();
    await settle(4);
    await vi.waitFor(() => expect(sheet()).toBeNull());
    // The sheet's entry was popped and the Ask modal's pushed after it, at
    // the same depth: one overlay entry above Folders, the Ask modal's.
    const askEntry = overlayFromHistoryState(history.state);
    expect(askEntry).not.toBeNull();
    expect(askEntry).not.toBe(sheetEntry);
    expect(history.state.depth).toBe(1);
    expect(get(askModalOpen)).toBe(true);
    const ask = Array.from(document.querySelectorAll('dialog[open]')).find((d) => d.querySelector('h2')?.textContent === 'Ask Jodd');
    expect(ask).toBeTruthy();
    // Back closes Ask and lands on Folders.
    history.back();
    await vi.waitFor(() => expect(get(askModalOpen)).toBe(false));
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
    expect(get(activePane)).toBe('folders');
  });

  it('the list has a bottom bar with New note', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    const btn = host.querySelector<HTMLButtonElement>('.phone-bar button[aria-label="New note"]');
    expect(btn).not.toBeNull();
    // NoteList's own + is not repeated on the phone.
    expect(host.querySelector('.note-list .list-header button[aria-label="New note"]')).toBeNull();
    btn!.click();
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('note'));
    expect(get(selectedNote)!.uuid.startsWith('tmp:')).toBe(true);
    expect(back()!.textContent!.trim()).toBe('Projects');
  });
});
