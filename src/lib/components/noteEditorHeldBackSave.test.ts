// @vitest-environment jsdom
//
// A remote change that replaces the OPEN note's row while the cursor is in the
// editor is held back (shouldRenderExternalBody) and the "Edited on another
// device" banner goes up. Its tooltip promised that saving would create a
// conflict copy. It did not: the next save took `local_version` from the
// REFRESHED row, so SQLite's compare-and-swap accepted it and the save landed on
// the primary — overwriting the other device's edit remotely, on every backend.
//
// Mounted, not a pure-function test (gotcha #28): the defect lives in which
// value the save reads at which reactive turn, and only the component shows
// that. The `invoke` mock below models `apply_local_edit_versioned`'s CAS
// exactly — refuse unless `expectedLocalVersion` matches the row — so "refused"
// here means what SQLite would decide, not what the editor intended.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';
const UUID = 'uuid-primary';
const COPY_UUID = 'uuid-conflict-copy';

const wrap = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
// wrapBody adds Apple's body style; what matters is the content.
const inner = (html: string | undefined) => html?.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? html;
const BASE = '<div>shared line one that is long enough</div>';
const MINE = `${BASE}<div>typed on this device</div>`;
const MINE_MORE = `${MINE}<div>and a little more</div>`;
const THEIRS = `${BASE}<div>written on the other device</div>`;

interface Row { uuid: string; title: string; body_html: string; label: string; local_version: number; id: string }
let row: Row;
let copies: Row[];
// When set, the next save_note waits on it — an autosave still in flight:
// `gate` parks it before SQLite applies it, `replyGate` after (the row is
// written, the reply has not reached the editor yet).
let gate: Promise<void> | null;
let replyGate: Promise<void> | null;

const asNote = (r: Row): Note => ({
  uuid: r.uuid, id: r.id, account_id: ACCOUNT_ID, title: r.title, body_html: r.body_html,
  date: '2026-09-26T00:00:00Z', label: r.label, local_version: r.local_version,
} as Note);

interface SaveArgs { existingUuid: string | null; expectedLocalVersion: number | null; title: string; bodyHtml: string; label: string }
const saves = () => invoke.mock.calls.filter(([c]) => c === 'save_note').map(([, a]) => a as SaveArgs);
const primarySaves = () => saves().filter((a) => a.existingUuid === UUID);

function bannerIsUp(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll('span')).some((s) =>
    (s.textContent ?? '').includes('Edited on another device'),
  );
}

