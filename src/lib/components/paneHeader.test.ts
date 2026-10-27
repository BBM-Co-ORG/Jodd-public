// @vitest-environment jsdom
//
// Task 4.2 (spec 2026-10-20 §5.1): every phone pane gets a titled PaneHeader,
// and the note pane's Back names the pane beneath it. The Back label is read
// from phoneNav's title stack, so every list push must record a title (ruling
// R22) — the App-level tests below drive the real push sites (App's folder /
// tag / Smart Folder watchers, Sidebar's folder tap, the cold-start seed and
// its refresh once accounts load) and assert the label the user would read.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick, createRawSnippet } from 'svelte';
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
const B = { id: 'gmail:kai@c.com', email: 'kai@c.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
let accountList: unknown[] = [A];
let listing: unknown[] = [];

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return accountList;
    case 'list_folders': return ['Notes', 'Notes/Projects', 'Notes/Work'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'note_connections': return { outgoing: [], backlinks: [] };
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

import PaneHeader from './PaneHeader.svelte';
import App from '../../App.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { activePane, backLabel, handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import {
  accounts, currentAccount, isAuthenticated, notes, selectedFolder, selectedNote,
  selectedSmartFolder, selectedTags, searchQuery, noteIndex, hydratedFolders,
  isLoading, refreshNotes,
} from '../stores/notes';
import { dupStats } from '../stores/dupStats';
import { settingsRoute } from '../stores/ui';
import { listTitleOf, listSubtitleOf, accountShortName, claimListNoteCount, listNoteCount } from '../stores/listView';
import type { Account, Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const layout = layoutReadonly as unknown as Writable<string>;

const note = (uuid: string, label: string, account = A.id): Note => ({
  uuid, id: `m-${uuid}`, account_id: account, title: `Note ${uuid}`, body_html: '<p>x</p>',
  date: '2026-10-20T00:00:00Z', label,
} as Note);

// ─── PaneHeader on its own ────────────────────────────────────────────────

describe('PaneHeader', () => {
  let host: HTMLElement;
  let comp: ReturnType<typeof mount> | undefined;
  beforeEach(() => { host = document.createElement('div'); document.body.appendChild(host); });
  afterEach(() => { if (comp) unmount(comp); comp = undefined; host.remove(); });

  it('shows a large title and subtitle, and Back names the parent', () => {
    comp = mount(PaneHeader, { target: host, props: { title: 'Projects', subtitle: '3 notes', back: 'Folders' } });
    flushSync();
    expect(host.querySelector('.pane-title')?.textContent).toBe('Projects');
    expect(host.querySelector('.pane-subtitle')?.textContent).toBe('3 notes');
    const back = host.querySelector<HTMLButtonElement>('button.pane-back')!;
    expect(back.textContent?.trim()).toBe('Folders');
    expect(back.getAttribute('aria-label')).toBe('Back to Folders');
  });

  it('Back calls history.back()', () => {
    const spy = vi.spyOn(history, 'back').mockImplementation(() => {});
    comp = mount(PaneHeader, { target: host, props: { title: 'X', back: 'Folders' } });
    flushSync();
    host.querySelector<HTMLButtonElement>('button.pane-back')!.click();
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });

  it('no Back button when back is null; an untitled parent reads "Back"', () => {
    comp = mount(PaneHeader, { target: host, props: { title: 'Folders', back: null } });
    flushSync();
    expect(host.querySelector('button.pane-back')).toBeNull();
    unmount(comp);
    comp = mount(PaneHeader, { target: host, props: { back: '' } });
    flushSync();
    expect(host.querySelector('button.pane-back')?.textContent?.trim()).toBe('Back');
    expect(host.querySelector('.pane-title')).toBeNull();
  });

  it('renders the actions snippet on the right', () => {
    const actions = createRawSnippet(() => ({ render: () => '<button type="button" class="act">Go</button>' }));
    comp = mount(PaneHeader, { target: host, props: { title: 'T', actions } });
    flushSync();
    expect(host.querySelector('.pane-actions .act')).not.toBeNull();
  });

  it('largeTitle=false puts the title in the bar', () => {
    comp = mount(PaneHeader, { target: host, props: { title: 'Small', largeTitle: false } });
    flushSync();
    expect(host.querySelector('.pane-bar .pane-title')?.textContent).toBe('Small');
    expect(host.querySelector('.pane-large')).toBeNull();
  });

  // jsdom has no CSS engine (Svelte's scoped styles never reach the document),
  // so the size rule is asserted on the stylesheet, as the other touch-size
  // tests in this directory do.
  it('buttons are ≥ 48px and the bar reserves the safe area', () => {
    const src = readFileSync('src/lib/components/PaneHeader.svelte', 'utf8');
    const style = src.slice(src.indexOf('<style>'));
    expect(style).toMatch(/\.pane-bar \{[^}]*min-height: calc\(var\(--touch-target\) \+ env\(safe-area-inset-top\)\)/);
    expect(style).toMatch(/\.pane-bar \{[^}]*padding-top: env\(safe-area-inset-top\)/);
    expect(style).toMatch(/\.pane-back \{[^}]*min-height: var\(--touch-target\)/);
    expect(style).toMatch(/\.pane-back \{[^}]*max-width: 40%/);
    expect(style).toMatch(/\.pane-actions :global\(button\) \{[^}]*min-height: var\(--touch-target\)[^}]*min-width: var\(--touch-target\)/);
    expect(style).toMatch(/\.pane-subtitle \{[^}]*white-space: nowrap;[^}]*overflow: hidden;[^}]*text-overflow: ellipsis/);
    expect(src).toMatch(/\$props\(\)/);
  });
});

