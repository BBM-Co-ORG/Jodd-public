// Saved views in the app (spec 2026-11-19-properties-tasks-views §8.5): the
// per-account list the sidebar's Views fold shows, and the open view's result
// the middle pane shows. Everything here reads SQLite through the Task 8
// commands (`list_views`, `run_view`, `create_view`, `get_cached_note`) —
// never the remote.
//
// Two rules shape this file:
//
// - **Latest call wins.** A `run_view` for view A that returns after the user
//   opened view B must not paint A's rows under B's name; a `list_views` that
//   returns after a newer one must not put the older list back. Each load
//   carries a token and applies only while it is still the newest.
// - **Gotcha #6: a route back to the pixel.** The sync worker changes notes
//   with no user command behind it, and a local save changes the derived
//   tables a view reads. `onListingRefreshed` (App's `loadNotes` /
//   `loadFolderNotes`, after the rows land — the pull itself on most
//   backends), `onRemoteChanged` (App's `remote-changed` listener),
//   `onPinChanged` and `startViewRefreshRoutes` (saves, moves, deletes,
//   account changes) re-read the list and re-run the open view.
import { invoke } from '@tauri-apps/api/core';
import { get, writable, type Writable } from 'svelte/store';
import {
  activeAccounts, noteMutationRevision, notes, recentlySavedUuids, selectedSmartFolder,
  smartFolderNotes, type SmartSelection,
} from '../stores/notes';
import type { Note } from '../types';
import { calendarPref, resolveCalendar } from '../format/dates';

/** `views::query::ViewError`. `line` is 0-based within the property block. */
export interface ViewError {
  line: number;
  key: string;
  message: string;
}

/** `views::run::ViewDef`. */
export interface ViewDef {
  uuid: string;
  name: string;
  label: string;
  lines: string[];
  errors: ViewError[];
}

/** `views::run::NoteRow`. Dates are `YYYY-MM-DD`. */
export interface ViewNoteRow {
  uuid: string;
  title: string;
  label: string;
  modified_ms: number;
  task_count: number;
  earliest_due: string | null;
}

/** `views::run::TaskRow`. */
export interface ViewTaskRow {
  uuid: string;
  title: string;
  label: string;
  idx: number;
  text: string;
  done: boolean;
  due: string | null;
}

/** `views::run::ViewRows`, serde's externally tagged enum. */
export type ViewRows = { Notes: ViewNoteRow[] } | { Tasks: ViewTaskRow[] };

/** `views::run::ViewResult`. */
export interface ViewResult {
  rows: ViewRows;
  notices: string[];
}

/** What the middle pane shows beside a task view's note row (§6.5). */
export interface ViewRowMeta {
  taskCount: number;
  earliestDue: string | null;
}

/** The open view's state, for the middle pane. */
export interface OpenViewState {
  /** `account|uuid` of the view this answers. */
  key: string;
  /** The view does not run: these say why, and there is no list. */
  errors: ViewError[] | null;
  notices: string[];
  show: 'notes' | 'tasks';
  /** Per note (`uuid`), for a tasks view. */
  meta: Map<string, ViewRowMeta>;
}

/** Each account's views, by account id. Absent until its first read. */
export const viewsByAccount: Writable<Record<string, ViewDef[]>> = writable({});

/** The open view's errors, notices and per-row meta; null when none is open. */
export const openViewState = writable<OpenViewState | null>(null);

const viewKey = (accountId: string, uuid: string) => `${accountId}|${uuid}`;

function selectedView(sf: SmartSelection | null): { account: string; uuid: string } | null {
  return sf && sf.kind === 'view' ? { account: sf.account, uuid: sf.uuid } : null;
}

/** The selected view's name, from the account's list; null while unknown. */
export function viewNameOf(views: Record<string, ViewDef[]>, sf: SmartSelection | null): string | null {
  const v = selectedView(sf);
  if (!v) return null;
  return views[v.account]?.find((d) => d.uuid === v.uuid)?.name ?? null;
}

const listSeq = new Map<string, number>();

/** Re-read one account's views. A failed read keeps the list it had. */
export async function refreshViews(accountId: string): Promise<void> {
  const token = (listSeq.get(accountId) ?? 0) + 1;
  listSeq.set(accountId, token);
  let list: ViewDef[];
  try {
    list = await invoke<ViewDef[]>('list_views', { accountId });
  } catch (e) {
    console.error(`list_views failed for ${accountId}`, e);
    return;
  }
  if (listSeq.get(accountId) !== token) return;
  // Gone while the read ran: do not bring a removed account's views back.
  if (!get(activeAccounts).some((a) => a.id === accountId)) return;
  viewsByAccount.update((m) => ({ ...m, [accountId]: list }));
}