describe('a save while a remote change is held back', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let editor: HTMLElement;

  function typeInto(inner: string) {
    editor.innerHTML = inner;
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
  }

  async function autosave() {
    await vi.advanceTimersByTimeAsync(1600);
    await tick();
    await tick();
  }

  // What App.svelte's refresh does when the row under the open note changes.
  function deliverRefresh() {
    const fresh = asNote(row);
    notes.update((ns) => ns.map((n) => (n.uuid === fresh.uuid ? fresh : n)));
    selectedNote.set(fresh);
    flushSync();
  }

  function focusEditor() {
    editor.tabIndex = 0; // jsdom does not treat contenteditable as focusable
    editor.focus();
    expect(document.activeElement).toBe(editor);
  }

  beforeEach(async () => {
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    vi.useFakeTimers();
    row = { uuid: UUID, id: 'msg-1', title: 'Shared', body_html: wrap(BASE), label: 'Notes', local_version: 1 };
    copies = [];
    gate = null;
    replyGate = null;
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'list_cached_notes_in_folder') {
        return Promise.resolve([row, ...copies].filter((r) => r.label === args.path).map(asNote));
      }
      if (cmd === 'save_note') {
        const a = args as unknown as SaveArgs;
        if (gate) { const g = gate; gate = null; await g; }
        if (!a.existingUuid) {
          const copy: Row = { uuid: COPY_UUID, id: '', title: a.title, body_html: a.bodyHtml, label: a.label, local_version: 1 };
          copy.uuid = `${COPY_UUID}${copies.length ? `-${copies.length}` : ''}`;
          copies.push(copy);
          return Promise.resolve({ id: '', uuid: copy.uuid, local_version: 1 });
        }
        // apply_local_edit_versioned: WHERE local_version = expected.
        if (a.expectedLocalVersion !== null && a.expectedLocalVersion !== row.local_version) {
          return Promise.reject(
            'This note changed elsewhere while you were editing (another device or an MCP agent). ' +
              'Reload the note and re-apply your changes before saving again.',
          );
        }
        row = { ...row, title: a.title, body_html: a.bodyHtml, local_version: row.local_version + 1 };
        const reply = { id: row.id, uuid: row.uuid, local_version: row.local_version };
        if (replyGate) { const g = replyGate; replyGate = null; await g; }
        return reply;
      }
      return Promise.resolve([]);
    });
    notes.set([asNote(row)]);
    selectedNote.set(asNote(row));
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await tick();
    await tick();
    editor = host.querySelector('.editor-body') as HTMLElement;

    // This device edits and autosaves: the row is now Dirty at v2.
    focusEditor();
    typeInto(MINE);
    await autosave();
    expect(row.local_version).toBe(2);
    expect(inner(row.body_html)).toBe(MINE);
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  function expectKeptBoth() {
    // The primary still holds the other device's text…
    expect(inner(row.body_html)).toBe(THEIRS);
    // …and no save addressed to it carried the version it was refreshed to.
    for (const s of primarySaves().slice(1)) expect(s.expectedLocalVersion).toBe(2);
    // This device's text survives, whole, as a separate note.
    expect(copies).toHaveLength(1);
    expect(inner(copies[0].body_html)).toBe(MINE_MORE);
    expect(copies[0].title).toMatch(/^Shared \(conflict copy /);
    expect(copies[0].label).toBe('Notes');
  }

  it('keep-both reconcile (version bumped): the save is refused and the remote text survives', async () => {
    // reconcile_one_db's keep-both branch: primary := remote, local_version bumped.
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
    expect(bannerIsUp(host)).toBe(true);

    typeInto(MINE_MORE);
    await autosave();

    expectKeptBoth();
  });

  it('clean pull (version NOT bumped): the editor refuses to write over what it held back', async () => {
    // The previous save was pushed, then another device's edit was pulled
    // into a CLEAN row: upsert_from_remote does not touch local_version, so
    // SQLite's CAS cannot see this one — only the editor knows.
    row = { ...row, body_html: wrap(THEIRS) };
    deliverRefresh();
    expect(bannerIsUp(host)).toBe(true);

    typeInto(MINE_MORE);
    await autosave();

    expectKeptBoth();
  });

  it('the editor follows its text into the copy, and the primary shows the remote version', async () => {
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
    typeInto(MINE_MORE);
    await autosave();

    expect(get(selectedNote)?.uuid).toBe(COPY_UUID);
    expect(editor.innerHTML).toBe(MINE_MORE); // not re-rendered: caret and undo survive
    expect(bannerIsUp(host)).toBe(false);
    const primary = get(notes).find((n) => n.uuid === UUID);
    expect(inner(primary?.body_html)).toBe(THEIRS);
    expect(get(notes).some((n) => n.uuid === COPY_UUID)).toBe(true);

    // Further typing saves to the copy, never to the primary.
    const before = primarySaves().length;
    typeInto(`${MINE_MORE}<div>still going</div>`);
    await autosave();
    expect(primarySaves()).toHaveLength(before);
    expect(inner(row.body_html)).toBe(THEIRS);
  });

  it('switching away with the change still held back does not flush over it', async () => {
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
    typeInto(MINE_MORE);

    // Switch notes before autosave fires: flushPendingEdit runs for the old note.
    const other = { ...asNote(row), uuid: 'uuid-other', body_html: wrap('<div>other</div>'), title: 'Other' };
    notes.update((ns) => [...ns, other]);
    selectedNote.set(other);
    flushSync();
    await autosave();

    expectKeptBoth();
  });

  it('a refusal SQLite detects on its own (store never refreshed) also keeps both', async () => {
    // Another writer bumped the row, and nothing has told the editor yet.
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };

    typeInto(MINE_MORE);
    await autosave();

    expect(primarySaves().at(-1)?.expectedLocalVersion).toBe(2);
    expectKeptBoth();
    expect(inner(get(notes).find((n) => n.uuid === UUID)?.body_html)).toBe(THEIRS);
  });

  it('a refresh that lands on a CLOSED note between two queued saves cannot slip the second one through', async () => {
    // No banner is ever raised here — the note is no longer open when its
    // change arrives — so only the version the text derives from can catch it.
    let release!: () => void;
    gate = new Promise((r) => { release = r; });
    typeInto(MINE);
    typeInto(`${BASE}<div>typed on this device, again</div>`);
    await vi.advanceTimersByTimeAsync(1600); // autosave #1: in flight, parked on the gate

    typeInto(MINE_MORE);
    const other = { ...asNote(row), uuid: 'uuid-other', body_html: wrap('<div>other</div>'), title: 'Other' };
    notes.update((ns) => [...ns, other]);
    selectedNote.set(other); // flush #2 queues behind #1
    flushSync();

    // Keep-both reconcile lands, and the list refresh carries it — into
    // `notes` only, because the note is not open.
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    notes.update((ns) => ns.map((n) => (n.uuid === UUID ? asNote(row) : n)));

    release();
    await autosave();

    expect(inner(row.body_html)).toBe(THEIRS);
    expect(inner(copies.at(-1)?.body_html)).toBe(MINE_MORE);
  });

  it('our own push coming back RE-ENCODED (iCloud) is not a conflict', async () => {
    // iCloud re-derives a pulled body through Apple's document model, so our
    // own text returns in different bytes. The byte-level banner cannot tell;
    // a conflict copy here would be minted on every push while typing.
    row = { ...row, body_html: wrap(MINE.replace(/<div>/g, '<div dir="auto">')) };
    deliverRefresh();

    typeInto(MINE_MORE);
    await autosave();

    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(MINE_MORE);
  });

  it('a refresh carrying the body of a save still in flight is not a conflict', async () => {
    let release!: () => void;
    replyGate = new Promise((r) => { release = r; });
    typeInto(`${MINE}<div>in flight</div>`);
    await vi.advanceTimersByTimeAsync(1600); // parked: SQLite has it, the editor has no reply yet
    expect(inner(row.body_html)).toBe(`${MINE}<div>in flight</div>`);
    typeInto(MINE_MORE); // keeps typing
    deliverRefresh(); // a list refresh reads the row mid-save

    release();
    await autosave();
    await autosave();

    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(MINE_MORE);
  });

  // The test above pins the held-back mark; the banner is the other half of
  // the same decision and was never released. It latched on this device's own
  // save and stayed up — "Edited on another device" with nobody else editing,
  // reported 2026-09-30 on a Gmail note after five earlier fixes to the banner.
  it('a refresh carrying the body of a save still in flight does not leave the banner up', async () => {
    let release!: () => void;
    replyGate = new Promise((r) => { release = r; });
    typeInto(`${MINE}<div>in flight</div>`);
    await vi.advanceTimersByTimeAsync(1600); // parked: SQLite has it, the editor has no reply yet
    typeInto(MINE_MORE); // keeps typing
    deliverRefresh(); // a list refresh reads the row mid-save

    release();
    await tick();
    await tick();
    flushSync();

    expect(bannerIsUp(host)).toBe(false);
    // Both halves reach jodd.log, so the next report is answered from the log.
    const logged = invoke.mock.calls
      .filter(([c]) => c === 'editor_diagnostic')
      .map(([, a]) => (a as { line: string }).line);
    expect(logged.map((l) => l.split(' ')[1])).toEqual(['RAISED', 'RELEASED-by-save-reply']);
    expect(logged[0]).toContain('saveInFlight=true');
    expect(logged.join('\n')).not.toContain('in flight'); // lengths and hashes, never text
  });

  it('the reply of a save does not take down a banner raised by a DIFFERENT body', async () => {
    let release!: () => void;
    replyGate = new Promise((r) => { release = r; });
    typeInto(`${MINE}<div>in flight</div>`);
    await vi.advanceTimersByTimeAsync(1600);
    typeInto(MINE_MORE);
    row = { ...row, body_html: wrap(THEIRS) }; // another writer, not our save
    deliverRefresh();
    expect(bannerIsUp(host)).toBe(true);

    release();
    await tick();
    await tick();
    flushSync();

    expect(bannerIsUp(host)).toBe(true);
  });

  it('a version bump that is NOT a content change (e.g. a move) does not manufacture a conflict', async () => {
    // move_notes_batch bumps local_version and changes nothing the editor shows.
    row = { ...row, local_version: 3 };
    deliverRefresh();
    expect(bannerIsUp(host)).toBe(false);

    typeInto(MINE_MORE);
    await autosave();

    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(MINE_MORE);
    expect(primarySaves().at(-1)?.expectedLocalVersion).toBe(3);
  });
});
