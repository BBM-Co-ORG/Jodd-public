// @vitest-environment jsdom
//
// A save the editor must not land on its note (a held-back remote change, or
// SQLite's version refusal) used to become a conflict copy every time. When
// the row only ADDED lines the editor's text does not touch, it now merges
// instead (2026-11-05, note 80D6C8E1: a Share on a day-stale Tab S7, then the
// user removed the share marker while another device's links sat in the row).
//
// The merge itself is Rust (`merge::merge_onto_moved_row`, tested there); the
// mock below answers `merge_onto_row` the way Rust would for these bodies, and
// checks it was asked with the body the editor's text was built on. What this
// pins is the editor's half: the row's lines go INTO the editor's DOM (no
// re-render, so caret and undo stay), the save lands on the primary against
// the row's version, and no copy is made.
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

const wrap = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const inner = (html: string | undefined) => html?.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? html;
const BASE = '<div>shared line one that is long enough</div>';
const MINE = `${BASE}<div>typed on this device</div>`;
const MINE_MORE = `${MINE}<div>and a little more</div>`;
const TOP = '<div>a link typed on the other device</div>';

interface Row { uuid: string; title: string; body_html: string; label: string; local_version: number; id: string }
let row: Row;
let copies: Row[];
let mergeGate: Promise<void> | null;
interface MergeArgs { accountId: string; uuid: string; title: string; baseBody: string; bodyHtml: string }
let mergeAsks: MergeArgs[];

const asNote = (r: Row): Note => ({
  uuid: r.uuid, id: r.id, account_id: ACCOUNT_ID, title: r.title, body_html: r.body_html,
  date: '2026-11-05T00:00:00Z', label: r.label, local_version: r.local_version,
} as Note);

interface SaveArgs { existingUuid: string | null; expectedLocalVersion: number | null; title: string; bodyHtml: string }
const primarySaves = () =>
  invoke.mock.calls.filter(([c, a]) => c === 'save_note' && (a as SaveArgs).existingUuid === UUID).map(([, a]) => a as SaveArgs);

function bannerIsUp(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll('span')).some((s) => (s.textContent ?? '').includes('Edited on another device'));
}

// What Rust answers when the row is TOP + the base: TOP goes before the
// editor's first line. Anything else: no merge.
function rustMerge(a: MergeArgs) {
  if (inner(a.baseBody) !== MINE || inner(row.body_html) !== `${TOP}${MINE}`) return null;
  const lines = inner(a.bodyHtml)!.match(/<div>[\s\S]*?<\/div>/g) ?? [];
  return {
    bodyHtml: a.bodyHtml.replace(/(<body[^>]*>)/, `$1${TOP}`),
    localLines: lines.length,
    inserts: [[0, TOP]],
    localVersion: row.local_version,
    rowBody: row.body_html,
  };
}

describe("a refused or held-back save merges with the row's new lines", () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let editor: HTMLElement;

  function typeInto(html: string) {
    editor.innerHTML = html;
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
  }
  async function autosave() {
    await vi.advanceTimersByTimeAsync(1600);
    await tick();
    await tick();
  }
  function deliverRefresh() {
    const fresh = asNote(row);
    notes.update((ns) => ns.map((n) => (n.uuid === fresh.uuid ? fresh : n)));
    selectedNote.set(fresh);
    flushSync();
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
    mergeGate = null;
    mergeAsks = [];
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
      if (cmd === 'list_cached_notes_in_folder') return [row, ...copies].filter((r) => r.label === args.path).map(asNote);
      if (cmd === 'merge_onto_row') {
        const a = args as unknown as MergeArgs;
        mergeAsks.push(a);
        if (mergeGate) { const g = mergeGate; mergeGate = null; await g; }
        return rustMerge(a);
      }
      if (cmd === 'save_note') {
        const a = args as unknown as SaveArgs;
        if (!a.existingUuid) {
          const copy: Row = { uuid: `copy-${copies.length}`, id: '', title: a.title, body_html: a.bodyHtml, label: 'Notes', local_version: 1 };
          copies.push(copy);
          return { id: '', uuid: copy.uuid, local_version: 1 };
        }
        if (a.expectedLocalVersion !== null && a.expectedLocalVersion !== row.local_version) {
          throw 'This note changed elsewhere while you were editing (another device or an MCP agent). Reload the note and re-apply your changes before saving again.';
        }
        row = { ...row, title: a.title, body_html: a.bodyHtml, local_version: row.local_version + 1 };
        return { id: row.id, uuid: row.uuid, local_version: row.local_version };
      }
      return [];
    });
    notes.set([asNote(row)]);
    selectedNote.set(asNote(row));
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await tick();
    await tick();
    editor = host.querySelector('.editor-body') as HTMLElement;
    editor.tabIndex = 0;
    editor.focus();

    typeInto(MINE);
    await autosave();
    expect(row.local_version).toBe(2);
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  function expectMerged() {
    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(`${TOP}${MINE_MORE}`);
    expect(primarySaves().at(-1)?.expectedLocalVersion).toBe(3);
    expect(editor.innerHTML).toBe(`${TOP}${MINE_MORE}`);
    expect(bannerIsUp(host)).toBe(false);
    // Asked against the body the editor's text was built on: its last save.
    expect(inner(mergeAsks[0].baseBody)).toBe(MINE);
    expect(inner(mergeAsks[0].bodyHtml)).toBe(MINE_MORE);
  }

  it('a held-back change that only added lines merges into the editor instead of a copy', async () => {
    row = { ...row, body_html: wrap(`${TOP}${MINE}`), local_version: 3 };
    deliverRefresh();
    expect(bannerIsUp(host)).toBe(true);

    typeInto(MINE_MORE);
    const typed = editor.lastChild;
    await autosave();

    expectMerged();
    expect(editor.lastChild).toBe(typed); // inserted around, not re-rendered
  });

  it('a refusal SQLite detects on its own merges the same way', async () => {
    row = { ...row, body_html: wrap(`${TOP}${MINE}`), local_version: 3 };

    typeInto(MINE_MORE);
    await autosave();

    expectMerged();
  });

  it('typing while the merge is asked for does not save the merge over the new text', async () => {
    row = { ...row, body_html: wrap(`${TOP}${MINE}`), local_version: 3 };
    deliverRefresh();
    let release!: () => void;
    mergeGate = new Promise((r) => { release = r; });

    typeInto(MINE_MORE);
    await vi.advanceTimersByTimeAsync(1600); // the save is now waiting on the merge
    const LATER = `${MINE_MORE}<div>typed meanwhile</div>`;
    typeInto(LATER);
    release();
    await tick();
    await tick();

    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(`${TOP}${MINE}`); // nothing saved from the stale ask
    expect(editor.innerHTML).toBe(LATER);
  });

  it('no merge (the lines clash) still keeps both', async () => {
    row = { ...row, body_html: wrap(`${BASE}<div>rewritten on the other device</div>`), local_version: 3 };
    deliverRefresh();

    typeInto(MINE_MORE);
    await autosave();

    expect(mergeAsks).toHaveLength(1);
    expect(copies).toHaveLength(1);
    expect(inner(copies[0].body_html)).toBe(MINE_MORE);
    expect(inner(row.body_html)).toBe(`${BASE}<div>rewritten on the other device</div>`);
  });
});
