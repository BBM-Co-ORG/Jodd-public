// @vitest-environment jsdom
//
// The phone Folders screen (spec 2026-11-12, after spec 2026-10-20 §5.1–§5.2):
// the desktop column at touch size — the "For ⌄" picker (2+ accounts), Views
// as a fold that opens its four views in place, Tags (a header that opens
// the Tags pane), then each account: its header (fold chevron, short name,
// backend, state marks, count; the header opens "All"), its folders, its
// Agents group and its own Recently Deleted — with a bottom bar (New folder ·
// AI ✦ · New note). Desktop and tablet keep the full Sidebar.
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
let foldersWritable: Record<string, boolean> = {};
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
        writes: { notes: true, relocate: true, folders: foldersWritable[String(a.accountId)] ?? true, sidecars: true },
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
import { chooseScopeAccount, chooseAllAccounts, scopeChoice } from '../stores/sidebarScope';
import AboutPage from './settings/AboutPage.svelte';
import AccountPage from './settings/AccountPage.svelte';
import App from '../../App.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import {
  activePane, backLabel, handlePopState, initPhoneNavHistory, navigateToPane,
  overlayFromHistoryState,
} from '../stores/phoneNav';
import { phonePaneAccount, phonePaneAllAccounts } from '../stores/phonePanes';
import {
  accounts, currentAccount, isAuthenticated, notes, selectedFolder, selectedNote, error,
  selectedSmartFolder, selectedTags, tagMatchMode, tagScope, searchQuery, noteIndex, hydratedFolders,
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
  // Views start closed (lib/sidebarFolds.ts); tests that tap a view open it.
  localStorage.removeItem('jodd.sidebar.viewsOpen');
  invoke.mockClear();
  accountList = [A];
  listing = [];
  hasTrash = {};
  foldersWritable = {};
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
  it('phone hides the dup pill and footer, and shows the Agents group in the tree', async () => {
    dupStats.set({ [A.id]: { collapsed: 2, uuids_affected: 1 } as never });
    await mountSidebar();
    await settle(2);
    // The Agents group sits in the tree, as on desktop (spec 2026-11-12 §5).
    const claude = host.querySelector('.folder-name[title="Claude"]');
    expect(claude).not.toBeNull();
    const label = host.querySelector<HTMLElement>('.account-section .group-label')!;
    expect(label.textContent).toBe('Agents');
    expect(label.compareDocumentPosition(claude!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(host.querySelector('.dup-pill')).toBeNull();
    expect(host.querySelector('.sidebar-footer')).toBeNull();
    expect(host.querySelector('.account-chip')).toBeNull();
    expect(host.querySelector('[aria-label="Ask Jodd"]')).toBeNull();
    // The "All <account>" row is gone: the account label opens it instead.
    expect(host.textContent).not.toContain('All a@b.com');
    // Views start folded, as on desktop; tag pills live in the Tags pane.
    expect(host.querySelector('[data-smart-folder]')).toBeNull();
    expect(host.querySelector('.tag-pill')).toBeNull();
  });

  // Spec 2026-11-12: the desktop column at touch size — the "For" picker
  // (2+ accounts), Views, Tags, then each account with its header, its tree
  // and its own Recently Deleted.
  it('Views and Tags sit once at the top; Recently Deleted sits in each account with a trash', async () => {
    accounts.set([A as never, B as never]);
    hasTrash = { [B.id]: false };
    foldersWritable = { [B.id]: false };
    currentAccount.set(A.id);
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    const [a, b] = Array.from(host.querySelectorAll<HTMLElement>('.account-section'));
    expect(a.dataset.accountId).toBe(A.id);
    // The desktop header: short name, then the backend as its own tag.
    expect(a.querySelector('.account-header .account-name')!.textContent!.trim()).toBe('a');
    expect(a.querySelector('.account-header .account-backend-tag')!.textContent!.trim()).toBe('Gmail');
    expect(b.querySelector('.account-header .account-name')!.textContent!.trim()).toBe('kai');
    expect(b.querySelector('.account-header .account-backend-tag')!.textContent!.trim()).toBe('Outlook');
    // Outlook's folders are read-only from Jodd: the lock, as on desktop.
    await vi.waitFor(() => expect(b.querySelector('.account-lock')).not.toBeNull());
    expect(a.querySelector('.account-lock')).toBeNull();
    const views = host.querySelector<HTMLElement>('.phone-views')!;
    const heads = () => Array.from(views.querySelectorAll<HTMLElement>('.phone-fold')).map((r) => r.dataset.row);
    expect(heads()).toEqual(['views', 'tags']);
    expect(views.querySelector('[data-row="tags"] .fold-num')!.textContent).toBe('2');
    // No "Smart views", and no Recently Deleted up here.
    expect(host.textContent).not.toContain('Smart views');
    expect(views.querySelector('[data-view="trash"]')).toBeNull();
    // Above every account, after the picker.
    expect(views.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const picker = host.querySelector('.scope-button')!;
    expect(picker.compareDocumentPosition(views) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // Recently Deleted: Gmail's last row; Outlook has no trash, so none.
    const aRows = Array.from(a.querySelectorAll<HTMLElement>('.folder-item'));
    expect(aRows.at(-1)!.dataset.view).toBe('trash');
    expect(aRows.at(-1)!.textContent).toContain('Recently Deleted');
    expect(b.querySelector('[data-view="trash"]')).toBeNull();
  });

  it('Views opens in place: four views, each with its meaning as a second line', async () => {
    await atFolders();
    await mountSidebar();
    const fold = host.querySelector<HTMLButtonElement>('.phone-views [data-row="views"]')!;
    expect(fold.getAttribute('aria-expanded')).toBe('false');
    fold.click();
    flushSync();
    expect(fold.getAttribute('aria-expanded')).toBe('true');
    // The same stored choice the desktop column reads (sidebarFolds.ts).
    expect(localStorage.getItem('jodd.sidebar.viewsOpen')).toBe('true');
    const rows = Array.from(host.querySelectorAll<HTMLElement>('.phone-view-row'));
    expect(rows.map((r) => r.dataset.smartFolder)).toEqual(['orphaned', 'stale', 'extracts', 'unreviewed']);
    expect(rows[0].querySelector('.folder-name')!.textContent).toBe('Not linked');
    expect(rows[0].querySelector('.view-hint')!.textContent).toBe('No other note links to them');
    // A view opens its list straight from Folders: no Smart views pane.
    rows[1].click();
    flushSync();
    expect(get(selectedSmartFolder)).toEqual({ account: A.id, kind: 'stale' });
    expect(get(activePane)).toBe('list');
    expect(history.state.depth).toBe(1);
    navigateToPane('note');
    expect(get(backLabel)).toBe('Untouched');
    localStorage.removeItem('jodd.sidebar.viewsOpen');
  });

  it('an account folds away from its header chevron, and the choice is kept', async () => {
    await mountSidebar();
    const section = host.querySelector<HTMLElement>(`.account-section[data-account-id="${A.id}"]`)!;
    const header = section.querySelector<HTMLElement>('.account-header')!;
    expect(header.getAttribute('aria-expanded')).toBe('true');
    section.querySelector<HTMLElement>('.account-header .expand-toggle')!.click();
    flushSync();
    expect(header.getAttribute('aria-expanded')).toBe('false');
    expect(section.querySelector('.folder-name[title="Work"]')).toBeNull();
    // The chevron folds; it does not open the account.
    expect(get(selectedFolder)).toBe('Notes/Projects');
    expect(JSON.parse(localStorage.getItem('jodd.sidebar.accountsCollapsed')!)).toEqual([A.id]);
    section.querySelector<HTMLElement>('.account-header .expand-toggle')!.click();
    flushSync();
    expect(section.querySelector('.folder-name[title="Work"]')).not.toBeNull();
    localStorage.removeItem('jodd.sidebar.accountsCollapsed');
  });

  it('changes waiting show on the phone header too, from the SQLite count', async () => {
    pendingCount = { notes: 2, deletes: 0, pins: 0, folders: 0 };
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelector('.account-header .account-mark[data-mark="waiting"]')).not.toBeNull());
    expect(host.querySelector('.account-header .account-mark')!.getAttribute('aria-label')).toBe('Syncing — 2 changes waiting');
    expect(invoke).toHaveBeenCalledWith('count_pending_pushes', { accountId: A.id });
  });

  it('a signed-out account says so under its label, with the way back', async () => {
    accounts.set([{ ...A, blocked_reason: 'Signed out. Sign in again to sync.' } as never, B as never]);
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    const [a, b] = Array.from(host.querySelectorAll<HTMLElement>('.account-section'));
    expect(a.querySelector('.account-mark')).not.toBeNull();
    expect(a.querySelector('.account-notice')!.textContent).toContain('Sign in again');
    expect(b.querySelector('.account-notice')).toBeNull();
  });

  it('the open folder carries aria-current, and a parent says whether it is open', async () => {
    selectedFolder.set('Notes/Work');
    await mountSidebar();
    const work = host.querySelector('.folder-item .folder-name[title="Work"]')!.closest<HTMLElement>('.folder-item')!;
    expect(work.getAttribute('aria-current')).toBe('true');
    const notesRow = host.querySelector('.folder-item .folder-name[title="Notes"]')!.closest<HTMLElement>('.folder-item')!;
    expect(notesRow.hasAttribute('aria-expanded')).toBe(true);
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
      host.querySelectorAll('.account-section')[i].querySelector('.account-header .folder-count')?.textContent;
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
    // The account header is desktop's "All" row now (spec 2026-11-10 §2).
    await vi.waitFor(() => expect(host.querySelector(`[data-account-id="${A.id}"] .account-header`)).not.toBeNull());
    const allRow = host.querySelector(`[data-account-id="${A.id}"] .account-header`)!;
    expect(allRow.querySelector('.folder-count')!.textContent).toBe('6');
  });

  it('the account header opens "All <account>" and pushes the list', async () => {
    await atFolders();
    await mountSidebar();
    host.querySelector<HTMLElement>('.account-header')!.click();
    flushSync();
    expect(get(selectedFolder)).toBe('__ALL__');
    expect(get(activePane)).toBe('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('All a@b.com');
  });

  it('the Tags header pushes the Tags pane for the account Views and Tags show', async () => {
    accounts.set([A as never, B as never]);
    currentAccount.set(B.id);
    await atFolders();
    await mountSidebar();
    await vi.waitFor(() => expect(host.querySelectorAll('.account-section').length).toBe(2));
    // B has no tags: no Tags header for it.
    expect(host.querySelector('.phone-views [data-row="tags"]')).toBeNull();
    // Choose A in the "For" picker: Tags follows; the list and the current
    // account stay where they were (spec 2026-11-12 §3).
    host.querySelector<HTMLButtonElement>('.scope-button')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    sheet()!.querySelector<HTMLButtonElement>(`[data-account-id="${A.id}"]`)!.click();
    flushSync();
    await vi.waitFor(() => expect(host.querySelector('.phone-views [data-row="tags"]')).not.toBeNull());
    expect(get(currentAccount)).toBe(B.id);
    expect(get(selectedFolder)).toBe('Notes/Projects');
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
    host.querySelector<HTMLElement>('.phone-views [data-row="tags"]')!.click();
    flushSync();
    expect(get(activePane)).toBe('tags');
    expect(get(phonePaneAccount)).toBe(A.id);
    expect(history.state.depth).toBe(1);
    history.back();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
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
    expect(block).toMatch(/\.folder-item \{[^}]*min-height: var\(--touch-target\)/);
    expect(block).toMatch(/\.phone-fold \{[^}]*min-height: var\(--touch-target\)/);
    expect(block).toMatch(/\.phone-view-row \{[^}]*min-height: var\(--touch-target\)/);
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
    expect(rule('.folder-count')).toMatch(/border-radius:\s*var\(--radius-sm\)/);
    // The drawn alert icon (spec 2026-11-10), in danger ink, as on desktop.
    expect(src).toMatch(/folder-blocked-indicator[\s\S]*?<Icon name="alert" size=\{14\} \/>/);
    expect(rule('.folder-blocked-indicator')).toMatch(/color:\s*var\(--danger\)/);
    expect(rule('.folder-blocked-indicator')).toMatch(/margin-left:\s*var\(--space-1\)/);
    expect(rule(".expand-toggle[role='button']:hover")).toMatch(/background:\s*var\(--surface-active\)/);
    // Counts are chips, and DESIGN.md gives chips 4px corners (--radius-sm).
    expect(readFileSync('src/lib/components/Sidebar.svelte', 'utf8'))
      .toMatch(/\.folder-count \{[^}]*border-radius: var\(--radius-sm\)/);
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
    expect(document.querySelector('dialog[data-presentation="alert"] h2')!.textContent).toBe('New folder in kai · Outlook');
    buttonByLabel(document.querySelector('dialog[data-presentation="alert"]')!, 'Cancel')!.click();
    flushSync();
  });

  it('AI ✦ opens a sheet with the AI menu; agent folders are in the tree, not here', async () => {
    await atFolders();
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(labelsIn(sheet()!)).toEqual(['Ask Jodd…', 'Make a note with AI…', "What's on my TV", 'Studio…']);
    expect(sheet()!.textContent).not.toContain('Agents');
    buttonByLabel(sheet()!, 'Ask Jodd…')!.click();
    flushSync();
    expect(get(askModalOpen)).toBe(true);
    await vi.waitFor(() => expect(sheet()).toBeNull());
  });

  it('AI ✦ › Make a note with AI opens the dialog', async () => {
    await atFolders();
    await mountSidebar();
    barButton('AI actions')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    buttonByLabel(sheet()!, 'Make a note with AI…')!.click();
    flushSync();
    expect(get(extractModalOpen)).toBe(true);
    await vi.waitFor(() => expect(sheet()).toBeNull());
  });

  it('an agent folder in the tree opens its list, titled', async () => {
    await atFolders();
    await mountSidebar();
    host.querySelector<HTMLElement>('.folder-name[title="Claude"]')!.click();
    flushSync();
    expect(get(selectedFolder)).toBe('Notes/__Claude__');
    expect(get(activePane)).toBe('list');
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

  it('a long tag list gets a filter that narrows the rows, Thai included', async () => {
    const many = Array.from({ length: 24 }, (_, i) => ({ uuid: `u${i}`, tag: `topic-${i}` }));
    setAccountNoteTags(A.id, [...many, { uuid: 'ux', tag: 'ดนตรี' }]);
    await atFolders();
    phonePaneAccount.set(A.id);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    const input = host.querySelector<HTMLInputElement>('.filter-input')!;
    expect(input.placeholder).toBe('Filter 25 tags');
    const set = (v: string) => { input.value = v; input.dispatchEvent(new Event('input', { bubbles: true })); flushSync(); };
    set('TOPIC-2');
    expect(Array.from(host.querySelectorAll<HTMLElement>('.tag-row')).map((r) => r.dataset.tag))
      .toEqual(['topic-2', 'topic-20', 'topic-21', 'topic-22', 'topic-23']);
    set('ตร');
    expect(Array.from(host.querySelectorAll<HTMLElement>('.tag-row')).map((r) => r.dataset.tag)).toEqual(['ดนตรี']);
    set('nothing');
    expect(host.querySelectorAll('.tag-row').length).toBe(0);
    expect(host.textContent).toContain('No tag contains “nothing”.');
  });

  it('lists the tags on the most notes first, ties A–Z (2026-11-13)', async () => {
    setAccountNoteTags(A.id, [
      { uuid: 'u1', tag: 'ai' },
      { uuid: 'u1', tag: 'work' }, { uuid: 'u2', tag: 'work' }, { uuid: 'u3', tag: 'work' },
      { uuid: 'u2', tag: 'idea' }, { uuid: 'u3', tag: 'idea' },
      { uuid: 'u4', tag: 'music' }, { uuid: 'u5', tag: 'music' },
    ]);
    await atFolders();
    phonePaneAccount.set(A.id);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    expect(Array.from(host.querySelectorAll<HTMLElement>('.tag-row')).map((r) => r.dataset.tag))
      .toEqual(['work', 'idea', 'music', 'ai']);
  });

  it('a short tag list has no filter', async () => {
    await atFolders();
    phonePaneAccount.set(A.id);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    expect(host.querySelector('.filter-input')).toBeNull();
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

// ─── Views on the Folders screen (spec 2026-11-12 §1) ──────────────────────

describe('Views on the Folders screen', () => {
  async function openViews() {
    await atFolders();
    await mountSidebar();
    host.querySelector<HTMLButtonElement>('.phone-views [data-row="views"]')!.click();
    flushSync();
  }
  const viewRow = (kind: string) => host.querySelector<HTMLButtonElement>(`.phone-view-row[data-smart-folder="${kind}"]`)!;
  afterEach(() => {
    localStorage.removeItem('jodd.sidebar.viewsOpen');
    localStorage.removeItem('jodd.sidebar.scopeAccount');
    scopeChoice.set(null);
  });

  it('a tap clears a tag filter, selects the view and pushes the list titled by its name', async () => {
    selectedTags.set(new Set(['work']));
    await openViews();
    viewRow('stale').click();
    flushSync();
    expect(get(selectedSmartFolder)).toEqual({ account: A.id, kind: 'stale' });
    expect(get(selectedTags).size).toBe(0);
    expect(viewRow('stale').getAttribute('aria-current')).toBe('true');
  });

  it('re-tapping the view already shown still pushes the list (R21)', async () => {
    selectedSmartFolder.set({ account: A.id, kind: 'unreviewed' });
    await openViews();
    viewRow('unreviewed').click();
    flushSync();
    expect(get(activePane)).toBe('list');
    expect(history.state.depth).toBe(1);
  });

  it('a view of the account chosen in "For" opens that account', async () => {
    accounts.set([A as never, B as never]);
    currentAccount.set(A.id);
    chooseScopeAccount(B.id);
    await openViews();
    expect(host.querySelector('.scope-button')!.getAttribute('data-scope-account')).toBe(B.id);
    viewRow('orphaned').click();
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
    const viewsGroup = host.querySelector('.workspace-views');
    return {
      header: host.querySelector('.sidebar-header .app-title')?.textContent,
      groups: Array.from(host.querySelectorAll('.group-header')).map((h) => h.textContent!.trim()),
      // The account header is the "All" row now (spec 2026-11-10 §2).
      all: !!host.querySelector(`[data-account-id="${A.id}"] .account-header[title^="All notes"]`),
      views: Array.from(viewsGroup?.querySelectorAll('.folder-name') ?? []).map((r) => r.textContent!.trim()),
      tags: host.querySelector('.tags-header')?.textContent?.replace(/\s+/g, ' ').trim(),
      dup: host.querySelector('.dup-pill')?.closest('.account-notice')?.querySelector('.notice-text')?.textContent,
      footer: Array.from(host.querySelectorAll('.sidebar-footer .footer-row button')).map(
        (b) => b.getAttribute('aria-label') ?? b.textContent!.trim(),
      ),
      bar: host.querySelector('.phone-bar'),
      phoneFolders: host.querySelector('.phone-folders'),
    };
  }

  it('desktop Sidebar unchanged', async () => {
    isAndroid.set(false);
    localStorage.removeItem('jodd.sidebar.viewsOpen'); // the default: closed
    expect(await headings()).toEqual({
      header: 'Jodd',
      // Views sit once at the top now (spec 2026-11-10 §1).
      groups: ['Views', 'Agents'],
      all: true,
      // Organize moved to the footer's AI menu (spec 2026-10-28 §5.3).
      // Views start closed, like Tags; its header stays (groups above).
      views: [],
      tags: 'Tags 2',
      dup: '2 duplicates in Gmail',
      footer: ['AI actions', 'App settings', 'Jodd v0.0.0'],
      bar: null,
      phoneFolders: null,
    });
  });

  it('tablet keeps the full Sidebar too', async () => {
    layout.set('tablet');
    const h = await headings();
    expect(h.groups).toEqual(['Views', 'Agents']);
    expect(h.all).toBe(true);
    expect(h.footer).toEqual(['AI actions', 'App settings', 'Jodd v0.0.0']);
    expect(h.bar).toBeNull();
  });
});

// ─── Reachability of what the phone screen dropped ───────────────────────

describe('reachability', () => {
  it('About opens from Settings › About & updates', async () => {
    comp = mount(AboutPage, { target: host });
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
      // In the app's error bar, not a native alert() (critique 2026-11-10).
      await vi.waitFor(() => expect(get(error)).toBe(STOP_WAITING_FAILED));
      expect(alertSpy).not.toHaveBeenCalled();
    } finally {
      vi.unstubAllGlobals();
      error.set(null);
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

  // Spec 2026-11-13: a view under All accounts reads every account — the
  // command gets accountId null — and its list says so.
  it('All accounts: a view reads every account, and its header says so', async () => {
    accountList = [A, B];
    chooseAllAccounts();
    await mountApp();
    await toFolders();
    host.querySelector<HTMLButtonElement>('[data-row="views"]')!.click();
    await settle(2);
    invoke.mockClear();
    host.querySelector<HTMLButtonElement>('.phone-view-row[data-smart-folder="stale"]')!.click();
    await settle(2);
    expect(invoke).toHaveBeenCalledWith('list_stale_notes', { accountId: null });
    await vi.waitFor(() => expect(title()).toBe('Untouched'));
    expect(header()!.querySelector('.pane-subtitle')!.textContent).toContain('All accounts');
    localStorage.removeItem('jodd.sidebar.viewsOpen');
    localStorage.removeItem('jodd.sidebar.scopeAccount');
    scopeChoice.set(null);
  });

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

  it('renders the Tags pane between Folders and the list, and a view\'s list right above Folders', async () => {
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
    // Views open in place on Folders: a view's list sits right above it.
    await vi.waitFor(() => expect(host.querySelector('[data-row="views"]')).not.toBeNull());
    host.querySelector<HTMLElement>('[data-row="views"]')!.click();
    await settle(2);
    host.querySelector<HTMLButtonElement>('[data-smart-folder="unreviewed"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Unreviewed'));
    expect(back()!.textContent!.trim()).toBe('Folders');
    localStorage.removeItem('jodd.sidebar.viewsOpen');
  });

  it('AI ✦ › Ask Jodd: the Ask modal\'s history entry survives the sheet\'s pop', async () => {
    await mountApp();
    await toFolders();
    host.querySelector<HTMLButtonElement>('.phone-bar button[aria-label="AI actions"]')!.click();
    flushSync();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());
    const sheetEntry = overlayFromHistoryState(history.state);
    buttonByLabel(sheet()!, 'Ask Jodd…')!.click();
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

// ─── All accounts on the phone (spec 2026-11-13 §3.7) ────────────────────────

describe('All accounts on the phone', () => {
  afterEach(() => {
    localStorage.removeItem('jodd.sidebar.scopeAccount');
    scopeChoice.set(null);
    phonePaneAllAccounts.set(false);
    tagScope.set('account');
  });

  it('the Tags header opens the union; a tap opens the cross-account filter', async () => {
    accounts.set([A as never, B as never]);
    setAccountNoteTags(B.id, [{ uuid: 'b1', tag: 'home' }, { uuid: 'b2', tag: 'study' }]);
    chooseAllAccounts();
    await atFolders();
    await mountSidebar();
    // A's two tags (study, work) and B's two (home, study): three names.
    expect(host.querySelector('.phone-views [data-row="tags"] .fold-num')!.textContent).toBe('3');
    host.querySelector<HTMLElement>('.phone-views [data-row="tags"]')!.click();
    flushSync();
    expect(get(phonePaneAllAccounts)).toBe(true);
    unmount(comp!);
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    expect(host.querySelector('.pane-subtitle')!.textContent).toBe('All accounts');
    // study: 2 notes in A + 1 in B, first.
    const rows = () => Array.from(host.querySelectorAll<HTMLElement>('.tag-row')).map((r) => r.dataset.tag);
    expect(rows()[0]).toBe('study');
    expect(host.querySelector('.tag-row[data-tag="study"] .tag-count')!.textContent).toBe('3');
    host.querySelector<HTMLButtonElement>('.tag-row[data-tag="home"]')!.click();
    flushSync();
    expect(get(tagScope)).toBe('all');
    expect([...get(selectedTags)]).toEqual(['home']);
    expect(get(currentAccount)).toBe(A.id);
  });

  it('Select mode counts across accounts; the tag sheet explains', async () => {
    accounts.set([A as never, B as never]);
    setAccountNoteTags(B.id, [{ uuid: 'b1', tag: 'study' }]);
    await atFolders();
    phonePaneAllAccounts.set(true);
    navigateToPane('tags', 'Tags');
    comp = mount(PhoneTagsPane, { target: host });
    flushSync();
    host.querySelector<HTMLButtonElement>('.select-toggle')!.click();
    flushSync();
    host.querySelector<HTMLButtonElement>('.tag-row[data-tag="study"]')!.click();
    flushSync();
    expect(host.querySelector('.show-notes')!.textContent!.trim()).toBe('Show 3 notes');
    host.querySelector<HTMLButtonElement>('.select-toggle')!.click();
    flushSync();
    host.querySelector('.tag-row[data-tag="study"]')!.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheet()!.textContent).toContain('Pick an account in For to rename or delete this tag.');
    expect(labelsIn(sheet()!)).toEqual([]);
  });
});