// ─── Pure helpers ─────────────────────────────────────────────────────────

describe('list title and subtitle', () => {
  const sel = { smart: null, folder: 'Notes/Projects', tags: [] as string[], search: '', accountDisplay: 'a@b.com' };
  it('folder leaf, Smart Folder, tags, search, All, Recently Deleted', () => {
    expect(listTitleOf(sel)).toBe('Projects');
    expect(listTitleOf({ ...sel, smart: { kind: 'unreviewed' } })).toBe('Unreviewed');
    expect(listTitleOf({ ...sel, tags: ['study'] })).toBe('#study');
    expect(listTitleOf({ ...sel, tags: ['study', 'thai'] })).toBe('#study #thai');
    expect(listTitleOf({ ...sel, tags: ['study'], search: 'x' })).toBe('Search');
    // NoteList shows search results first, even inside a Smart Folder.
    expect(listTitleOf({ ...sel, smart: { kind: 'stale' }, search: 'x' })).toBe('Search');
    expect(listTitleOf({ ...sel, smart: { kind: 'stale' }, search: '  ' })).toBe('Stale');
    expect(listTitleOf({ ...sel, folder: '__ALL__' })).toBe('All a@b.com');
    expect(listTitleOf({ ...sel, folder: '__TRASH__' })).toBe('Recently Deleted');
  });
  it('N notes, with the account only when more than one is active', () => {
    expect(listSubtitleOf(3, A as Account, [A as Account])).toBe('3 notes');
    expect(listSubtitleOf(1, A as Account, [A as Account])).toBe('1 note');
    expect(listSubtitleOf(3, B as Account, [A, B] as Account[])).toBe('3 notes · kai');
    expect(listSubtitleOf(null, B as Account, [A, B] as Account[])).toBe('kai');
  });
  it('short name: local part, Local Folder leaf, SSH name; widens on a clash', () => {
    const C = { ...B, id: 'microsoft:kai@d.com', email: 'kai@d.com', backend_kind: 'microsoft' } as Account;
    // Same local part, different address: the full address.
    expect(accountShortName(B as Account, [B as Account, C])).toBe('kai@c.com');
    // Same address on two backends: the address plus the backend.
    const I = { ...A, id: 'icloud:a@b.com', backend_kind: 'icloud' } as Account;
    expect(accountShortName(I, [A as Account, I])).toBe('a@b.com · iCloud');
    expect(accountShortName(A as Account, [A as Account, I])).toBe('a@b.com · Gmail');
    // A Local Folder's email is a path: its last segment.
    const L = { ...A, id: 'local_fs:/Users/k/Notes/Vault', email: '/Users/k/Notes/Vault', backend_kind: 'local_fs' } as Account;
    expect(accountShortName(L, [A as Account, L])).toBe('Vault');
    const W = { ...L, id: 'local_fs:C:\\Notes\\Vault', email: 'C:\\Notes\\Vault' } as Account;
    expect(accountShortName(W, [W])).toBe('Vault');
    expect(accountShortName(L, [L, W])).toBe('/Users/k/Notes/Vault');
    expect(accountShortName({ ...A, email: 'box:Jodd-Notes', backend_kind: 'ssh' } as Account, [])).toBe('box:Jodd-Notes');
  });

  it('the list count belongs to the writer that set it last', () => {
    const trash = claimListNoteCount();
    const list = claimListNoteCount();
    trash.set(0);
    list.set(3); // the incoming list reports before the outgoing one is destroyed
    trash.release();
    expect(get(listNoteCount)).toBe(3);
    list.release();
    expect(get(listNoteCount)).toBeNull();
  });
});

