// @vitest-environment jsdom
//
// Spec 2026-10-20 §5.3 on the real route, desktop: a click on a Sidebar row
// opens the view's first note (pinned first, then newest) — read from the
// listing already in memory, or, for a view not loaded yet, from App's own
// local SQLite read once it lands. A `remote-changed` refresh that reorders
// the list afterwards leaves the open note open (gotcha #6). controllers/
// selection.test.ts pins the rules; this pins the wiring: Sidebar → viewSelection
// → App's paint → listingLoaded → the editor.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get } from 'svelte/store';
import type { Note } from '../types';

const ACCOUNT = { id: 'gmail:test@example.com', email: 'test@example.com', backend_kind: 'gmail', status: 'active', added_at: '' };
const handlers = new Map<string, (e: { payload: unknown }) => void>();

const n = (uuid: string, label: string, date: string, extra: Partial<Note> = {}): Note => ({
  uuid, id: `id-${uuid}`, account_id: ACCOUNT.id, title: uuid.toUpperCase(), label, date,
  body_html: `<html><head></head><body><div>${uuid.toUpperCase()}</div><div>body of ${uuid}</div></body></html>`,
  x_mail_created_date: null, local_version: 1, ...extra,
} as Note);

const INBOX = n('inbox', 'Notes', '2026-10-01T00:00:00Z');
let projects: Note[] = [];
let stale: Note[] = [];
let resolveStale: (() => void) | null = null;

// A second account, used by the cross-account tag pick: its tagged note is in
// no startup listing — only its own tag read (list_cached_notes_with_tags)
// brings it, so the pick must wait for that paint.
const OTHER = { id: 'gmail:other@example.com', email: 'other@example.com', backend_kind: 'gmail', status: 'active', added_at: '' };
let withOther = false;
const OTHER_NOTE = {
  uuid: 'other-study', id: 'id-other', account_id: OTHER.id, title: 'OTHER STUDY', label: 'Notes',
  date: '2026-10-02T00:00:00Z', body_html: '<html><head></head><body><div>OTHER STUDY</div></body></html>',
  x_mail_created_date: null, local_version: 1,
} as Note;

