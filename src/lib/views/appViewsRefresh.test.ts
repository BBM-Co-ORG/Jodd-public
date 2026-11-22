// @vitest-environment jsdom
//
// Gotcha #6, traced through App itself: the sync worker changes notes with no
// user command behind it, and a local save changes the tables a view reads.
// Both must reach the Views fold (list_views) and the open view (run_view).
// This mounts App and fires the real `remote-changed` listener and a real
// save signal, rather than asserting on a handler in isolation.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get } from 'svelte/store';

const ACCOUNT = { id: 'gmail:test@example.com', email: 'test@example.com', backend_kind: 'gmail' };
const VIEW = { uuid: 'view-1', name: 'Due today', label: 'Notes/__Views__', lines: ['Show: tasks', 'Due: <= today'], errors: [] };
const TARGET = { id: 'm1', uuid: 'n1', title: 'Groceries', body_html: '<div>x</div>', date: '2026-11-19T00:00:00Z', label: 'Notes', account_id: ACCOUNT.id };
const handlers = new Map<string, (e: { payload: unknown }) => void>();
const RESULT = { rows: { Tasks: [{ uuid: 'n1', title: 'Groceries', label: 'Notes', idx: 0, text: 'milk 📅 2026-11-19', done: false, due: '2026-11-19' }] }, notices: [] };
const SECOND = { ...TARGET, id: 'm2', uuid: 'n2', title: 'Pharmacy' };
const RESULT_AFTER_PULL = { rows: { Tasks: [{ uuid: 'n2', title: 'Pharmacy', label: 'Notes', idx: 0, text: 'pills 📅 2026-11-19', done: false, due: '2026-11-19' }] }, notices: [] };
// While set, each run_view waits here until the test releases it.
let holdRuns = false;
const heldRuns: Array<() => void> = [];
// While set, each listing read (list_notes / list_notes_in_folder) waits here.
// Releasing one is the pull landing in SQLite: run_view answers the new rows
// only from then on, which is the ordering iCloud's detector produces
// (remote-changed first, the rows later) and the only route Gmail has.
let holdListings = false;
const heldListings: Array<() => void> = [];
let pulled = false;
const listing = () => {
  if (!holdListings) return Promise.resolve([]);
  return new Promise((r) => heldListings.push(() => { pulled = true; r([]); }));
};

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return [ACCOUNT];
    case 'list_unreviewed_trust': return [];
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_views': return [VIEW];
    case 'run_view':
      if (holdRuns) return new Promise((r) => heldRuns.push(() => r(RESULT)));
      return pulled ? RESULT_AFTER_PULL : RESULT;
    case 'list_notes': return listing();
    case 'list_notes_in_folder': return listing();
    case 'get_cached_note': return (_args as { uuid?: string })?.uuid === 'n2' ? SECOND : TARGET;
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
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

import App from '../../App.svelte';
import { selectedSmartFolder, smartFolderNotes, markRecentlySaved, selectedNote, refreshNotes } from '../stores/notes';
import { REFRESH_FOLDER_EVENT } from '../capture';
import { togglePin } from '../controllers/noteActions';
import { viewsByAccount } from './viewsStore';
import { selectView } from '../controllers/viewSelection';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

const called = (cmd: string, args: unknown) =>
  invoke.mock.calls.some(([c, a]) => c === cmd && JSON.stringify(a) === JSON.stringify(args));

