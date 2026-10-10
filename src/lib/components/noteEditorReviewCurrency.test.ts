// @vitest-environment jsdom
//
// Mark reviewed may only certify content that is on screen. Found 2026-10-08 in
// the provenance live pass: an agent appended to the open note, Mark reviewed
// was correctly refused as stale, and the chip at once showed the agent's edit
// (markReviewed puts the fresh Trust back) — while the editor still showed the
// old body until the next full listing, ~48 s later. A second click in that
// window passed verify_note's stale check (it was given the event the chip now
// showed) and certified text the user never saw.
//
// Mounted (gotcha #28): the defect is which body is rendered at the moment the
// button can be clicked, and only the component shows that. `get_cached_note`
// stands for the SQLite row; `verify_note` models Db::verify_note's stale rule
// for the event the chip showed being newer than the version the editor sent.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes, unreviewedTrust, unreviewedKey, type Trust } from '../stores/notes';
import { persistenceByNote, pendingLocalEdits, savingNoteKeys } from '../notePersistence';
import { clearNoteAliases } from '../noteIdentity';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';
const UUID = 'uuid-b';
const wrap = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const V1 = wrap('<div>Note B</div><div>first line</div>');
const V2 = wrap('<div>Note B</div><div>first line</div><div>AGENT LINE</div>');
const noteAt = (body: string, local_version: number): Note => ({
  uuid: UUID, id: 'msg-b', account_id: ACCOUNT_ID, title: 'Note B', body_html: body,
  date: '2026-10-08T00:00:00Z', label: 'Notes', local_version,
} as Note);

// T1: the chip as first shown ("changed elsewhere"). T2: the agent's append.
const T1: Trust = { tier: 'unreviewed', by: null, at: '2026-10-08T01:00:00Z', event_id: 7, unreviewed_by: null, unreviewed_at: '2026-10-08T01:00:00Z', created_by: null, unrecorded_change: false, note_local_version: 3 };
const T2: Trust = { ...T1, by: 'claude-code/2.1.0', event_id: 9, unreviewed_by: 'claude-code/2.1.0', unreviewed_at: '2026-10-08T02:00:00Z', note_local_version: 4 };
const key = unreviewedKey(ACCOUNT_ID, UUID);
const put = (t: Trust) => unreviewedTrust.set({ [key]: { account: ACCOUNT_ID, trust: t } });

describe('Mark reviewed certifies only what the editor shows', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  // The SQLite row, and a gate that parks get_cached_note until released.
  let dbNote: Note;
  let readGate: Promise<void>;
  let releaseRead: () => void;
  let dbTrust: Trust;

  const editor = () => host.querySelector('.editor-body') as HTMLElement;
  const chip = () => host.querySelector('[data-testid="trust-chip"]');
  const button = () => chip()?.querySelector('button') as HTMLButtonElement | null;
  const verifies = () => invoke.mock.calls.filter(([c]) => c === 'verify_note').map(([, a]) => a);

  async function settle() {
    for (let i = 0; i < 8; i++) {
      await tick();
      await Promise.resolve();
    }
    flushSync();
  }

  function parkReads() {
    readGate = new Promise((r) => (releaseRead = r));
  }

  beforeEach(async () => {
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    dbNote = noteAt(V1, 3);
    dbTrust = T1;
    readGate = Promise.resolve();
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
      if (cmd === 'get_cached_note') {
        await readGate;
        return dbNote;
      }
      if (cmd === 'verify_note') {
        // Db::verify_note: the shown event is unseen if a newer one exists, or
        // if it is newer than the version the caller says it saw.
        const seen = args.seenEventId as number;
        const stale = dbTrust.event_id! > seen || (dbTrust.event_id === seen && dbTrust.note_local_version > (args.seenLocalVersion as number));
        return stale ? { kind: 'stale', trust: dbTrust } : { kind: 'verified', trust: { ...dbTrust, tier: 'human_reviewed' } };
      }
      return [];
    });
    unreviewedTrust.set({});
    notes.set([noteAt(V1, 3)]);
    selectedNote.set(noteAt(V1, 3));
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    put(T1);
    await settle();
    expect(editor().textContent).toContain('first line');
    expect(button()?.disabled).toBe(false);
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    unreviewedTrust.set({});
    vi.clearAllMocks();
  });

  // The agent's append lands in SQLite; the store still holds v1.
  function agentAppends() {
    dbNote = noteAt(V2, 4);
    dbTrust = T2;
  }

  it('a stale review renders the newer body before Mark reviewed can verify', async () => {
    agentAppends();
    parkReads();
    button()!.click();
    await settle();
    // Refused, and the chip names the agent at once ...
    expect(verifies()).toHaveLength(1);
    expect(chip()?.textContent).toContain('claude-code');
    // ... but the agent's line is not on screen yet, so nothing can certify it.
    expect(editor().textContent).not.toContain('AGENT LINE');
    expect(button()!.disabled).toBe(true);
    button()!.click();
    await settle();
    expect(verifies()).toHaveLength(1);

    releaseRead();
    await settle();
    expect(editor().textContent).toContain('AGENT LINE');
    expect(button()!.disabled).toBe(false);
    button()!.click();
    await settle();
    expect(verifies()).toHaveLength(2);
    expect(verifies()[1]).toEqual({ accountId: ACCOUNT_ID, uuid: UUID, seenEventId: 9, seenLocalVersion: 4 });
    expect(chip()).toBeNull();
  });

  it('a chip advanced by a refresh, not by a click, also re-reads the note', async () => {
    agentAppends();
    parkReads();
    put(T2); // what refreshUnreviewed does after a listing that predates the append
    await settle();
    expect(editor().textContent).not.toContain('AGENT LINE');
    expect(button()!.disabled).toBe(true);
    releaseRead();
    await settle();
    expect(editor().textContent).toContain('AGENT LINE');
    expect(button()!.disabled).toBe(false);
  });

  // PR #139 review: a listing fetched before the append lands AFTER the
  // re-read and puts the old body back. The listing no longer stamps an old
  // body with the row's new version (overlay_cached_row), but the editor must
  // not rely on that: a body change under a shown chip re-reads SQLite before
  // Mark reviewed can verify, so the old body never gets certified.
  it('a late listing that puts the old body back re-reads before Mark reviewed can verify', async () => {
    agentAppends();
    put(T2);
    await settle();
    expect(editor().textContent).toContain('AGENT LINE');
    expect(button()!.disabled).toBe(false);

    parkReads();
    selectedNote.set(noteAt(V1, 4)); // what reconcileSelection does with that listing
    await settle();
    expect(button()!.disabled).toBe(true);
    button()!.click();
    await settle();
    expect(verifies()).toHaveLength(0);

    releaseRead();
    await settle();
    expect(editor().textContent).toContain('AGENT LINE');
    expect(button()!.disabled).toBe(false);
    button()!.click();
    await settle();
    expect(verifies()).toEqual([{ accountId: ACCOUNT_ID, uuid: UUID, seenEventId: 9, seenLocalVersion: 4 }]);
  });

  it('a failed read leaves an error that a later successful read clears', async () => {
    const base = invoke.getMockImplementation()!;
    let failed = false;
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'get_cached_note' && !failed) { failed = true; throw new Error('db locked'); }
      return base(cmd, args);
    });
    agentAppends();
    put(T2);
    await settle();
    expect(chip()?.textContent).toContain('Could not load the latest version.');
    expect(button()!.disabled).toBe(true);
    selectedNote.set(noteAt(V2, 4)); // the next listing brings the append; a fresh read follows
    await settle();
    expect(button()!.disabled).toBe(false);
    expect(chip()?.textContent).not.toContain('Could not load');
  });

  it('unsaved typing is not clobbered, and Mark reviewed stays off while the change is held back', async () => {
    const ed = editor();
    ed.tabIndex = 0; // jsdom does not treat contenteditable as focusable
    ed.focus();
    ed.innerHTML = '<div>Note B</div><div>first line</div><div>my own words</div>';
    ed.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    agentAppends();
    put(T2);
    await settle();
    expect(editor().textContent).toContain('my own words');
    expect(editor().textContent).not.toContain('AGENT LINE');
    expect(host.textContent).toContain('Edited on another device');
    expect(button()!.disabled).toBe(true);
    button()!.click();
    await settle();
    expect(verifies()).toHaveLength(0);
  });
});