function isViewError(x: unknown): x is ViewError {
  if (!x || typeof x !== 'object') return false;
  const e = x as Record<string, unknown>;
  return typeof e.line === 'number' && typeof e.key === 'string' && typeof e.message === 'string';
}

/**
 * `run_view`'s refusal as a list of errors. A view that does not parse is
 * `Err` carrying the JSON of its `ViewError`s; anything else (a DB failure)
 * is plain text, shown as one error with no line.
 */
export function viewErrorsFrom(e: unknown): ViewError[] {
  const text = typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
  try {
    const parsed: unknown = JSON.parse(text);
    if (Array.isArray(parsed) && parsed.length > 0 && parsed.every(isViewError)) return parsed;
  } catch {
    // Not JSON: plain text below.
  }
  return [{ line: -1, key: '', message: text }];
}

/** Run a saved view. A view that does not run answers `{ errors }`. */
export async function openView(accountId: string, uuid: string): Promise<ViewResult | { errors: ViewError[] }> {
  try {
    return await invoke<ViewResult>('run_view', { accountId, uuid });
  } catch (e) {
    return { errors: viewErrorsFrom(e) };
  }
}

/**
 * The notes a result lists, in the engine's order, with a tasks view's
 * per-note count and earliest due (§6.5: one row per note that holds at
 * least one matching task row).
 */
export function viewListing(result: ViewResult): { show: 'notes' | 'tasks'; uuids: string[]; meta: Map<string, ViewRowMeta> } {
  const meta = new Map<string, ViewRowMeta>();
  if ('Notes' in result.rows) {
    return { show: 'notes', uuids: result.rows.Notes.map((r) => r.uuid), meta };
  }
  const uuids: string[] = [];
  for (const t of result.rows.Tasks) {
    const m = meta.get(t.uuid);
    if (!m) {
      uuids.push(t.uuid);
      meta.set(t.uuid, { taskCount: 1, earliestDue: t.due });
    } else {
      m.taskCount += 1;
      if (t.due && (!m.earliestDue || t.due < m.earliestDue)) m.earliestDue = t.due;
    }
  }
  return { show: 'tasks', uuids, meta };
}

/**
 * The full notes for `uuids`, in that order: the in-memory copy when there is
 * one, else the SQLite row. A row the cache no longer holds is left out.
 */
async function notesFor(accountId: string, uuids: string[]): Promise<Note[]> {
  const loaded = new Map(
    get(notes).filter((n) => n.account_id === accountId).map((n) => [n.uuid, n] as const),
  );
  const found = await Promise.all(uuids.map(async (uuid) => {
    const inMemory = loaded.get(uuid);
    if (inMemory) return inMemory;
    try {
      return await invoke<Note | null>('get_cached_note', { accountId, uuid });
    } catch (e) {
      console.error('get_cached_note failed for a view row', e);
      return null;
    }
  }));
  return found.filter((n): n is Note => !!n);
}

let runSeq = 0;

/** What the open view says when its note is no longer a view. */
export const VIEW_GONE = 'This view was deleted.';

const paintListeners = new Set<() => void>();

/**
 * Called after every run that paints, whatever started it (a pick, a save, a
 * `remote-changed`). App settles a waiting pick here (`listingLoaded`): only
 * the run that paints may settle it — a run superseded mid-flight paints
 * nothing, and settling then would answer the pick with an empty list while
 * the newer run's rows arrive with nothing left to open the first note.
 * Returns the unsubscribe.
 */
export function onViewPainted(fn: () => void): () => void {
  paintListeners.add(fn);
  return () => { paintListeners.delete(fn); };
}

function painted(): void {
  for (const fn of paintListeners) fn();
}

/**
 * Run the selected view and paint its result: the notes into
 * `smartFolderNotes` (NoteList's virtual listing), errors / notices / meta
 * into `openViewState`. Applies only if it is still the newest run and the
 * view is still the one selected; then tells `onViewPainted` listeners.
 */
