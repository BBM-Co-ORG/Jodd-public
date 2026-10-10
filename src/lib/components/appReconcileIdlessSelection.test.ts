// @vitest-environment jsdom
//
// App.svelte's `reconcileSelection` skipped any open note without an `id` as
// "new unsaved — preserve user's in-memory work". But `id` is the remote's: a
// note in SQLite that the worker has not pushed yet has none either — an
// agent's create_note/remember in `__Agent__` above all — and nothing updates
// the open selection's `id` when the push lands. So such a note never received
// a listing's newer body until it was re-selected. Under no trust chip (the
// agent workspace never shows one) the review re-read does not run either, so
// the old body stayed on screen. PR #141 fixed the same gate in
// `refreshSelection`; this is the listing's route.
//
// The note that really lives only in memory is a brand-new editor note with a
// `tmp:` uuid (`tmp:` never reaches Rust). That one must still be left alone:
// the listing holds an older copy of it, and taking that copy would throw away
// what the user typed.
//
// Mounted (gotcha #28): the defect is what the editor renders, and the route
// is the real one — remote-changed -> requestRefresh -> loadNotes ->
// reconcileSelection -> NoteEditor's render decision.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get } from 'svelte/store';
import type { Note } from '../types';

const ACCOUNT = { id: 'gmail:test@example.com', email: 'test@example.com', backend_kind: 'gmail' };
const handlers = new Map<string, (e: { payload: unknown }) => void>();
// What every listing returns — the SQLite rows behind `list_notes`.
let listing: Note[] = [];

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return [ACCOUNT];
    case 'list_unreviewed_trust': return [];
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
  listen: async (name: string, fn: (e: { payload: unknown }) => void) => { handlers.set(name, fn); return () => handlers.delete(name); },
  emit: async () => {},
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));

import App from '../../App.svelte';
import { selectedNote, notes } from '../stores/notes';

const LABEL = 'Notes/__Agent__';
const wrap = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const V1 = wrap('<div>Agent note</div><div>first line</div>');
const V2 = wrap('<div>Agent note</div><div>first line</div><div>REMEMBERED LINE</div>');
// In SQLite, not pushed yet: a real uuid, no remote `id`.
const agentNote = (body: string, local_version: number): Note => ({
  uuid: 'agent-uuid-1', id: '', account_id: ACCOUNT.id, title: 'Agent note', body_html: body,
  date: '2026-10-08T00:00:00Z', label: LABEL, local_version,
} as Note);

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

function bannerIsUp(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll('span')).some((s) =>
    (s.textContent ?? '').includes('Edited on another device'),
  );
}

describe('App: a listing reaches an open note that has no remote id', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    handlers.clear();
    listing = [];
    selectedNote.set(null);
    notes.set([]);
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  const editor = () => host!.querySelector('.editor-body') as HTMLElement;

  // Mount App, let startup settle, then open `open` with `listing` behind it.
  async function openWith(open: Note) {
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
    expect(handlers.has('remote-changed'), 'App registered the remote-changed listener').toBe(true);
    await settle();
    notes.set([...listing]);
    selectedNote.set(open);
    await settle();
    expect(editor(), 'the editor is mounted for the open note').not.toBeNull();
  }

  // The worker's nudge, past requestRefresh's 2 s throttle — the real route
  // into loadNotes -> reconcileSelection.
  async function remoteChanged() {
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);
    invoke.mockClear();
    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_notes'); i++) await settle(1);
    expect(invoke).toHaveBeenCalledWith('list_notes', { accountId: ACCOUNT.id });
    await settle();
  }

  it('renders a newer body for an id-less, chip-less note', async () => {
    listing = [agentNote(V1, 1)];
    await openWith(agentNote(V1, 1));
    expect(editor().innerHTML).not.toContain('REMEMBERED LINE');

    listing = [agentNote(V2, 2)];
    await remoteChanged();

    expect(get(selectedNote)?.local_version).toBe(2);
    expect(editor().innerHTML).toContain('REMEMBERED LINE');
  });

  // The protection the old `id` gate claimed: unsaved typing is not replaced.
  // It belongs to NoteEditor's render decision (hasPendingEdit), which holds
  // the change back and says so — not to whether the remote has an id yet.
  it('holds the newer body back behind the banner while there is unsaved typing', async () => {
    listing = [agentNote(V1, 1)];
    await openWith(agentNote(V1, 1));
    const typed = '<div>Agent note</div><div>first line</div><div>typed by the user</div>';
    editor().innerHTML = typed;
    editor().dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    listing = [agentNote(V2, 2)];
    await remoteChanged();

    expect(editor().innerHTML).toContain('typed by the user');
    expect(editor().innerHTML).not.toContain('REMEMBERED LINE');
    expect(bannerIsUp(host!)).toBe(true);
  });

  // The race the id gate used to hide for an unpushed note: a listing read
  // before the user's save commits, landing after it. Its row is the note one
  // local write ago. `upsert_from_remote` never moves `local_version` on an
  // existing row (it counts local writes; a remote edit arrives at the SAME
  // version), so a LOWER version than the selection's can only be stale.
  it('ignores a listing row older than the selection', async () => {
    const SAVED = wrap('<div>Agent note</div><div>first line</div><div>just saved</div>');
    listing = [agentNote(SAVED, 2)];
    await openWith(agentNote(SAVED, 2));
    expect(editor().innerHTML).toContain('just saved');

    listing = [agentNote(V1, 1)];
    await remoteChanged();

    expect(get(selectedNote)?.local_version).toBe(2);
    expect(editor().innerHTML).toContain('just saved');
  });

  // A `tmp:` note lives only in memory. The listing merge keeps the store's
  // copy of it (mergeAccountListings' survivors), which is older than what the
  // user typed into the selection — taking it would discard their work.
  it('leaves a tmp: note alone', async () => {
    const TMP = 'tmp:1234';
    const stale: Note = { ...agentNote(wrap('<div>New Note</div>'), 0), uuid: TMP, title: 'New Note', label: 'Notes' };
    const typing: Note = { ...stale, title: 'Draft', body_html: wrap('<div>Draft</div><div>typed</div>') };
    listing = [];
    await openWith(typing);
    notes.set([stale]);
    await settle();

    await remoteChanged();

    expect(get(selectedNote)?.uuid).toBe(TMP);
    expect(get(selectedNote)?.body_html).toBe(typing.body_html);
    expect(get(selectedNote)?.title).toBe('Draft');
  });
});
