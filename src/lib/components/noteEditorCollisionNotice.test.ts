// @vitest-environment jsdom
//
// The collision moment, as the user sees it. Another device's edit arrives
// while this device has unsaved typing: the editor holds it back
// (shouldRenderExternalBody), and the next autosave keeps the edits here —
// merged in, or saved as a separate copy (noteEditorHeldBackSave.test.ts pins
// that). What this file pins is what the SCREEN says about it:
//
//   - a notice row that says the edits here will be kept, with the one way to
//     drop them, "Use other version";
//   - that action asks before discarding typing not yet saved, and acts at
//     once when there is nothing to lose;
//   - a keep-both is reported as what it is — a notice with "Open original",
//     or a toast if the user had moved on — never the red error bar.
//
// Mounted, with the same CAS-faithful `invoke` mock as the held-back tests:
// the notice, the dialog and the identity they hang on change across reactive
// turns, and only the component shows that (gotcha #28).
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes, error } from '../stores/notes';
import { toast } from '../stores/toast';
import { pendingLocalEdits } from '../notePersistence';
import { noteKey } from '../noteIdentity';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';
const UUID = 'uuid-primary';
const COPY_UUID = 'uuid-conflict-copy';

const wrap = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const inner = (html: string | undefined) => html?.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? html;
const BASE = '<div>shared line one that is long enough</div>';
const MINE = `${BASE}<div>typed on this device</div>`;
const MINE_MORE = `${MINE}<div>and a little more</div>`;
const THEIRS = `${BASE}<div>written on the other device</div>`;

interface Row { uuid: string; title: string; body_html: string; label: string; local_version: number; id: string }
let row: Row;
let copies: Row[];
// When set, the next save_note to the primary waits on it before SQLite applies it.
let gate: Promise<void> | null;

const asNote = (r: Row): Note => ({
  uuid: r.uuid, id: r.id, account_id: ACCOUNT_ID, title: r.title, body_html: r.body_html,
  date: '2026-09-26T00:00:00Z', label: r.label, local_version: r.local_version,
} as Note);

interface SaveArgs { existingUuid: string | null; expectedLocalVersion: number | null; title: string; bodyHtml: string; label: string }
const primarySaves = () =>
  invoke.mock.calls.filter(([c, a]) => c === 'save_note' && (a as SaveArgs).existingUuid === UUID);