const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
  if (args?.accountId === OTHER.id) {
    if (cmd === 'list_note_tags') return [{ uuid: OTHER_NOTE.uuid, tag: 'study' }];
    if (cmd === 'list_cached_notes_with_tags') return [OTHER_NOTE];
    if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
    if (cmd === 'backend_capabilities') return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    return [];
  }
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return withOther ? [ACCOUNT, OTHER] : [ACCOUNT];
    case 'list_note_tags': return [{ uuid: INBOX.uuid, tag: 'study' }];
    case 'list_cached_notes_with_tags': return [INBOX];
    case 'list_folders': return ['Notes', 'Notes/Projects'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_cached_notes':
    case 'list_notes':
      return [INBOX, ...projects];
    case 'list_cached_notes_in_folder':
    case 'list_notes_in_folder':
      return args?.path === 'Notes/Projects' ? projects : args?.path === 'Notes' ? [INBOX] : [];
    case 'list_stale_notes':
      // The Smart Folder's own local read, held until the test lets it land.
      return new Promise((r) => { resolveStale = () => r(stale); });
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (name: string, fn: (e: { payload: unknown }) => void) => { handlers.set(name, fn); return () => handlers.delete(name); },
  emit: async () => {},
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import App from '../../App.svelte';
import { selectedNote, selectedFolder, notes, selectedSmartFolder, selectedTags, currentAccount } from '../stores/notes';
import { toggleTag } from '../controllers/viewSelection';
import { smartFolderCommand } from '../smartFolders';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

describe('App: picking a view opens its first note (desktop)', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    handlers.clear();
    selectedNote.set(null);
    selectedSmartFolder.set(null);
    selectedFolder.set('Notes');
    notes.set([]);
    projects = [];
    stale = [];
    resolveStale = null;
    withOther = false;
    selectedTags.set(new Set());
    currentAccount.set(null);
    vi.restoreAllMocks();
    invoke.mockClear();
  });

  async function start() {
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    await vi.waitFor(() => expect(host!.querySelector('.folder-item .folder-name[title="Projects"]')).not.toBeNull());
    await settle();
  }
  const row = (title: string) => host!.querySelector(`.folder-item .folder-name[title="${title}"]`)!.closest('.folder-item') as HTMLElement;
  const editorTitle = () => (host!.querySelector('.title-input') as HTMLInputElement | null)?.value;

  // Galaxy Tab S7 live pass of v0.35.2 (2026-10-21): a cold start left the
  // editor half of the screen on "Select a note / Notes". The starting view
  // counts as a pick, once, when the cache paint lands.
  it('a cold start opens the starting folder\'s first note', async () => {
    await start();
    await vi.waitFor(() => expect(get(selectedNote)?.uuid).toBe('inbox'));
    await settle();
    expect(editorTitle()).toBe('INBOX');
  });

  it('a folder click opens the pinned note, then a remote-changed reorder leaves it open', async () => {
    projects = [
      n('newest', 'Notes/Projects', '2026-10-19T00:00:00Z'),
      n('pinned', 'Notes/Projects', '2026-01-01T00:00:00Z', { pinned: true }),
    ];
    await start();
    row('Projects').click();
    flushSync();
    // In memory since startup's listing: open in the click's own step.
    expect(get(selectedNote)?.uuid).toBe('pinned');
    await settle();
    expect(editorTitle()).toBe('PINNED');
    expect(host!.querySelector('.note-list .note-item')?.textContent).toContain('PINNED');

    // The worker's nudge, past requestRefresh's throttle: the pin went away
    // elsewhere, so the list reorders. The open note stays open.
    projects = [
      n('newest', 'Notes/Projects', '2026-10-19T00:00:00Z', { local_version: 2 }),
      n('pinned', 'Notes/Projects', '2026-01-01T00:00:00Z', { local_version: 2 }),
    ];
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);
    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('list_notes', { accountId: ACCOUNT.id }));
    await settle();
    await vi.waitFor(() => expect(host!.querySelector('.note-list .note-item')?.textContent).toContain('NEWEST'));
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });

  it('a Smart Folder not loaded yet opens its first note when its local read lands', async () => {
    expect(smartFolderCommand('stale')).toBe('list_stale_notes');
    stale = [
      n('old', 'Notes', '2025-01-01T00:00:00Z'),
      n('recent', 'Notes', '2025-06-01T00:00:00Z'),
    ];
    await start();
    selectedNote.set(INBOX);
    await settle();
    const staleRow = Array.from(host!.querySelectorAll('.folder-item')).find((el) => el.textContent?.includes('Stale')) as HTMLElement;
    staleRow.click();
    flushSync();
    expect(get(selectedNote)).toBeNull();
    await vi.waitFor(() => expect(resolveStale).not.toBeNull());
    resolveStale!();
    await vi.waitFor(() => expect(get(selectedNote)?.uuid).toBe('recent'));
    await settle();
    expect(editorTitle()).toBe('RECENT');
  });

  it('the same tag set picked in another account paints that account and opens its note', async () => {
    withOther = true;
    await start();
    toggleTag(ACCOUNT.id, 'study');
    flushSync();
    expect(get(selectedNote)?.uuid).toBe('inbox');
    await settle();
    // Same tag set, other account: not in memory, so the pick waits for the
    // other account's tag paint — which runs only if the paint is keyed on
    // the account as well as the tags.
    toggleTag(OTHER.id, 'study');
    flushSync();
    expect(get(selectedNote)).toBeNull();
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('list_cached_notes_with_tags', { accountId: OTHER.id, tags: ['study'] }));
    await vi.waitFor(() => expect(get(selectedNote)?.uuid).toBe('other-study'));
  });
});
