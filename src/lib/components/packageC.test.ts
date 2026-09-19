// @vitest-environment jsdom
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import NoteEditor from './NoteEditor.svelte';
import NoteList from './NoteList.svelte';
import { moveNoteOptimistic } from '../moveNotes';
import * as stores from '../stores/notes';
import { persistenceByNote, pendingLocalEdits, savingNoteKeys, refreshNotePersistence, rekeyNoteStores } from '../notePersistence';
import { clearNoteAliases, noteKey } from '../noteIdentity';
import { deleteNoteOptimistic } from '../deleteNote';
import type { Note } from '../types';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const a: Note = { account_id: 'a', uuid: 'same', id: 'a-id', title: 'Alpha', body_html: '<body>Alpha body</body>', label: 'Notes', date: '2026-09-19', local_version: 2 };
const b: Note = { ...a, account_id: 'b', id: 'b-id', title: 'Beta', body_html: '<body>Beta body</body>' };
let host: HTMLElement, app: ReturnType<typeof mount>;
beforeEach(() => {
  vi.useFakeTimers(); clearNoteAliases(); persistenceByNote.set({}); pendingLocalEdits.set({}); savingNoteKeys.set(new Set());
  Object.defineProperty(HTMLElement.prototype, 'innerText', { configurable: true, get() { return this.textContent ?? ''; } });
  invoke.mockReset(); invoke.mockImplementation((cmd, args) => Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : cmd === 'note_persistence' ? { account_id: args.accountId, uuid: args.uuid, local_version: 2, sync_state: 'dirty', push_blocked_reason: null } : []));
  stores.accounts.set([]); stores.currentAccount.set('a'); stores.notes.set([a,b]); stores.selectedNote.set(a);
  stores.selectedFolder.set('Notes'); stores.selectedSmartFolder.set(null); stores.selectedTags.set(new Set()); stores.selectedUuids.set(new Set()); stores.searchQuery.set(''); stores.isSaving.set(false);
  host = document.createElement('div'); document.body.append(host);
});
afterEach(async () => { if (app) await unmount(app); app = undefined!; host.remove(); stores.selectedNote.set(null); vi.useRealTimers(); });
async function settle() { await tick(); await vi.advanceTimersByTimeAsync(0); flushSync(); await tick(); }
it('switches editor and flushes the old account when UUIDs collide', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'unsaved Alpha'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  invoke.mockImplementation((cmd) => cmd === 'save_note' ? new Promise(() => {}) : Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : []));
  stores.selectedNote.set(b); await settle();
  expect(editor.textContent).toBe('Beta body');
  expect(invoke.mock.calls.find(([c]) => c === 'save_note')?.[1]).toMatchObject({ accountId: 'a', existingUuid: 'same' });
});
it('renders both colliding rows and selects only the clicked account', async () => {
  invoke.mockResolvedValue([a,b]); app = mount(NoteList, { target: host }); flushSync();
  stores.searchQuery.set('body'); flushSync(); await vi.advanceTimersByTimeAsync(150); await settle();
  const rows = host.querySelectorAll('.note-item'); expect(rows).toHaveLength(2);
  (rows[1].querySelector('.note-btn') as HTMLElement).dispatchEvent(new MouseEvent('click', { bubbles: true, ctrlKey: true })); flushSync(); expect(get(stores.selectedNote)?.account_id).toBe('b');
  expect(host.querySelectorAll('.note-item.active')).toHaveLength(1);
});
it('moves only the requested account and rolls back without changing its twin', async () => {
  invoke.mockRejectedValue(new Error('offline'));
  const pending = moveNoteOptimistic({ accountId: 'b', uuid: 'same', fromLabel: 'Notes', toLabel: 'Notes/Moved', remoteId: 'b-id' });
  expect(get(stores.notes).map(n => n.label)).toEqual(['Notes', 'Notes/Moved']);
  await pending; expect(get(stores.notes).map(n => n.label)).toEqual(['Notes', 'Notes']);
});
it('shows locally saved and pending from the persisted dirty row on reopen', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  expect(host.querySelector('.save-status')?.textContent).toContain('Saved on this device');
  expect(host.querySelector('.save-status')?.textContent).toContain('Sync pending');
});