describe('App: saved views follow remote and local changes (gotcha #6)', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    handlers.clear();
    holdRuns = false;
    heldRuns.length = 0;
    holdListings = false;
    heldListings.length = 0;
    pulled = false;
    selectedSmartFolder.set(null);
    smartFolderNotes.set([]);
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  async function mountWithView() {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    expect(handlers.has('remote-changed'), 'App registered the remote-changed listener').toBe(true);
    await settle();
    expect(get(viewsByAccount)[ACCOUNT.id], 'the Views fold read the account on start').toEqual([VIEW]);
    selectedSmartFolder.set({ account: ACCOUNT.id, kind: 'view', uuid: VIEW.uuid });
    for (let i = 0; i < 50 && get(smartFolderNotes).length === 0; i++) await settle(1);
    expect(get(smartFolderNotes).map((n) => n.uuid), 'opening the view ran it').toEqual(['n1']);
    invoke.mockClear();
  }

  it('a remote-changed event re-reads the views and re-runs the open view', async () => {
    await mountWithView();
    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    for (let i = 0; i < 50 && !called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid }); i++) await settle(1);

    expect(called('list_views', { accountId: ACCOUNT.id })).toBe(true);
    expect(called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid })).toBe(true);
  });

  it('a save of a note re-reads the views and re-runs the open view', async () => {
    await mountWithView();
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);
    markRecentlySaved(ACCOUNT.id, 'n1');
    for (let i = 0; i < 50 && !called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid }); i++) await settle(1);

    expect(called('list_views', { accountId: ACCOUNT.id })).toBe(true);
    expect(called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid })).toBe(true);
  });

  // Final review I1: the rows a view reads reach SQLite through the listing
  // read itself (Gmail, Microsoft, LocalFs, SSH: no event at all) or after
  // remote-changed (iCloud: the detector emits, the pull lands later). A
  // re-run on remote-changed alone paints the pre-pull rows and stops there.
  it('a listing reload that lands after remote-changed re-runs the open view with the new rows', async () => {
    await mountWithView();
    holdListings = true;
    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    get(refreshNotes)(); // the reload remote-changed asks for (manual: no throttle in a test)
    for (let i = 0; i < 50 && heldListings.length < 1; i++) await settle(1);
    expect(heldListings.length, 'the listing read is out').toBeGreaterThanOrEqual(1);
    await settle();
    expect(get(smartFolderNotes).map((n) => n.uuid), 'remote-changed alone re-ran against the old rows').toEqual(['n1']);

    invoke.mockClear();
    for (const release of heldListings.splice(0)) release();
    for (let i = 0; i < 50 && get(smartFolderNotes)[0]?.uuid !== 'n2'; i++) await settle(1);

    expect(called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid }), 'the view re-ran after the listing landed').toBe(true);
    expect(called('list_views', { accountId: ACCOUNT.id }), 'the Views fold re-read too').toBe(true);
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['n2']);
  });

  it('a folder listing that lands re-runs the open view with the new rows', async () => {
    await mountWithView();
    holdListings = true;
    window.dispatchEvent(new CustomEvent(REFRESH_FOLDER_EVENT, { detail: { accountId: ACCOUNT.id, folder: 'Notes' } }));
    for (let i = 0; i < 50 && !called('list_notes_in_folder', { accountId: ACCOUNT.id, path: 'Notes' }); i++) await settle(1);
    // The owed folder read runs after the refresh queue's own work: release
    // whatever listing is out until the folder read has been answered.
    for (let i = 0; i < 50 && get(smartFolderNotes)[0]?.uuid !== 'n2'; i++) {
      for (const release of heldListings.splice(0)) release();
      await settle(1);
    }
    expect(called('list_notes_in_folder', { accountId: ACCOUNT.id, path: 'Notes' })).toBe(true);
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['n2']);
  });

  it('a pin that reaches SQLite re-runs the open view', async () => {
    await mountWithView();
    await togglePin(TARGET as never);
    for (let i = 0; i < 50 && !called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid }); i++) await settle(1);

    expect(called('set_pin', { accountId: ACCOUNT.id, uuid: 'n1', pinned: true })).toBe(true);
    expect(called('run_view', { accountId: ACCOUNT.id, uuid: VIEW.uuid })).toBe(true);
  });

  // Review fix round 1: a pick's first run superseded mid-flight by a save's
  // re-run must not settle the pick as empty; the run that paints settles it.
  it('a pick whose run is superseded by a save re-run still opens the first note', async () => {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    await settle();
    expect(get(viewsByAccount)[ACCOUNT.id]).toEqual([VIEW]);

    holdRuns = true;
    selectView(ACCOUNT.id, VIEW.uuid); // the pick: waits for the view's listing
    for (let i = 0; i < 50 && heldRuns.length < 1; i++) await settle(1);
    expect(heldRuns, 'the pick started a run').toHaveLength(1);
    expect(get(selectedNote)).toBeNull();

    markRecentlySaved(ACCOUNT.id, 'other'); // the flushed save re-runs the view
    for (let i = 0; i < 50 && heldRuns.length < 2; i++) await settle(1);
    expect(heldRuns, 'the save re-ran the view').toHaveLength(2);

    heldRuns[0](); // the pick's own run returns, superseded
    await settle();
    heldRuns[1](); // the newer run paints
    for (let i = 0; i < 50 && get(selectedNote) === null; i++) await settle(1);

    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['n1']);
    expect(get(selectedNote)?.uuid).toBe('n1');
  });
});
