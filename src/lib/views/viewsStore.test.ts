// @vitest-environment jsdom
//
// Saved views (spec 2026-11-19-properties-tasks-views §8.5): the per-account
// list, the open view's result, the stale-result guard, and the routes back
// to the screen when the worker or a save changes notes (gotcha #6).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

type Handler = (args: Record<string, unknown>) => unknown;
const handlers = new Map<string, Handler>();
const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
  const h = handlers.get(cmd);
  return h ? h(args ?? {}) : null;
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args) }));

import {
  accounts, notes, selectedSmartFolder, smartFolderNotes, markRecentlySaved, noteMutationRevision,
  recentlySavedUuids,
} from '../stores/notes';
import {
  viewsByAccount, refreshViews, openView, onRemoteChanged, runSelectedView, openViewState,
  startViewRefreshRoutes, viewListing, viewErrorsFrom, createView, onListingRefreshed, VIEW_GONE,
  type ViewDef, type ViewResult,
} from './viewsStore';
import type { Account, Note } from '../types';

const A = 'gmail:a@b.c';
const B = 'ssh:me@host';
const acct = (id: string): Account => ({ id, email: id.split(':')[1], backend_kind: id.split(':')[0] } as Account);
const note = (uuid: string, account = A): Note =>
  ({ id: `m-${uuid}`, uuid, title: `T ${uuid}`, body_html: `<div>${uuid}</div>`, date: '2026-11-19T00:00:00Z', label: 'Notes', account_id: account }) as Note;
const def = (uuid: string, name: string): ViewDef => ({ uuid, name, label: 'Notes/__Views__', lines: ['Show: notes'], errors: [] });
const notesResult = (...uuids: string[]): ViewResult => ({
  rows: { Notes: uuids.map((uuid) => ({ uuid, title: uuid, label: 'Notes', modified_ms: 0, task_count: 0, earliest_due: null })) },
  notices: [],
});

async function settle() {
  for (let i = 0; i < 10; i++) await new Promise((r) => setTimeout(r, 0));
}

beforeEach(() => {
  handlers.clear();
  invoke.mockClear();
  accounts.set([acct(A), acct(B)]);
  notes.set([]);
  selectedSmartFolder.set(null);
  smartFolderNotes.set([]);
  openViewState.set(null);
  viewsByAccount.set({});
  recentlySavedUuids.set(new Map());
});

afterEach(() => {
  vi.useRealTimers();
});

describe('refreshViews', () => {
  it('stores the account\'s list', async () => {
    handlers.set('list_views', ({ accountId }) => (accountId === A ? [def('v1', 'Today')] : []));
    await refreshViews(A);
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: A });
    expect(get(viewsByAccount)[A]).toEqual([def('v1', 'Today')]);
  });

  it('an older read that returns last does not put the old list back', async () => {
    let release!: (v: ViewDef[]) => void;
    handlers.set('list_views', () => new Promise<ViewDef[]>((r) => { release = r; }));
    const first = refreshViews(A);
    const releaseFirst = release;
    handlers.set('list_views', () => [def('v2', 'New')]);
    await refreshViews(A);
    releaseFirst([def('v1', 'Old')]);
    await first;
    expect(get(viewsByAccount)[A]).toEqual([def('v2', 'New')]);
  });

  it('a failed read keeps the list it had', async () => {
    viewsByAccount.set({ [A]: [def('v1', 'Today')] });
    handlers.set('list_views', () => { throw 'db locked'; });
    await refreshViews(A);
    expect(get(viewsByAccount)[A]).toEqual([def('v1', 'Today')]);
  });
});