it('refreshes pending → blocked → synced for only the current version without touching the DOM edit', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  let state = { account_id: 'a', uuid: 'same', local_version: 2, sync_state: 'dirty', push_blocked_reason: null as string | null };
  invoke.mockImplementation((cmd) => Promise.resolve(cmd === 'note_persistence' ? state : []));
  state = { ...state, push_blocked_reason: 'unsupported content' };
  await refreshNotePersistence(a); await settle(); expect(host.textContent).toContain('Sync blocked');
  state = { ...state, sync_state: 'clean', push_blocked_reason: null };
  await refreshNotePersistence(a); await settle(); expect(host.querySelector('.save-status')?.textContent).toBe('Synced');
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'new edit during push'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  await refreshNotePersistence(a); await settle();
  expect(editor.textContent).toBe('new edit during push');
  expect(host.querySelector('.save-status')?.textContent).toContain('Unsaved');
});
it('rekeys an open dirty editor, selection and caches without losing typed text or the other account', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  stores.selectedUuids.set(new Set([noteKey(a), noteKey(b)]));
  stores.setNoteTags('a', 'same', ['tag']); stores.setFolderSuggestion('a', 'same', { path: 'Notes/Next', reason: null, ai_result_id: 'B-eligibility' });
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'unsaved across rekey'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  rekeyNoteStores('a', 'same', 'assigned'); await settle();
  expect(editor.textContent).toBe('unsaved across rekey');
  expect(get(stores.notes).map(n => [n.account_id, n.uuid])).toEqual([['a', 'assigned'], ['b', 'same']]);
  expect(get(stores.selectedUuids)).toEqual(new Set([noteKey({ ...a, uuid: 'assigned' }), noteKey(b)]));
  expect(get(stores.noteTagsByAccount).get('a')?.get('assigned')).toEqual(['tag']);
  expect(get(stores.folderSuggestions)[stores.folderSuggestionKey('a', 'assigned')].ai_result_id).toBe('B-eligibility');
});
it('optimistically deletes only one account and rolls back without erasing newer unrelated work', async () => {
  let reject!: (e: Error) => void;
  invoke.mockReturnValue(new Promise((_, r) => { reject = r; }));
  const pending = deleteNoteOptimistic(a, 'a');
  expect(get(stores.notes)).toEqual([b]); expect(get(stores.selectedNote)).toBeNull();
  const newerB = { ...b, title: 'newer Beta' }; stores.notes.set([newerB]); stores.selectedNote.set(newerB);
  reject(new Error('SQLite unavailable')); expect(await pending).toBe(false);
  expect(get(stores.notes)).toEqual([newerB, a]); expect(get(stores.selectedNote)).toEqual(newerB);
  expect(get(stores.error)).toContain('Could not delete');
});
it('keeps a newer DOM edit unsaved when an earlier local save finishes', async () => {
  let resolve!: (n: unknown) => void;
  app = mount(NoteEditor, { target: host }); await settle();
  invoke.mockImplementation((cmd, args) => cmd === 'save_note' ? new Promise(r => { resolve = r; }) : Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : cmd === 'note_persistence' ? { account_id: args.accountId, uuid: args.uuid, local_version: 3, sync_state: 'clean', push_blocked_reason: null } : []));
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'first edit'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  await vi.advanceTimersByTimeAsync(1500);
  editor.innerHTML = 'second edit while saving'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  resolve({ id: 'a-id', uuid: 'same', local_version: 3 }); await settle();
  expect(editor.textContent).toBe('second edit while saving');
  expect(get(stores.selectedNote)?.body_html).toContain('second edit while saving');
  expect(host.querySelector('.save-status')?.textContent).toContain('Unsaved');
});
it('a late save after switching to the same UUID in another account cannot replace that editor', async () => {
  let resolve!: (n: unknown) => void;
  app = mount(NoteEditor, { target: host }); await settle();
  invoke.mockImplementation((cmd) => cmd === 'save_note' ? new Promise(r => { resolve = r; }) : Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : []));
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'saved Alpha only'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  stores.selectedNote.set(b); await settle();
  resolve({ id: 'new-a', uuid: 'rekeyed-a', local_version: 3 }); await settle();
  expect(get(stores.selectedNote)).toEqual(b); expect(editor.textContent).toBe('Beta body');
  expect(get(stores.notes).find(n => n.account_id === 'b')).toEqual(b);
  expect(get(stores.notes).find(n => n.account_id === 'a')?.uuid).toBe('rekeyed-a');
});
it('search snapshots hide an optimistic deletion and refresh after rollback', async () => {
  invoke.mockResolvedValue([a,b]); app = mount(NoteList, { target: host }); flushSync();
  stores.searchQuery.set('body'); flushSync(); await vi.advanceTimersByTimeAsync(150); await settle();
  let reject!: (e: Error) => void;
  invoke.mockImplementation((cmd) => cmd === 'delete_note' ? new Promise((_, r) => { reject = r; }) : Promise.resolve([a,b]));
  const pending = deleteNoteOptimistic(a, 'a'); flushSync();
  expect([...host.querySelectorAll('.note-title')].map(n => n.textContent?.trim())).toEqual(['Beta']);
  reject(new Error('refused')); await pending; flushSync(); await vi.advanceTimersByTimeAsync(150); await settle();
  expect(host.querySelectorAll('.note-item')).toHaveLength(2);
});
it('a failed local edit remains visibly unsaved when reopened, even if the older DB version is clean', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  invoke.mockImplementation((cmd, args) => cmd === 'save_note' ? Promise.reject(new Error('disk busy')) : Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : cmd === 'note_persistence' ? { account_id: args.accountId, uuid: args.uuid, local_version: 2, sync_state: 'clean', push_blocked_reason: null } : []));
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'not yet on disk'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  await vi.advanceTimersByTimeAsync(1500); await settle();
  const draft = get(stores.notes).find(n => n.account_id === 'a')!;
  stores.selectedNote.set(b); await settle(); stores.selectedNote.set(draft); await settle();
  expect(editor.textContent).toBe('not yet on disk');
  expect(host.querySelector('.save-status')?.textContent).toContain('Unsaved');
});
it('a delayed save response cannot resurrect an optimistically deleted note', async () => {
  let resolve!: (n: unknown) => void;
  app = mount(NoteEditor, { target: host }); await settle();
  invoke.mockImplementation((cmd) => cmd === 'save_note' ? new Promise(r => { resolve = r; }) : Promise.resolve(cmd === 'note_connections' ? { outgoing: [], backlinks: [] } : []));
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML = 'last edit'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  await vi.advanceTimersByTimeAsync(1500);
  await deleteNoteOptimistic(a, 'a'); await settle();
  resolve({ id: 'a-id', uuid: 'same', local_version: 3 }); await settle();
  expect(get(stores.notes)).toEqual([b]); expect(get(stores.selectedNote)).toBeNull();
});