// ─── Wired into App's phone layout ────────────────────────────────────────

async function settle(rounds = 10) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

describe('App phone pane headers', () => {
  let host: HTMLElement;
  let app: ReturnType<typeof mount> | undefined;

  const header = () => host.querySelector('.pane-header');
  const title = () => header()?.querySelector('.pane-title')?.textContent ?? null;
  const subtitle = () => header()?.querySelector('.pane-subtitle')?.textContent ?? null;
  const back = () => header()?.querySelector<HTMLButtonElement>('button.pane-back') ?? null;

  beforeEach(() => {
    invoke.mockClear();
    accountList = [A];
    listing = [];
    isAndroid.set(true);
    layout.set('phone');
    isAuthenticated.set(false);
    accounts.set([]);
    currentAccount.set(A.id);
    notes.set([]);
    selectedNote.set(null);
    selectedFolder.set('Notes/Projects');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    searchQuery.set('');
    noteIndex.set(new Map());
    hydratedFolders.set(new Map());
    dupStats.set({});
    settingsRoute.set(null);
    // A fresh page: initPhoneNavHistory seeds synchronously over a null state.
    history.replaceState(null, '');
    host = document.createElement('div');
    document.body.appendChild(host);
  });

  afterEach(async () => {
    if (app) unmount(app);
    app = undefined;
    host.remove();
    window.removeEventListener('popstate', handlePopState);
    isAuthenticated.set(false);
    settingsRoute.set(null);
  });

  async function mountApp() {
    app = mount(App, { target: host });
    await vi.waitFor(() => expect(get(accounts).length).toBeGreaterThan(0));
    await settle();
    await vi.waitFor(() => expect(header()).not.toBeNull());
  }

  async function openNote(n: Note) {
    selectedNote.set(n);
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('note'));
  }

  async function goBack() {
    back()!.click();
    await vi.waitFor(() => expect(header()).not.toBeNull());
  }

  it('list header shows folder leaf and N notes', async () => {
    listing = [note('1', 'Notes/Projects'), note('2', 'Notes/Projects'), note('3', 'Notes/Projects')];
    await mountApp();
    await vi.waitFor(() => expect(subtitle()).toBe('3 notes'));
    expect(title()).toBe('Projects');
    // NoteList's own title row is hidden on phone: the header carries it.
    expect(host.querySelector('.note-list .list-header h2')).toBeNull();
  });

  it('list subtitle names the account when more than one is active', async () => {
    accountList = [A, B];
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await vi.waitFor(() => expect(subtitle()).toBe('1 note · a'));
  });

  it('back button text is the parent title', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    expect(back()?.textContent?.trim()).toBe('Folders');
    await openNote(listing[0] as Note);
    await vi.waitFor(() => expect(back()?.textContent?.trim()).toBe('Projects'));
    expect(title()).toBeNull();
  });

  it('no back on Folders; the gear opens Settings and carries a dot for duplicates', async () => {
    await mountApp();
    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    await vi.waitFor(() => expect(title()).toBe('Folders'));
    expect(back()).toBeNull();
    // The Sidebar's own header (with its dead collapse ‹) is gone on phone.
    expect(host.querySelector('.sidebar-header')).toBeNull();
    const gear = header()!.querySelector<HTMLButtonElement>('button[aria-label="Settings"]')!;
    expect(gear.querySelector('.pane-dot')).toBeNull();
    dupStats.set({ [A.id]: { collapsed: 2, uuids_affected: 1 } as never });
    flushSync();
    expect(gear.querySelector('.pane-dot')).not.toBeNull();
    gear.click();
    // The section list (the phone's Settings root), not a section page.
    expect(get(settingsRoute)).toBe('');
  });

  it('a folder tapped on Folders titles the list, and so does re-tapping it', async () => {
    await mountApp();
    await goBack();
    await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
    host.querySelector<HTMLElement>('.folder-item .folder-name[title="Work"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    expect(title()).toBe('Work');
    expect(get(backLabel)).toBe('Folders');
    await openNote(note('w', 'Notes/Work'));
    expect(get(backLabel)).toBe('Work');
    // Back twice to Folders, then tap the folder already selected: the
    // folder watcher sees no change, Sidebar's own push must carry the title.
    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    await vi.waitFor(() => expect(host.querySelector('.folder-item .folder-name[title="Work"]')).not.toBeNull());
    host.querySelector<HTMLElement>('.folder-item .folder-name[title="Work"]')!.click();
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    selectedNote.set(null);
    await settle(2);
    await openNote(note('w2', 'Notes/Work'));
    expect(get(backLabel)).toBe('Work');
  });

  it('a tag or Smart Folder picked from Folders titles the list', async () => {
    await mountApp();
    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    selectedTags.set(new Set(['study']));
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    expect(title()).toBe('#study');
    await openNote(note('t', 'Notes'));
    expect(get(backLabel)).toBe('#study');

    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    await goBack();
    await vi.waitFor(() => expect(get(activePane)).toBe('folders'));
    selectedNote.set(null);
    selectedTags.set(new Set());
    selectedSmartFolder.set({ account: A.id, kind: 'unreviewed' });
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    expect(title()).toBe('Unreviewed');
    await openNote(note('s', 'Notes'));
    expect(get(backLabel)).toBe('Unreviewed');
  });

  it('the cold-start list title uses the account name once accounts load', async () => {
    selectedFolder.set('__ALL__');
    await mountApp();
    await vi.waitFor(() => expect(title()).toBe('All a@b.com'));
    await openNote(note('1', 'Notes'));
    expect(get(backLabel)).toBe('All a@b.com');
  });

  it('a search renames the list it is on, so Back from a result says "Search"', async () => {
    await mountApp();
    searchQuery.set('milk');
    await settle(2);
    expect(title()).toBe('Search');
    await openNote(note('r', 'Notes/Projects'));
    expect(get(backLabel)).toBe('Search');
  });

  it('a graph folder node lands on that folder\'s list, titled', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(note('w', 'Notes/Work'));
    expect(get(backLabel)).toBe('Projects');
    // Graph is in ⋯ (Task 5.2) — on phone the note header's ⋯, a sheet
    // (Task 6.2: the toolbar row does not render there, spec §5.6).
    header()!.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('[data-testid="note-more-menu"]')).not.toBeNull());
    Array.from(document.querySelectorAll<HTMLButtonElement>('[data-testid="note-more-menu"] button.item'))
      .find((b) => b.querySelector('.label')?.textContent?.trim() === 'Show graph')!
      .click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('[aria-label="Go to folder Notes/Work"]')).not.toBeNull());
    // The graph's own history entry is on top once the sheet's pop landed:
    // the folder node must close THAT entry into the list.
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());
    document.querySelector('[aria-label="Go to folder Notes/Work"]')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await settle(2);
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    await vi.waitFor(() => expect(title()).toBe('Work'));
    selectedNote.set(null);
    await settle(2);
    await openNote(note('w2', 'Notes/Work'));
    expect(get(backLabel)).toBe('Work');
  });

  it('Search focuses the list search field', async () => {
    await mountApp();
    header()!.querySelector<HTMLButtonElement>('button[aria-label="Search"]')!.click();
    expect(document.activeElement).toBe(host.querySelector('.note-list .search-input'));
  });

  // S23 FE screenshots (v0.35.1): on the phone the list's own header row kept
  // only ⟳ — a whole padded, bordered row under the search field. The user
  // chose to move ⟳ into the pane header beside Search.
  it('⟳ is in the list pane header after Search, and the list has no header row', async () => {
    await mountApp();
    const actions = Array.from(header()!.querySelectorAll<HTMLButtonElement>('button')).map((b) => b.getAttribute('aria-label'));
    expect(actions.indexOf('Refresh notes')).toBeGreaterThan(actions.indexOf('Search'));
    expect(host.querySelector('.note-list .list-header')).toBeNull();
    const btn = header()!.querySelector<HTMLButtonElement>('button[aria-label="Refresh notes"]')!;
    isLoading.set(true);
    flushSync();
    expect(btn.disabled).toBe(true);
    expect(btn.querySelector('.spinning')).not.toBeNull();
    isLoading.set(false);
    flushSync();
    expect(btn.disabled).toBe(false);
    expect(btn.querySelector('.spinning')).toBeNull();
    const refresh = vi.fn();
    refreshNotes.set(refresh);
    btn.click();
    expect(refresh).toHaveBeenCalledOnce();
  });

  it('note header shows the save state and ⋯ opens the note sheet', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    await vi.waitFor(() => expect(header()!.querySelector('.pane-save-state')).not.toBeNull());
    expect(header()!.querySelector('.pane-save-state')!.getAttribute('title')).toBeTruthy();
    header()!.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="sheet"]')).not.toBeNull());
  });

  // Task 6.2 (spec §5.6, ruling R28): the header's ⋯ is the editor's own ⋯
  // menu (NoteMoreMenu) as a sheet — every note action, Delete last — and it
  // is the ONLY "Note actions" on a phone.
  const moreMenu = () => document.querySelector<HTMLElement>('[data-testid="note-more-menu"]');
  const moreLabels = () => Array.from(moreMenu()!.querySelectorAll('.item .label')).map((l) => l.textContent!.trim());
  const moreItem = (label: string) =>
    Array.from(moreMenu()!.querySelectorAll<HTMLButtonElement>('button.item'))
      .find((b) => b.querySelector('.label')?.textContent?.trim() === label)!;
  async function openHeaderMore() {
    const more = header()!.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]')!;
    expect(more.getAttribute('aria-expanded')).toBe('false');
    more.click();
    flushSync();
    await vi.waitFor(() => expect(moreMenu()).not.toBeNull());
    return more;
  }

  it('phone header ⋯ opens the note actions sheet, Delete last; it is the only ⋯', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    expect(document.querySelectorAll('button[aria-label="Note actions"]').length).toBe(1);
    const more = await openHeaderMore();
    expect(more.getAttribute('aria-expanded')).toBe('true');
    expect(moreMenu()!.closest('dialog[data-presentation="sheet"]')).not.toBeNull();
    // §5.6's list; Keyboard shortcuts is desktop only (Task 5.2).
    expect(moreLabels()).toEqual([
      'Pin', 'Move to…', 'Tags…', 'Find in note', 'Copy wiki link', 'Show graph', 'Ask Jodd about this note',
      'Duplicate', 'Refetch from Gmail', 'Delete',
    ]);
    const items = Array.from(moreMenu()!.querySelectorAll<HTMLElement>('button.item'));
    expect(items.at(-1)!.classList.contains('danger')).toBe(true);
    // The interim context-menu sheet is gone: no second menu opened with it.
    expect(document.querySelectorAll('dialog[data-presentation="sheet"]').length).toBe(1);
  });

  it('header ⋯ › Find in note opens the find bar and focuses it', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    await openHeaderMore();
    moreItem('Find in note').click();
    flushSync();
    await vi.waitFor(() => expect(moreMenu()).toBeNull());
    await vi.waitFor(() => expect(host.querySelector('.find-bar .find-input')).not.toBeNull());
    await vi.waitFor(() => expect(document.activeElement).toBe(host.querySelector('.find-bar .find-input')));
    // The sheet's own history entry is gone; the note pane is still on top.
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
    expect(get(activePane)).toBe('note');
  });

  it('header ⋯ › Delete pops to the list', async () => {
    listing = [note('1', 'Notes/Projects'), note('2', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    await openHeaderMore();
    moreItem('Delete').click();
    flushSync();
    await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'delete_note')).toBe(true));
    await vi.waitFor(() => expect(get(activePane)).toBe('list'));
    await vi.waitFor(() => expect(moreMenu()).toBeNull());
    expect(get(selectedNote)).toBeNull();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  });

  // Task 6.1 carry-forward (spec §6.1, §5.6): the toolbar row that shows
  // "Edited on another device" + Reload does not render on a phone, so the
  // note header shows it, beside the save state.
  it('note header shows "Edited on another device" + Reload once the toolbar row is gone', async () => {
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    expect(host.querySelector('.editor-toolbar')).toBeNull();
    expect(header()!.querySelector('.pane-external')).toBeNull();
    // Unsaved typing here, then a newer body from another device.
    const body = host.querySelector<HTMLElement>('.editor-body')!;
    body.innerHTML = '<div>typed here</div>';
    body.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    selectedNote.set({ ...get(selectedNote)!, body_html: '<div>edited in Apple Notes</div>' });
    await settle(2);
    await vi.waitFor(() => expect(header()!.querySelector('.pane-external')).not.toBeNull());
    const ext = header()!.querySelector('.pane-external')!;
    expect(ext.textContent).toBe('Edited on another device');
    expect(ext.getAttribute('title')).toContain('edited on another device');
    // Beside the save state, not instead of it.
    expect(header()!.querySelector('.pane-save-state:not(.pane-external)')?.textContent).toBe('Unsaved');
    const reload = Array.from(header()!.querySelectorAll<HTMLButtonElement>('button'))
      .find((b) => b.textContent?.trim() === 'Reload')!;
    reload.click();
    await settle(2);
    await vi.waitFor(() => expect(header()!.querySelector('.pane-external')).toBeNull());
    expect(host.querySelector('.editor-body')!.textContent).toContain('edited in Apple Notes');
  });

  it('the list count survives a Recently Deleted ↔ folder switch', async () => {
    listing = [note('1', 'Notes/Projects'), note('2', 'Notes/Projects'), note('3', 'Notes/Projects')];
    await mountApp();
    await vi.waitFor(() => expect(subtitle()).toBe('3 notes'));
    selectedFolder.set('__TRASH__');
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Recently Deleted'));
    await vi.waitFor(() => expect(subtitle()).toBe('0 notes'));
    selectedFolder.set('Notes/Projects');
    await settle(2);
    await vi.waitFor(() => expect(title()).toBe('Projects'));
    await settle(2);
    expect(subtitle()).toBe('3 notes');
  });

  it('rotating off the phone drops the ⋯ sheet; rotating back does not reopen it', async () => {
    listing = [note('1', 'Notes/Projects')];
    await mountApp();
    await openNote(listing[0] as Note);
    header()!.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]')!.click();
    flushSync();
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="sheet"]')).not.toBeNull());
    layout.set('tablet');
    await settle(2);
    await vi.waitFor(() => expect(document.querySelector('dialog[data-presentation="sheet"]')).toBeNull());
    layout.set('phone');
    await settle(2);
    expect(document.querySelector('dialog[data-presentation="sheet"]')).toBeNull();
    expect(document.querySelector('.context-menu')).toBeNull();
  });

  it('desktop renders no PaneHeader', async () => {
    isAndroid.set(false);
    app = mount(App, { target: host });
    await vi.waitFor(() => expect(get(accounts).length).toBeGreaterThan(0));
    await settle();
    await vi.waitFor(() => expect(host.querySelector('.note-list')).not.toBeNull());
    expect(header()).toBeNull();
    expect(host.querySelector('.note-list .list-header h2')).not.toBeNull();
    expect(host.querySelector('.sidebar-header')).not.toBeNull();
  });
});