describe('openView', () => {
  it('returns the result of a view that runs', async () => {
    handlers.set('run_view', () => notesResult('n1'));
    expect(await openView(A, 'v1')).toEqual(notesResult('n1'));
    expect(invoke).toHaveBeenCalledWith('run_view', { accountId: A, uuid: 'v1' });
  });

  it('returns the parsed errors for an Err carrying ViewError JSON', async () => {
    const errs = [{ line: 1, key: 'due', message: '`Due: soon` is not a date' }];
    handlers.set('run_view', () => { throw JSON.stringify(errs); });
    expect(await openView(A, 'v1')).toEqual({ errors: errs });
  });

  it('a plain-text Err (a DB failure) is one error with no line', async () => {
    handlers.set('run_view', () => { throw 'run_view: database is locked'; });
    expect(await openView(A, 'v1')).toEqual({ errors: [{ line: -1, key: '', message: 'run_view: database is locked' }] });
  });

  it('viewErrorsFrom: JSON that is not a ViewError list is plain text', () => {
    expect(viewErrorsFrom('[1,2]')).toEqual([{ line: -1, key: '', message: '[1,2]' }]);
    expect(viewErrorsFrom('{"a":1}')).toEqual([{ line: -1, key: '', message: '{"a":1}' }]);
  });
});

describe('viewListing', () => {
  it('a tasks result lists each note once, with its row count and earliest due', () => {
    const t = (uuid: string, idx: number, due: string | null) => ({ uuid, title: uuid, label: 'Notes', idx, text: 'x', done: false, due });
    const l = viewListing({ rows: { Tasks: [t('n2', 0, '2026-11-25'), t('n1', 3, null), t('n2', 4, '2026-11-20'), t('n1', 5, '2026-12-01')] }, notices: [] });
    expect(l.show).toBe('tasks');
    expect(l.uuids).toEqual(['n2', 'n1']);
    expect(l.meta.get('n2')).toEqual({ taskCount: 2, earliestDue: '2026-11-20' });
    expect(l.meta.get('n1')).toEqual({ taskCount: 2, earliestDue: '2026-12-01' });
  });
});

describe('runSelectedView', () => {
  it('paints the notes in the engine\'s order, from memory or the cache', async () => {
    notes.set([note('n2')]);
    handlers.set('run_view', () => notesResult('n1', 'n2'));
    handlers.set('get_cached_note', ({ uuid }) => (uuid === 'n1' ? note('n1') : null));
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    await runSelectedView();
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['n1', 'n2']);
    expect(invoke).not.toHaveBeenCalledWith('get_cached_note', { accountId: A, uuid: 'n2' });
    expect(get(openViewState)).toMatchObject({ key: `${A}|v1`, errors: null, show: 'notes' });
  });

  // Final review I1 / Task 10 M2: a view deleted elsewhere has no property
  // block, which run_view reports as "no conditions" — the account's list,
  // once read, says what actually happened.
  it('a selected view no longer in the account\'s list says it was deleted', async () => {
    smartFolderNotes.set([note('old')]);
    handlers.set('list_views', () => [def('v2', 'Other')]);
    handlers.set('run_view', () => { throw JSON.stringify([{ line: 0, key: '', message: 'this view has no conditions' }]); });
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    await onListingRefreshed([A]);
    await settle();
    expect(get(smartFolderNotes)).toEqual([]);
    expect(get(openViewState)?.errors).toEqual([{ line: -1, key: '', message: VIEW_GONE }]);
  });

  it('a view that does not parse shows its errors and no list', async () => {
    smartFolderNotes.set([note('old')]);
    handlers.set('run_view', () => { throw JSON.stringify([{ line: 0, key: '', message: 'this view has no conditions' }]); });
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    await runSelectedView();
    expect(get(smartFolderNotes)).toEqual([]);
    expect(get(openViewState)?.errors).toEqual([{ line: 0, key: '', message: 'this view has no conditions' }]);
  });

  it('a slower run for view A that returns after view B opened does not overwrite B', async () => {
    let releaseA!: (r: ViewResult) => void;
    handlers.set('run_view', ({ uuid }) =>
      uuid === 'vA' ? new Promise<ViewResult>((r) => { releaseA = r; }) : notesResult('nB'));
    handlers.set('get_cached_note', ({ uuid }) => note(uuid as string));
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'vA' });
    const runA = runSelectedView();
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'vB' });
    await runSelectedView();
    releaseA(notesResult('nA'));
    await runA;
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['nB']);
    expect(get(openViewState)?.key).toBe(`${A}|vB`);
  });

  it('an older run of the same view that returns last does not put its rows back', async () => {
    let releaseOld!: (r: ViewResult) => void;
    handlers.set('run_view', () => new Promise<ViewResult>((r) => { releaseOld = r; }));
    handlers.set('get_cached_note', ({ uuid }) => note(uuid as string));
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const older = runSelectedView();
    const releaseFirst = releaseOld;
    handlers.set('run_view', () => notesResult('fresh'));
    await runSelectedView();
    releaseFirst(notesResult('stale'));
    await older;
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['fresh']);
  });
});