describe('the collision notice', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let editor: HTMLElement;

  const notice = () => host.querySelector<HTMLElement>('[data-testid="external-change-notice"]');
  const copyNotice = () => host.querySelector<HTMLElement>('[data-testid="conflict-copy-notice"]');
  const dialog = () => host.querySelector<HTMLDialogElement>('dialog');
  const buttonIn = (root: ParentNode | null, text: string) =>
    Array.from(root?.querySelectorAll<HTMLButtonElement>('button') ?? [])
      .find((b) => (b.getAttribute('aria-label') ?? b.textContent?.trim()) === text) ?? null;

  function typeInto(html: string) {
    editor.innerHTML = html;
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
  }

  async function autosave() {
    await vi.advanceTimersByTimeAsync(1600);
    await tick();
    await tick();
    flushSync();
  }

  // The notice is new and nothing was typed since: the autosave waits up to
  // the reading window (NOTICE_READ_MS, 10s) before it answers the notice.
  async function afterReading() {
    await vi.advanceTimersByTimeAsync(10_100);
    await autosave();
  }

  async function settle() {
    for (let i = 0; i < 4; i++) await tick();
    flushSync();
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
    error.set(null);
    toast.set(null);
    invoke.mockImplementation(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return { outgoing: [], backlinks: [] };
      if (cmd === 'list_cached_notes_in_folder') {
        return [row, ...copies].filter((r) => r.label === args.path).map(asNote);
      }
      if (cmd === 'save_note') {
        const a = args as unknown as SaveArgs;
        if (!a.existingUuid) {
          const copy: Row = { uuid: `${COPY_UUID}${copies.length ? `-${copies.length}` : ''}`, id: '', title: a.title, body_html: a.bodyHtml, label: a.label, local_version: 1 };
          copies.push(copy);
          return { id: '', uuid: copy.uuid, local_version: 1 };
        }
        if (a.existingUuid !== UUID) {
          const i = copies.findIndex((c) => c.uuid === a.existingUuid);
          copies[i] = { ...copies[i], title: a.title, body_html: a.bodyHtml, local_version: copies[i].local_version + 1 };
          return { id: '', uuid: copies[i].uuid, local_version: copies[i].local_version };
        }
        if (gate) { const g = gate; gate = null; await g; }
        // apply_local_edit_versioned: WHERE local_version = expected.
        if (a.expectedLocalVersion !== null && a.expectedLocalVersion !== row.local_version) {
          throw 'This note changed elsewhere while you were editing (another device or an MCP agent). ' +
            'Reload the note and re-apply your changes before saving again.';
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

    // This device edits and autosaves: the row is now at v2, nothing unsaved.
    focusEditor();
    typeInto(MINE);
    await autosave();
    expect(row.local_version).toBe(2);
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    error.set(null);
    toast.set(null);
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  // Unsaved typing here, then the other device's edit (a keep-both reconcile).
  function collide() {
    typeInto(MINE_MORE);
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
  }

  it('says the edits here will be kept, with "Use other version", only while a change is held back', async () => {
    expect(notice()).toBeNull();
    collide();
    expect(notice()).not.toBeNull();
    expect(notice()!.textContent).toContain('Edited on another device. Your edits here will be kept.');
    // How they are kept, exactly as the editor does it (mergeOntoRow →
    // merge.rs merge_onto_moved_row): only lines the other device ADDED are
    // merged; any other change keeps both. "Changes on different lines are
    // merged in" promised more (re-critique after #292). This very collision
    // is the second case: the row lacks a line typed here, so it keeps both
    // (the keep-both tests below).
    expect(notice()!.querySelector('.touched-notice-detail')!.textContent).toBe(
      'Lines the other device added are merged into yours. If it changed or removed a line, your version is saved as a separate copy.',
    );
    expect(buttonIn(notice(), 'Use other version')).not.toBeNull();
    // No Reload anywhere in the editor any more.
    expect(buttonIn(host, 'Reload')).toBeNull();
    // The toolbar keeps its short label.
    // Said once, by the notice: no toolbar copy of the label (final re-critique).
    expect(host.querySelector('.save-status.external')).toBeNull();

    await afterReading(); // keeps both; the banner goes down, and so does the notice
    expect(notice()).toBeNull();
  });

  // 2026-11-12, Android, Gmail: the cursor in the note with nothing unsaved,
  // a refresh brought another device's edit, and focus alone held it back.
  // The first keystroke then saved the held-back text and kept both →
  // "(conflict copy 2026-11-12 19:46)". With nothing of the user's to lose,
  // the other version is simply shown, and focus stays in the body (on a
  // phone, the keyboard stays up).
  it('with nothing unsaved, the other version is shown at once, with no notice and focus kept', async () => {
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
    await settle();

    expect(notice()).toBeNull();
    const body = host.querySelector<HTMLElement>('.editor-body')!;
    expect(inner(body.innerHTML)).toBe(THEIRS);
    expect(document.activeElement).toBe(body);
    expect(dialog()).toBeNull();
  });

  it('with unsaved typing it asks first; "Keep my edits" saves them at once, as a copy, and returns to the text', async () => {
    collide();
    buttonIn(notice(), 'Use other version')!.click();
    await settle();

    expect(dialog()).not.toBeNull();
    expect(dialog()!.textContent).toContain('Use the other version?');
    expect(dialog()!.textContent).toContain("haven't been saved yet");

    // The answer the autosave was waiting for: save now, not 1.5s later.
    buttonIn(dialog(), 'Keep my edits')!.click();
    await settle();
    await settle();
    expect(dialog()).toBeNull();
    expect(copies).toHaveLength(1);
    expect(inner(copies[0].body_html)).toBe(MINE_MORE);
    expect(inner(row.body_html)).toBe(THEIRS);
    expect(copyNotice()).not.toBeNull();
    // Back in the text, not on <body>.
    expect(document.activeElement).toBe(editor);
  });

  // Re-critique after #294: after Discard, focus was on <body> and the save
  // word said "Unsaved" for the thrown-away text, which stayed in the drafts.
  it('"Discard my edits" returns focus to the text and leaves nothing "Unsaved"', async () => {
    collide();
    buttonIn(notice(), 'Use other version')!.click();
    await settle();
    buttonIn(dialog(), 'Discard my edits')!.click();
    await settle();
    await settle();
    expect(dialog()).toBeNull();
    // The reload renders a fresh editor element: focus is in it.
    expect(document.activeElement).toBe(host.querySelector('.editor-body'));
    expect(get(pendingLocalEdits)[noteKey(asNote(row))]).toBeUndefined();
    expect(host.querySelector('.editor-toolbar .save-status')!.textContent!.trim()).not.toBe('Unsaved');
  });

  // Re-critique after #295: Keep and Discard returned focus to the body but
  // put the caret on line 1 (a bare focus() starts at the beginning once the
  // dialog has taken the selection). It goes back to where it was.
  it('"Keep my edits" returns the caret to where it was, not to line 1', async () => {
    collide();
    const lines = editor.querySelectorAll('div');
    const last = lines[lines.length - 1].firstChild!;
    editor.focus();
    const r = document.createRange();
    r.setStart(last, 3);
    r.collapse(true);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(r);
    const offset = editor.textContent!.length - (last.textContent!.length - 3);
    buttonIn(notice(), 'Use other version')!.focus(); // focus leaves the body
    buttonIn(notice(), 'Use other version')!.click();
    await settle();
    buttonIn(dialog(), 'Keep my edits')!.click();
    await settle();
    await settle();
    const body = host.querySelector<HTMLElement>('.editor-body')!;
    expect(document.activeElement).toBe(body);
    const sel = window.getSelection()!.getRangeAt(0);
    const at = document.createRange();
    at.selectNodeContents(body);
    at.setEnd(sel.startContainer, sel.startOffset);
    expect(at.toString().length).toBe(offset);
  });

  it('"Discard my edits" shows the other version and never saves the discarded text', async () => {
    collide();
    buttonIn(notice(), 'Use other version')!.click();
    await settle();
    buttonIn(dialog(), 'Discard my edits')!.click();
    await settle();

    expect(dialog()).toBeNull();
    expect(notice()).toBeNull();
    expect(inner(host.querySelector('.editor-body')!.innerHTML)).toBe(THEIRS);

    const before = primarySaves().length;
    await autosave();
    expect(primarySaves()).toHaveLength(before);
    expect(copies).toHaveLength(0);
    expect(inner(row.body_html)).toBe(THEIRS);
    expect(get(selectedNote)?.uuid).toBe(UUID);
  });

  // Re-critique after #296: once the user typed after the notice appeared,
  // each keystroke had written their text over the held-back version in the
  // store, so Discard re-rendered their own text and did nothing.
  it('"Discard my edits" shows the other version even after typing under the notice', async () => {
    collide();
    typeInto(`${MINE_MORE}<div>typed after the notice</div>`);
    buttonIn(notice(), 'Use other version')!.click();
    await settle();
    buttonIn(dialog(), 'Discard my edits')!.click();
    await settle();
    await settle();

    expect(notice()).toBeNull();
    expect(inner(host.querySelector('.editor-body')!.innerHTML)).toBe(THEIRS);
    expect(inner(get(selectedNote)?.body_html)).toBe(THEIRS);
    const before = primarySaves().length;
    await autosave();
    expect(primarySaves()).toHaveLength(before);
    expect(copies).toHaveLength(0);
  });

  // Re-critique after #293: the autosave already scheduled by the typing ran
  // ~1.3s after the dialog opened, forked the note under the question and
  // pulled the dialog away. It now waits while the user answers.
  it('the autosave waits while the dialog is open: no fork under the question', async () => {
    const savedBefore = primarySaves().length; // beforeEach's own saves
    collide();
    buttonIn(notice(), 'Use other version')!.click();
    await settle();
    expect(dialog()).not.toBeNull();

    await autosave(); // the debounce the typing scheduled comes due…
    await autosave(); // …and again: still waiting
    await settle();

    expect(copies).toHaveLength(0);
    expect(primarySaves()).toHaveLength(savedBefore);
    expect(dialog()).not.toBeNull();
    expect(notice()).not.toBeNull();
  });

  // Re-critique after #294–#296: reading the notice did not hold the
  // autosave, so the question settled itself ~1.5s after the last keystroke.
  // Now it waits while the notice is new and nothing has been typed since
  // (up to 10s, so the edits still reach SQLite), and typing on answers it.
  it('the autosave waits while the user reads the new notice, and typing on lets it answer', async () => {
    const savedBefore = primarySaves().length;
    collide();
    await autosave();
    await autosave();
    expect(copies).toHaveLength(0);
    expect(primarySaves()).toHaveLength(savedBefore);
    expect(notice()).not.toBeNull();

    typeInto(`${MINE_MORE}<div>and on</div>`); // the user carries on
    await autosave();
    await settle();
    expect(copies).toHaveLength(1);
  });

  it('…and only up to the reading window: walking away still saves the edits', async () => {
    collide();
    await afterReading();
    await settle();
    expect(copies).toHaveLength(1);
  });

  it('"See the other version" shows the other device\'s text before the user chooses', async () => {
    collide();
    const other = notice()!.querySelector('[data-testid="other-version"]')!;
    expect(other.textContent).toContain('written on the other device');
    expect(other.closest('details')!.querySelector('summary')!.textContent!.trim()).toBe('See the other version');
  });

  it('the autosave also waits while keyboard focus is on the notice', async () => {
    collide();
    buttonIn(notice(), 'Use other version')!.focus();
    await autosave();
    await settle();
    expect(copies).toHaveLength(0);
    expect(notice()).not.toBeNull();

    editor.focus(); // focus leaves the notice: once the reading window is past, the next due autosave runs
    await afterReading();
    await settle();
    expect(copies).toHaveLength(1);
  });

  it('a save still in flight counts as unsaved: it asks, and the dialog goes when that save keeps the edits', async () => {
    let release!: () => void;
    gate = new Promise((r) => { release = r; });
    typeInto(MINE_MORE);
    await vi.advanceTimersByTimeAsync(1600); // the save is parked on the gate
    row = { ...row, body_html: wrap(THEIRS), local_version: 3 };
    deliverRefresh();
    expect(notice()).not.toBeNull();
    buttonIn(notice(), 'Use other version')!.click();
    flushSync();
    expect(dialog()).not.toBeNull();

    release();
    await autosave();

    expect(dialog()).toBeNull();
    expect(get(selectedNote)?.uuid).toBe(COPY_UUID);
    expect(editor.innerHTML).toBe(MINE_MORE);
    expect(inner(row.body_html)).toBe(THEIRS);
  });

  it('a keep-both shows a notice, not an error, and "Open original" opens the note with the other version', async () => {
    collide();
    await afterReading();

    expect(get(error)).toBeNull();
    expect(get(selectedNote)?.uuid).toBe(COPY_UUID);
    const n = copyNotice();
    expect(n).not.toBeNull();
    expect(n!.textContent).toContain("Saved as a separate copy. “Shared” kept the other device's version.");

    buttonIn(n, 'Open original')!.click();
    await settle();

    expect(get(selectedNote)?.uuid).toBe(UUID);
    expect(inner(get(selectedNote)?.body_html)).toBe(THEIRS);
    expect(inner(host.querySelector('.editor-body')!.innerHTML)).toBe(THEIRS);
    // The notice was about the copy; it went with it.
    expect(copyNotice()).toBeNull();
  });

  it('the keep-both notice goes on Dismiss', async () => {
    collide();
    await afterReading();
    expect(copyNotice()).not.toBeNull();

    buttonIn(copyNotice(), 'Dismiss')!.click();
    flushSync();
    expect(copyNotice()).toBeNull();
    expect(get(selectedNote)?.uuid).toBe(COPY_UUID);
  });

  it('the keep-both notice goes with the note it was shown on', async () => {
    collide();
    await afterReading();
    expect(copyNotice()).not.toBeNull();

    const other = { ...asNote(row), uuid: 'uuid-other', body_html: wrap('<div>other</div>'), title: 'Other' };
    notes.update((ns) => [...ns, other]);
    selectedNote.set(other);
    flushSync();
    expect(copyNotice()).toBeNull();
  });

  it('a keep-both after the user moved on is a toast, not an error', async () => {
    collide();
    // Switch notes before autosave fires: the flush for the old note keeps both.
    const other = { ...asNote(row), uuid: 'uuid-other', body_html: wrap('<div>other</div>'), title: 'Other' };
    notes.update((ns) => [...ns, other]);
    selectedNote.set(other);
    flushSync();
    await autosave();

    expect(copies).toHaveLength(1);
    expect(get(error)).toBeNull();
    expect(get(toast)?.text).toBe("Saved as a separate copy. “Shared” kept the other device's version.");
    expect(copyNotice()).toBeNull();
  });
});