export async function runSelectedView(): Promise<void> {
  const v = selectedView(get(selectedSmartFolder));
  if (!v) return;
  const token = ++runSeq;
  const key = viewKey(v.account, v.uuid);
  const current = () => {
    const now = selectedView(get(selectedSmartFolder));
    return token === runSeq && !!now && viewKey(now.account, now.uuid) === key;
  };
  const result = await openView(v.account, v.uuid);
  if (!current()) return;
  // Deleted (or moved out of `__Views__`) elsewhere: its uuid now has no
  // property block, which `run_view` would report as "no conditions". The
  // account's list, once read, says what actually happened.
  const listed = get(viewsByAccount)[v.account];
  if (listed && !listed.some((d) => d.uuid === v.uuid)) {
    smartFolderNotes.set([]);
    openViewState.set({ key, errors: [{ line: -1, key: '', message: VIEW_GONE }], notices: [], show: 'notes', meta: new Map() });
    painted();
    return;
  }
  if ('errors' in result) {
    smartFolderNotes.set([]);
    openViewState.set({ key, errors: result.errors, notices: [], show: 'notes', meta: new Map() });
    painted();
    return;
  }
  const listing = viewListing(result);
  const rows = await notesFor(v.account, listing.uuids);
  if (!current()) return;
  smartFolderNotes.set(rows);
  openViewState.set({ key, errors: null, notices: result.notices, show: listing.show, meta: listing.meta });
  painted();
}

/** The open view's state when it answers `sf`, else null. */
export function stateFor(state: OpenViewState | null, sf: SmartSelection | null): OpenViewState | null {
  const v = selectedView(sf);
  return state && v && state.key === viewKey(v.account, v.uuid) ? state : null;
}

/**
 * Bumped whenever the rows a view reads may have changed — a listing landed,
 * `remote-changed`, a save, a move/delete/transfer, a pin. The live strip on
 * an open view note (`ViewPreview`) re-runs on it: its count covers the whole
 * account, not only the note being edited (gotcha #6).
 */
export const viewDataRevision = writable(0);

function viewDataChanged(): void {
  viewDataRevision.update((n) => n + 1);
}

/** Re-run the open view if it belongs to one of `accountIds` (all when omitted). */
function rerunOpenView(accountIds?: Iterable<string>): void {
  viewDataChanged();
  const v = selectedView(get(selectedSmartFolder));
  if (!v) return;
  if (accountIds && !new Set(accountIds).has(v.account)) return;
  void runSelectedView();
}

/**
 * App's `remote-changed` listener: the worker changed `accountId`'s notes, so
 * its views and the open view (if it is that account's) are re-read (gotcha #6).
 *
 * Not enough on its own. On iCloud the detector emits BEFORE the pull writes
 * SQLite (`detect_remote_changes` drops the cache and hands the read to the
 * frontend's refresh), and on Gmail / Microsoft / LocalFs / SSH a remote edit
 * reaches SQLite only through the frontend's own listing reads, with no event
 * at all. `onListingRefreshed` is the route for both.
 */
export function onRemoteChanged(accountId: string): void {
  void refreshViews(accountId);
  rerunOpenView([accountId]);
}

/**
 * A listing read (`list_notes` / `list_notes_in_folder`) finished and its rows
 * reached the stores: that read IS the pull for most backends, so whatever it
 * wrote to SQLite is what the Views fold and the open view must show now
 * (gotcha #6). The fold is re-read first and the open view re-run after it, so
 * a view deleted elsewhere is seen as deleted rather than as an empty query.
 */
export async function onListingRefreshed(accountIds: Iterable<string>): Promise<void> {
  const ids = [...accountIds];
  if (ids.length === 0) return;
  await Promise.all(ids.map((id) => refreshViews(id)));
  rerunOpenView(ids);
}

/**
 * A pin or unpin reached SQLite (`set_pin` / `set_pin_batch` resolved): a
 * `Pinned:` view's membership moved with no save, move or pull behind it.
 * The views list itself is unchanged, so only the open view re-runs.
 */
export function onPinChanged(accountId: string): void {
  rerunOpenView([accountId]);
}

/**
 * The local routes back (gotcha #6): a save (`recentlySavedUuids`), a move,
 * delete or transfer (`noteMutationRevision`), and the active accounts
 * changing. Returns the unsubscribe. Called once by App.
 */