describe('onViewPainted', () => {
  it('fires for the run that paints, never for one superseded mid-flight', async () => {
    const { onViewPainted } = await import('./viewsStore');
    const fired: string[] = [];
    const stop = onViewPainted(() => fired.push(get(smartFolderNotes).map((n) => n.uuid).join(',')));
    let releaseOld!: (r: ViewResult) => void;
    handlers.set('run_view', () => new Promise<ViewResult>((r) => { releaseOld = r; }));
    handlers.set('get_cached_note', ({ uuid }) => note(uuid as string));
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const older = runSelectedView();
    const releaseFirst = releaseOld;
    let releaseNew!: (r: ViewResult) => void;
    handlers.set('run_view', () => new Promise<ViewResult>((r) => { releaseNew = r; }));
    const newer = runSelectedView(); // takes the token while the older run is in flight
    releaseFirst(notesResult('stale'));
    await older;
    expect(fired).toEqual([]);
    releaseNew(notesResult('fresh'));
    await newer;
    expect(fired).toEqual(['fresh']);
    stop();
  });

  it('fires for a view that does not parse (the pick settles as empty)', async () => {
    const { onViewPainted } = await import('./viewsStore');
    let n = 0;
    const stop = onViewPainted(() => { n += 1; });
    handlers.set('run_view', () => { throw 'run_view: database is locked'; });
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    await runSelectedView();
    expect(n).toBe(1);
    stop();
  });
});

describe('routes back to the screen (gotcha #6)', () => {
  it('a remote-changed for the open view\'s account re-reads its views and re-runs it', async () => {
    handlers.set('list_views', () => [def('v1', 'Today')]);
    handlers.set('run_view', () => notesResult('n1'));
    handlers.set('get_cached_note', ({ uuid }) => note(uuid as string));
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    invoke.mockClear();
    onRemoteChanged(A);
    await settle();
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: A });
    expect(invoke).toHaveBeenCalledWith('run_view', { accountId: A, uuid: 'v1' });
    expect(get(smartFolderNotes).map((n) => n.uuid)).toEqual(['n1']);
  });

  it('a remote-changed for another account re-reads that account\'s views only', async () => {
    handlers.set('list_views', () => []);
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    invoke.mockClear();
    onRemoteChanged(B);
    await settle();
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: B });
    expect(invoke.mock.calls.some(([c]) => c === 'run_view')).toBe(false);
  });

  it('a save of any note re-reads its account\'s views and re-runs the open view', async () => {
    handlers.set('list_views', () => [def('v1', 'Today')]);
    handlers.set('run_view', () => notesResult());
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const stop = startViewRefreshRoutes();
    await settle();
    invoke.mockClear();
    markRecentlySaved(A, 'n9');
    await settle();
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: A });
    expect(invoke).toHaveBeenCalledWith('run_view', { accountId: A, uuid: 'v1' });
    stop();
  });

  it('a second save in the same millisecond is still a save', async () => {
    handlers.set('list_views', () => []);
    handlers.set('run_view', () => notesResult());
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const stop = startViewRefreshRoutes();
    await settle();
    vi.spyOn(Date, 'now').mockReturnValue(1_800_000_000_000);
    markRecentlySaved(A, 'n1');
    await settle();
    invoke.mockClear();
    markRecentlySaved(B, 'n2');
    await settle();
    vi.restoreAllMocks();
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: B });
    stop();
  });

  it('subscribing does not count the saves already recorded', async () => {
    handlers.set('list_views', () => []);
    markRecentlySaved(A, 'old');
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const stop = startViewRefreshRoutes();
    await settle();
    expect(invoke.mock.calls.some(([c]) => c === 'run_view')).toBe(false);
    stop();
  });

  it('a move or delete re-reads every account\'s views and re-runs the open view', async () => {
    handlers.set('list_views', () => []);
    handlers.set('run_view', () => notesResult());
    selectedSmartFolder.set({ account: A, kind: 'view', uuid: 'v1' });
    const stop = startViewRefreshRoutes();
    await settle();
    invoke.mockClear();
    noteMutationRevision.update((n) => n + 1);
    await settle();
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: A });
    expect(invoke).toHaveBeenCalledWith('list_views', { accountId: B });
    expect(invoke).toHaveBeenCalledWith('run_view', { accountId: A, uuid: 'v1' });
    stop();
  });

  it('reads each active account\'s views on start and drops a removed account\'s', async () => {
    handlers.set('list_views', ({ accountId }) => [def(`v-${accountId}`, 'x')]);
    const stop = startViewRefreshRoutes();
    await settle();
    expect(Object.keys(get(viewsByAccount)).sort()).toEqual([A, B].sort());
    accounts.set([acct(A)]);
    await settle();
    expect(Object.keys(get(viewsByAccount))).toEqual([A]);
    stop();
  });
});

