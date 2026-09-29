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

it('discloses folder context without disturbing pending typing, then closes with an explicit save', async () => {
  app = mount(NoteEditor, { target: host }); await settle();
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.focus(); editor.innerHTML = 'ยังไม่บันทึก'; editor.dispatchEvent(new Event('input', { bubbles: true })); flushSync();
  let resolve!: (v: unknown) => void;
  invoke.mockImplementation(cmd => cmd === 'save_note' ? new Promise(r => { resolve = r; }) : Promise.resolve(cmd === 'note_connections' ? {outgoing:[],backlinks:[]} : []));
  stores.selectedFolder.set('Notes/Empty'); await settle();
  expect(editor.textContent).toBe('ยังไม่บันทึก');
  expect(document.activeElement).toBe(editor);
  expect(host.querySelector('.context-notice')?.textContent).toContain('Notes/Empty');
  (host.querySelector('.context-notice button') as HTMLButtonElement).click(); await settle();
  expect(host.querySelector('.empty-editor')?.textContent).toContain('Notes/Empty');
  expect(document.activeElement).toBe(host.querySelector('.empty-editor'));
  expect(invoke.mock.calls.filter(([c]) => c === 'save_note')).toHaveLength(1);
  expect(invoke.mock.calls.find(([c]) => c === 'save_note')?.[1]).toMatchObject({accountId:'a',existingUuid:'same',label:'Notes'});
  stores.currentAccount.set('b'); stores.selectedNote.set(b); await settle();
  resolve({id:'new-a',uuid:'assigned',local_version:3}); await settle();
  expect(get(stores.selectedNote)).toEqual(b);
  expect(host.querySelector('.editor-body')?.textContent).toBe('Beta body');
  expect(get(stores.notes).find(n => n.account_id === 'a')?.body_html).toContain('ยังไม่บันทึก');
});
it('collapses sources, exposes readable Thai paths, and clears old sources before a new account reply', async () => {
  const urls = Array.from({length:8}, (_,i) => `https://example.test/${encodeURIComponent('ประชุม')}/${i+1}`);
  invoke.mockImplementation((cmd) => Promise.resolve(cmd === 'note_citations' ? urls : cmd === 'note_connections' ? {outgoing:[],backlinks:[]} : []));
  app = mount(NoteEditor, {target:host}); await settle();
  expect(host.querySelectorAll('.source-list a')).toHaveLength(3);
  expect(host.querySelector('.source-list a')?.textContent).toContain('ประชุม');
  const toggle = host.querySelector('.source-list button') as HTMLButtonElement;
  expect(toggle.getAttribute('aria-expanded')).toBe('false');
  toggle.click(); await settle(); expect(host.querySelectorAll('.source-list a')).toHaveLength(8);
  const last = host.querySelectorAll('.source-list a')[7] as HTMLAnchorElement;last.focus();expect(document.activeElement).toBe(last);
  toggle.focus();toggle.click();await settle();expect(document.activeElement).toBe(toggle);
  let reply!: (v:unknown) => void;
  invoke.mockImplementation(cmd => cmd === 'note_citations' ? new Promise(r => {reply=r;}) : Promise.resolve(cmd === 'note_connections' ? {outgoing:[],backlinks:[]} : []));
  stores.selectedNote.set(b);await settle();
  expect(host.querySelectorAll('.source-list a')).toHaveLength(0);
  expect(host.textContent).toContain('Loading sources');
  reply(['https://example.test/b']);await settle();
  expect(host.querySelectorAll('.source-list a')).toHaveLength(1);
});
it('ignores late citation replies across same-UUID account switches and shows failures honestly', async () => {
  let reply!: (v:unknown) => void;
  invoke.mockImplementation(cmd => cmd === 'note_citations' ? new Promise(r => {reply=r;}) : Promise.resolve(cmd === 'note_connections' ? {outgoing:[],backlinks:[]} : []));
  app=mount(NoteEditor,{target:host});await settle();
  invoke.mockImplementation(cmd => cmd === 'note_citations' ? Promise.reject(new Error('unavailable')) : Promise.resolve(cmd === 'note_connections' ? {outgoing:[],backlinks:[]} : []));
  stores.selectedNote.set(b);await settle();reply(['https://example.test/old']);await settle();
  expect(host.querySelectorAll('.source-list a')).toHaveLength(0);
  expect(host.textContent).toContain('Sources unavailable');
});
it('preserves the newer closed draft if its queued save fails after an older save rekeys', async () => {
  const saves: {resolve:(v:unknown)=>void;reject:(e:Error)=>void}[]=[];
  app=mount(NoteEditor,{target:host});await settle();
  invoke.mockImplementation(cmd=>cmd==='save_note'?new Promise((resolve,reject)=>saves.push({resolve,reject})):Promise.resolve(cmd==='note_connections'?{outgoing:[],backlinks:[]}:[]));
  const editor=host.querySelector('.editor-body') as HTMLElement;
  editor.innerHTML='first snapshot';editor.dispatchEvent(new Event('input',{bubbles:true}));flushSync();await vi.advanceTimersByTimeAsync(1500);
  editor.innerHTML='newer Thai draft ยังไม่บันทึก';editor.dispatchEvent(new Event('input',{bubbles:true}));flushSync();
  stores.selectedFolder.set('Notes/Empty');await settle();
  (host.querySelector('.context-notice button') as HTMLButtonElement).click();await settle();
  expect(saves).toHaveLength(1); // second local write must remain serialized
  saves[0].resolve({id:'assigned-id',uuid:'assigned',local_version:3});await settle();
  expect(saves).toHaveLength(2);
  const calls=invoke.mock.calls.filter(([cmd])=>cmd==='save_note');
  expect(calls[1][1]).toMatchObject({accountId:'a',existingUuid:'assigned',expectedLocalVersion:3});
  saves[1].reject(new Error('disk busy'));await settle();
  stores.selectedNote.set(get(stores.notes).find(n=>n.account_id==='a')!);await settle();
  expect(host.querySelector('.editor-body')?.textContent).toBe('newer Thai draft ยังไม่บันทึก');
  expect(host.querySelector('.save-status')?.textContent).toContain('Unsaved');
});