export function startViewRefreshRoutes(): () => void {
  // Saves: the accounts holding a save stamp not seen before. Per note, not a
  // high-water time, so two saves in the same millisecond are both seen. The
  // stamps already there on subscribe are history, not a new save.
  let seen: Map<string, number> | null = null;
  const unsubSaves = recentlySavedUuids.subscribe((m) => {
    const changed = new Set<string>();
    const now = new Map<string, number>();
    for (const [account, inner] of m) {
      for (const [uuid, t] of inner) {
        const key = `${account}|${uuid}`;
        now.set(key, t);
        if (seen && seen.get(key) !== t) changed.add(account);
      }
    }
    seen = now;
    if (changed.size === 0) return;
    for (const a of changed) void refreshViews(a);
    rerunOpenView(changed);
  });

  // Moves, deletes, transfers: no account on the signal, so every active one.
  let firstRevision = true;
  const unsubMutations = noteMutationRevision.subscribe(() => {
    if (firstRevision) { firstRevision = false; return; }
    for (const a of get(activeAccounts)) void refreshViews(a.id);
    rerunOpenView();
  });

  // Accounts: read a newly active account's views, drop a removed one's.
  let known = new Set<string>();
  const unsubAccounts = activeAccounts.subscribe((list) => {
    const ids = new Set(list.map((a) => a.id));
    for (const id of ids) if (!known.has(id)) void refreshViews(id);
    if ([...known].some((id) => !ids.has(id))) {
      viewsByAccount.update((m) => Object.fromEntries(Object.entries(m).filter(([id]) => ids.has(id))));
    }
    known = ids;
  });

  return () => {
    unsubSaves();
    unsubMutations();
    unsubAccounts();
  };
}

/** The device's local date as `YYYY-MM-DD` ("today" is never stored, §6.3). */
function localIsoDate(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

function dayOffset(due: string, now: Date): number {
  const [y, m, d] = due.split('-').map(Number);
  const [ty, tm, td] = localIsoDate(now).split('-').map(Number);
  return Math.round((Date.UTC(y, m - 1, d) - Date.UTC(ty, tm - 1, td)) / 86_400_000);
}

/** A due date against today: what colour its text takes. */
export function dueState(due: string, now: Date = new Date()): 'overdue' | 'today' | 'future' {
  const days = dayOffset(due, now);
  return days < 0 ? 'overdue' : days === 0 ? 'today' : 'future';
}

/** A task view row's earliest due, in words: "Due today", "Overdue, 18 Nov". */
export function dueText(due: string, now: Date = new Date(), locale?: string): string {
  const days = dayOffset(due, now);
  if (days === 0) return 'Due today';
  if (days === 1) return 'Due tomorrow';
  const [y, m, d] = due.split('-').map(Number);
  const calendar = resolveCalendar(get(calendarPref));
  const day = new Date(y, m - 1, d).toLocaleDateString(locale ?? [], { month: 'short', day: 'numeric', calendar });
  return days < 0 ? `Overdue, ${day}` : `Due ${day}`;
}

/**
 * What the Views fold says in New view's place for an account that cannot
 * create folders (§8.7) — `create_view`'s own refusal, word for word.
 */
export const VIEWS_NEED_FOLDERS = 'Views need an account that can create folders.';

/** One account's saved views as the Views fold lists them. */
export interface ViewGroup {
  accountId: string;
  /** Named under its account: All accounts lists every account's views. */
  headed: boolean;
  views: ViewDef[];
}

/**
 * The Views fold's saved-view rows (§8.5): the scoped account's views, or —
 * under All accounts (`scopeAccount: null`) — each active account that has
 * views, grouped under its name.
 */
export function viewGroupsFor(
  scopeAccount: string | null,
  active: { id: string }[],
  views: Record<string, ViewDef[]>,
): ViewGroup[] {
  if (scopeAccount !== null) return [{ accountId: scopeAccount, headed: false, views: views[scopeAccount] ?? [] }];
  return active
    .filter((a) => (views[a.id]?.length ?? 0) > 0)
    .map((a) => ({ accountId: a.id, headed: true, views: views[a.id] }));
}

/** A view row's icon: a checklist for a tasks view, a search otherwise. */
export function viewIcon(v: ViewDef): 'checkbox' | 'search' {
  return v.lines.some((l) => /^show:\s*tasks\s*$/i.test(l.replace(/ /g, ' ').trim())) ? 'checkbox' : 'search';
}

/** Why a New view could not be made: `create_view`'s refusal, verbatim. */
export function createViewRefusal(e: unknown): string {
  return typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
}

/**
 * Make a starter view in `accountId` and return its uuid and folder, after
 * re-reading the account's views so the new row is in the fold. Throws
 * `create_view`'s refusal text (e.g. "Views need an account that can create
 * folders.").
 */
export async function createView(accountId: string): Promise<{ uuid: string; label: string }> {
  let uuid: string;
  try {
    uuid = await invoke<string>('create_view', { accountId });
  } catch (e) {
    throw new Error(createViewRefusal(e));
  }
  await refreshViews(accountId);
  const label = get(viewsByAccount)[accountId]?.find((v) => v.uuid === uuid)?.label ?? 'Notes/__Views__';
  return { uuid, label };
}
