// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import NoteList from './NoteList.svelte';
import { notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex,
  hydratedFolders, selectedTags, selectedSmartFolder, searchQuery, selectedUuids,
  capabilitiesByAccount, noteTagsByAccount, isLoading } from '../stores/notes';
import type { Note } from '../types';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
function deferred() {
  let resolve!: (notes: Note[]) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<Note[]>((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}
const note = (title: string): Note => ({ uuid: title, id: title, account_id: 'a', title,
  body_html: '<p>ประชุม</p>', date: '2026-09-19', label: 'Notes', x_mail_created_date: null });
let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  vi.useFakeTimers(); invoke.mockReset(); invoke.mockResolvedValue([]);
  notes.set([]); selectedNote.set(null); selectedFolder.set('Notes'); currentAccount.set('a');
  accounts.set([]); noteIndex.set(new Map()); hydratedFolders.set(new Map());
  selectedTags.set(new Set()); selectedSmartFolder.set(null); selectedUuids.set(new Set());
  searchQuery.set(''); capabilitiesByAccount.set({}); noteTagsByAccount.set(new Map()); isLoading.set(false);
  host = document.createElement('div'); document.body.append(host);
  component = mount(NoteList, { target: host }); flushSync();
});
afterEach(async () => { await dispose(); host.remove(); vi.useRealTimers(); });
async function dispose() { if (component) { await unmount(component); component = undefined!; } }
const query = (q: string) => { searchQuery.set(q); flushSync(); };
const settle = async () => { await vi.advanceTimersByTimeAsync(0); flushSync(); };
const start = async () => { await vi.advanceTimersByTimeAsync(150); flushSync(); };
function scope(value: string) {
  const select = host.querySelector('select')!;
  select.value = value; select.dispatchEvent(new Event('change', { bubbles: true })); flushSync();
}
const titles = () => [...host.querySelectorAll('.note-title')].map(e => e.textContent?.trim());
it('invalidates A immediately when B is typed, before B debounce', async () => {
  const a = deferred(); invoke.mockReturnValueOnce(a.promise);
  query('A'); await start(); query('B'); a.resolve([note('obsolete')]); await settle();
  expect(titles()).toEqual([]);
  expect(host.textContent).toContain('Searching');
});
it('drops out-of-order successes and failures', async () => {
  const a = deferred(), b = deferred(); invoke.mockReturnValueOnce(a.promise).mockReturnValueOnce(b.promise);
  query('A'); await start(); query('B'); await start(); b.resolve([note('current')]); await settle();
  a.reject(new Error('old error')); await settle();
  expect(titles()).toEqual(['current']); expect(host.textContent).not.toContain('old error');
});
it('invalidates clear/retype even for the same query', async () => {
  const a = deferred(); invoke.mockReturnValueOnce(a.promise);
  query('A'); await start(); query(''); query('A'); a.resolve([note('obsolete')]); await settle();
  expect(titles()).toEqual([]);
});
it.each(['scope', 'account', 'folder'])('invalidates on %s changes before debounce', async (change) => {
  const a = deferred(); invoke.mockReturnValueOnce(a.promise);
  query('ประชุม'); if (change === 'folder') scope('folder'); await start();
  if (change === 'scope') scope('all');
  if (change === 'account') currentAccount.set('b');
  if (change === 'folder') selectedFolder.set('Notes/Work');
  flushSync(); a.resolve([note('obsolete')]); await settle(); expect(titles()).toEqual([]);
  await start();
  expect(invoke).toHaveBeenLastCalledWith('search_notes', {
    query: 'ประชุม', accountId: change === 'scope' ? null : change === 'account' ? 'b' : 'a',
    label: change === 'folder' ? 'Notes/Work' : null,
  });
});
it('clears old matches, reports failure, and retries the current query', async () => {
  invoke.mockResolvedValueOnce([note('old')]); query('A'); await start();
  invoke.mockRejectedValueOnce(new Error('cache unavailable')); query('B');
  expect(titles()).toEqual([]); await start();
  expect(host.textContent).toContain('Could not search');
  const retry = [...host.querySelectorAll('button')].find(b => b.textContent === 'Retry');
  expect(retry).toBeDefined(); invoke.mockResolvedValueOnce([note('retried')]); retry!.click(); await start();
  expect(titles()).toEqual(['retried']);
});
it('shows search-specific empty copy and a scope-matched placeholder', async () => {
  expect(host.querySelector('input')?.placeholder).toBe('Search this account...');
  query('ไม่มี'); scope('folder'); await start();
  expect(host.querySelector('input')?.placeholder).toBe('Search this folder...');
  expect(host.textContent).toContain('No results');
  expect(host.textContent).not.toContain('No notes in this folder');
});
it('cancels the debounce timer on unmount', async () => {
  query('A'); await dispose(); await start(); expect(invoke).not.toHaveBeenCalled();
});
it('ignores a pending reply after unmount', async () => {
  const a = deferred(); invoke.mockReturnValueOnce(a.promise); query('A'); await start();
  await dispose(); a.resolve([note('late')]); await settle(); expect(host.textContent).toBe('');
});
it('invalidates cross-account tags and exposes loading/error/retry', async () => {
  noteTagsByAccount.set(new Map([['a', new Map([['obsolete', ['B']], ['current', ['B']]])]]));
  selectedTags.set(new Set(['A'])); flushSync();
  const a = deferred(), b = deferred(); invoke.mockReturnValueOnce(a.promise).mockReturnValueOnce(b.promise);
  scope('all'); selectedTags.set(new Set(['B'])); flushSync();
  a.resolve([note('obsolete')]); await settle(); expect(titles()).toEqual([]);
  expect(host.textContent).toContain('Loading tagged notes');
  b.reject(new Error('cache unavailable')); await settle(); expect(host.textContent).toContain('Could not load tagged notes');
  invoke.mockResolvedValueOnce([note('current')]);
  [...host.querySelectorAll('button')].find(b => b.textContent === 'Retry')!.click(); await settle();
  expect(titles()).toEqual(['current']);
});
it('keeps the newer success when an older success arrives last', async () => {
  const a = deferred(), b = deferred(); invoke.mockReturnValueOnce(a.promise).mockReturnValueOnce(b.promise);
  query('A'); await start(); query('B'); await start(); b.resolve([note('new')]); await settle();
  a.resolve([note('old')]); await settle(); expect(titles()).toEqual(['new']);
});
it.each(['account', 'scope', 'clear'])('invalidates tag requests on %s changes', async (change) => {
  noteTagsByAccount.set(new Map([['a', new Map([['old', ['A']]])]]));
  selectedTags.set(new Set(['A'])); flushSync();
  const a = deferred(); invoke.mockReturnValueOnce(a.promise); scope('all');
  if (change === 'account') currentAccount.set('b');
  if (change === 'scope') scope('account');
  if (change === 'clear') selectedTags.set(new Set());
  flushSync(); a.resolve([note('old')]); await settle(); expect(titles()).toEqual([]);
});
it('does not turn a cleared search into an error when its pending request fails', async () => {
  const a = deferred(); invoke.mockReturnValueOnce(a.promise); query('A'); await start(); query('');
  a.reject(new Error('old error')); await settle();
  expect(host.textContent).not.toContain('Could not search');
});
it('handles pending tag failure after unmount', async () => {
  selectedTags.set(new Set(['A'])); flushSync();
  const a = deferred(); invoke.mockReturnValueOnce(a.promise); scope('all'); await dispose();
  a.reject(new Error('old error')); await settle(); expect(host.textContent).toBe('');
});
it('describes the account-wide fallback for virtual folders without changing backend scope', async () => {
  selectedFolder.set('__ALL__'); query('ประชุม'); scope('folder'); await start();
  expect(host.querySelector('input')?.placeholder).toBe('Search this account...');
  expect(host.querySelector('option[value="folder"]')?.textContent).toBe('This account (all folders)');
  expect(invoke).toHaveBeenLastCalledWith('search_notes', { accountId: 'a', label: null, query: 'ประชุม' });
});
it('treats whitespace as a cleared search', async () => {
  query('   '); await start(); expect(invoke).not.toHaveBeenCalled();
  expect(host.querySelector('select[aria-label="Search scope"]')).toBeNull();
  expect(host.textContent).not.toContain('Results:');
});