describe('createView', () => {
  it('returns the new view\'s uuid and folder, with the list re-read', async () => {
    handlers.set('create_view', () => 'v-new');
    handlers.set('list_views', () => [{ ...def('v-new', 'New view'), label: 'Notes/__Views__' }]);
    expect(await createView(A)).toEqual({ uuid: 'v-new', label: 'Notes/__Views__' });
    expect(get(viewsByAccount)[A].map((v) => v.uuid)).toEqual(['v-new']);
  });

  it('throws the refusal text verbatim', async () => {
    handlers.set('create_view', () => { throw 'Views need an account that can create folders.'; });
    await expect(createView('microsoft:x@y.z')).rejects.toThrow('Views need an account that can create folders.');
  });
});

describe('due text', () => {
  const now = new Date(2026, 10, 19, 15, 0); // 19 Nov 2026, local
  it('says today, tomorrow, overdue and future in words', async () => {
    const { dueText, dueState } = await import('./viewsStore');
    expect(dueText('2026-11-19', now)).toBe('Due today');
    expect(dueText('2026-11-20', now)).toBe('Due tomorrow');
    expect(dueText('2026-11-17', now, 'en-GB')).toMatch(/^Overdue, 17 Nov/);
    expect(dueText('2026-11-25', now, 'en-GB')).toMatch(/^Due 25 Nov/);
    expect(dueState('2026-11-18', now)).toBe('overdue');
    expect(dueState('2026-11-19', now)).toBe('today');
    expect(dueState('2026-12-01', now)).toBe('future');
  });
});

describe('viewGroupsFor', () => {
  it('one account: its views, unheaded, even when it has none', async () => {
    const { viewGroupsFor } = await import('./viewsStore');
    expect(viewGroupsFor(A, [{ id: A }, { id: B }], {})).toEqual([{ accountId: A, headed: false, views: [] }]);
  });

  it('All accounts: each account that has views, under its name, in account order', async () => {
    const { viewGroupsFor } = await import('./viewsStore');
    const views = { [B]: [def('vb', 'B view')], [A]: [] };
    expect(viewGroupsFor(null, [{ id: A }, { id: B }], views)).toEqual([{ accountId: B, headed: true, views: [def('vb', 'B view')] }]);
  });

  it('viewIcon: a checklist for a tasks view, a search otherwise', async () => {
    const { viewIcon } = await import('./viewsStore');
    expect(viewIcon({ ...def('v', 'x'), lines: ['Show: tasks', 'Done: no'] })).toBe('checkbox');
    expect(viewIcon(def('v', 'x'))).toBe('search');
  });
});