// An agent's create_note/remember writes a row to SQLite with no remote id
// until the worker pushes it, and the listing hands the editor that row. The
// re-read must still apply to it: refreshSelection used to skip any selection
// without an id — meant for a brand-new note that lives only in memory — so
// the agent's later append was read, dropped, and Mark reviewed came back on
// the old body, refused as stale on every click until the push landed.
describe('Mark reviewed on an agent note that has not been pushed yet', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let dbNote: Note | null;
  let dbTrust: Trust;

  const unpushed = (body: string, local_version: number): Note => ({ ...noteAt(body, local_version), id: '' });
  const editor = () => host.querySelector('.editor-body') as HTMLElement;
  const chip = () => host.querySelector('[data-testid="trust-chip"]');
  const button = () => chip()?.querySelector('button') as HTMLButtonElement | null;
  const verifies = () => invoke.mock.calls.filter(([c]) => c === 'verify_note').map(([, a]) => a);

  async function settle() {
    for (let i = 0; i < 8; i++) {
      await tick();
      await Promise.resolve();
    }
    flushSync();
  }

  async function open(selection: Note) {
    // Same note key as the suite above, whose last test leaves a draft behind.
    clearNoteAliases(); persistenceByNote.set({}); pendingLocalEdits.set({}); savingNoteKeys.set(new Set());
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
      if (cmd === 'get_cached_note') return dbNote;
      if (cmd === 'verify_note') {
        const seen = args.seenEventId as number;
        const stale = dbTrust.event_id! > seen || (dbTrust.event_id === seen && dbTrust.note_local_version > (args.seenLocalVersion as number));
        return stale ? { kind: 'stale', trust: dbTrust } : { kind: 'verified', trust: { ...dbTrust, tier: 'human_reviewed' } };
      }
      return [];
    });
    unreviewedTrust.set({});
    notes.set([selection]);
    selectedNote.set(selection);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  }

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    unreviewedTrust.set({});
    vi.clearAllMocks();
  });

  it('the re-read renders the agent body and Mark reviewed verifies it', async () => {
    // The listing that opened the note predates the agent's append; SQLite has it.
    dbNote = unpushed(V2, 4);
    dbTrust = T2;
    await open(unpushed(V1, 3));
    put(T2);
    await settle();
    expect(editor().textContent).toContain('AGENT LINE');
    expect(button()!.disabled).toBe(false);
    button()!.click();
    await settle();
    expect(verifies()).toEqual([{ accountId: ACCOUNT_ID, uuid: UUID, seenEventId: 9, seenLocalVersion: 4 }]);
    expect(chip()).toBeNull();
  });

  it('a note that is only in memory is left as it is', async () => {
    // A brand-new note before its first save: no row, so nothing to apply.
    dbNote = null;
    dbTrust = T2;
    await open(unpushed(V1, 0));
    put(T2);
    await settle();
    expect(invoke.mock.calls.some(([c]) => c === 'get_cached_note')).toBe(true);
    expect(editor().textContent).toContain('first line');
    expect(editor().textContent).not.toContain('AGENT LINE');
  });
});
