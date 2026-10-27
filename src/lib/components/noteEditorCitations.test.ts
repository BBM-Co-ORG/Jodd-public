// @vitest-environment jsdom
//
// Task 5.4 (spec 2026-10-20 §6.5), at the NoteEditor level: what the
// connections line says about Sources across a failure, a Retry, a save and
// a note switch. ConnectionsDisclosure's own wording is connectionsDisclosure
// .test.ts; this is the state NoteEditor feeds it (citationsSettled).
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import * as stores from '../stores/notes';
import { persistenceByNote, pendingLocalEdits, savingNoteKeys } from '../notePersistence';
import { clearNoteAliases } from '../noteIdentity';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const a: Note = { account_id: 'a', uuid: 'note-a', id: 'a-id', title: 'Alpha', body_html: '<body>Alpha body</body>', label: 'Notes', date: '2026-09-19', local_version: 2 };
const b: Note = { ...a, uuid: 'note-b', id: 'b-id', title: 'Beta', body_html: '<body>Beta body</body>' };

/** What each note_citations call does, in order; the last one repeats. */
let citationReplies: Array<() => Promise<unknown>> = [];
let citationCalls = 0;
function route(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  if (cmd === 'note_citations') {
    const reply = citationReplies[Math.min(citationCalls, citationReplies.length - 1)];
    citationCalls++;
    return reply();
  }
  if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
  if (cmd === 'save_note') return Promise.resolve({ id: 'a-id-2', uuid: args.existingUuid, local_version: 3 });
  if (cmd === 'note_persistence') {
    return Promise.resolve({ account_id: args.accountId, uuid: args.uuid, local_version: 3, sync_state: 'dirty', push_blocked_reason: null });
  }
  return Promise.resolve([]);
}
const never = () => new Promise(() => {});
const none = () => Promise.resolve([]);
const fail = () => Promise.reject(new Error('unavailable'));

let host: HTMLElement;
let app: ReturnType<typeof mount> | undefined;

beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
  clearNoteAliases();
  persistenceByNote.set({});
  pendingLocalEdits.set({});
  savingNoteKeys.set(new Set());
  Object.defineProperty(HTMLElement.prototype, 'innerText', { configurable: true, get() { return this.textContent ?? ''; } });
  citationReplies = [none];
  citationCalls = 0;
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => route(cmd, args ?? {}));
  stores.accounts.set([]);
  stores.currentAccount.set('a');
  stores.notes.set([a, b]);
  stores.selectedNote.set(a);
  stores.selectedFolder.set('Notes');
  stores.isSaving.set(false);
  host = document.createElement('div');
  document.body.append(host);
});
afterEach(async () => {
  if (app) await unmount(app);
  app = undefined;
  host.remove();
  stores.selectedNote.set(null);
  vi.useRealTimers();
});

async function settle() {
  await tick();
  await vi.advanceTimersByTimeAsync(0);
  flushSync();
  await tick();
}
/** The connections line, or null when it is hidden (nothing to say). */
const line = () => host.querySelector('.connections-disclosure .line')?.textContent?.replace(/\s+/g, ' ').trim() ?? null;

async function typeAndSave(text: string) {
  const editor = host.querySelector('.editor-body') as HTMLElement;
  editor.focus();
  editor.innerHTML = text;
  editor.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
  await vi.advanceTimersByTimeAsync(1500); // the autosave debounce
  await settle();
  await settle();
}

it('a rejected note_citations shows "Sources unavailable · Retry"', async () => {
  citationReplies = [fail];
  app = mount(NoteEditor, { target: host });
  await settle();
  expect(line()).toBe('Sources unavailable · Retry');
});

it('Retry after a failure says "Sources…" while it reads, not nothing', async () => {
  // Read once (no sources), then a save's re-read fails, then Retry hangs.
  citationReplies = [none, fail, never];
  app = mount(NoteEditor, { target: host });
  await settle();
  expect(line()).toBeNull();
  await typeAndSave('Alpha body, edited');
  expect(citationCalls).toBe(2);
  expect(line()).toBe('Sources unavailable · Retry');
  (host.querySelector('.connections-disclosure button.retry') as HTMLButtonElement).click();
  await settle();
  expect(citationCalls).toBe(3);
  expect(line()).toBe('Sources…');
});

it('no "Sources…" flash while a zero-source note re-reads after a save', async () => {
  citationReplies = [none, never];
  app = mount(NoteEditor, { target: host });
  await settle();
  expect(line()).toBeNull();
  await typeAndSave('Alpha body, edited');
  expect(citationCalls).toBe(2); // the re-read is in flight
  expect(line()).toBeNull();
});

it('switching notes forgets that the last one was read: the new one says "Sources…"', async () => {
  citationReplies = [none, never];
  app = mount(NoteEditor, { target: host });
  await settle();
  expect(line()).toBeNull();
  stores.selectedNote.set(b);
  await settle();
  expect(citationCalls).toBe(2);
  expect(line()).toBe('Sources…');
});
