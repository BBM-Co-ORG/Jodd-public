// @vitest-environment jsdom
//
// The One Voice Rule (DESIGN.md; editor/editorStatus.ts): every editor state
// is said in one place, its actions live there, and only states that need the
// user are announced. Before it, a refused push was said three times (the
// metadata chip, its popover, the notice) with two Try again buttons, the
// reason was the transport's own words ("no Exchange folder id for 'Notes'"),
// the save word announced every autosave, and a notice arriving mid-typing
// moved the line under the caret (re-critique after #291, 28/40).
import './components/__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readFileSync } from 'node:fs';
import { EDITOR_STATUS, statesSaidBy, COPY_TO_NEW_NOTE, MOVE_TO_NOTES } from './editor/editorStatus';
import { META_STATUS_PRIORITY } from './editor/metaStatus';
import { reasonForUser, UNKNOWN_REFUSAL, type RefusalAction } from './controllers/noteStatus';
import { accounts, notes, selectedNote } from './stores/notes';
import type { Note } from './types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './components/NoteEditor.svelte';

describe('the status table', () => {
  it('the chip says exactly the chip-voiced states, nothing a notice says', () => {
    expect([...META_STATUS_PRIORITY].sort()).toEqual(statesSaidBy('chip').sort());
    for (const id of statesSaidBy('notice')) expect(META_STATUS_PRIORITY as readonly string[]).not.toContain(id);
  });

  it('the save word is never announced; notices with actions always are', () => {
    for (const id of statesSaidBy('saveWord')) expect(EDITOR_STATUS[id].announced, id).toBe(false);
    for (const id of statesSaidBy('notice')) {
      if (EDITOR_STATUS[id].actions.length) expect(EDITOR_STATUS[id].announced, id).toBe(true);
    }
  });

  it('a refused push offers only the actions its class can use; a lock offers the copy', () => {
    expect(EDITOR_STATUS.notSynced.actions).toEqual(['Try again', COPY_TO_NEW_NOTE, MOVE_TO_NOTES]);
    expect(EDITOR_STATUS.readonly.actions).toEqual([COPY_TO_NEW_NOTE]);
    // "Save as a copy" sat beside the keep-both notice's "Saved as a separate
    // copy", one word for two things.
    expect(COPY_TO_NEW_NOTE).not.toMatch(/save/i);
  });
});

const LABEL: Record<RefusalAction, string> = { retry: 'Try again', copy: COPY_TO_NEW_NOTE, moveToRoot: MOVE_TO_NOTES };

describe('reasonForUser: a refusal in plain words, with the next step and only buttons that can work', () => {
  const cases: [string, boolean, string, RegExp, RefusalAction[]][] = [
    // A folder the server refused: Move to Notes, never a copy into the same folder.
    ["permanent: no Exchange folder id for 'Notes/Ideas', and Graph offers…", false, 'Notes/Ideas', /^Outlook can't find the folder \u201cIdeas\u201d yet\. Move the note to Notes and it will sync\.$/, ['moveToRoot']],
    ["permanent: Gmail refused to create label 'Notes/New' (HTTP 409) and has no label…", false, 'Notes/New', /^Gmail wouldn't create the folder \u201cNew\u201d\. Move the note to Notes/, ['moveToRoot']],
    // Notes itself: Outlook writes it by its well-known name, so a copy there syncs.
    ["permanent: no Exchange folder id for 'Notes'", false, 'Notes', /^Outlook can't reach its Notes folder yet\. Try again later, or copy your text to a new note\.$/, ['retry', 'copy']],
    ['permanent: attachments are impossible on an Exchange account — Apple refuses them too.', false, 'Notes', /^This account can't keep attachments/, ['retry']],
    ['permanent: attachments are not supported on SSH accounts yet', false, 'Notes', /^This account can't keep attachments/, ['retry']],
    ['permanent: Permission denied (os error 13)', false, 'Notes', /^Jodd can't write to this folder on disk/, ['retry']],
    ['permanent: this note is password-protected in Apple Notes — Jodd can show its title but cannot write to it', true, 'Notes', /^This note is password-protected in Apple Notes\. Edit it there\.$/, ['copy']],
    ['permanent: this note contains 2 attachment(s), table(s) or inline tag(s) Jodd cannot yet write — open it in Apple Notes to edit', true, 'Notes', /^This note has attachments, tables or tags/, ['copy']],
    ["permanent: this note's formatting (3 run(s) covering 10 of 12 characters) is not a shape Jodd can preserve — open it in Apple Notes to edit", true, 'Notes', /^Jodd can't keep this note's formatting exactly.*Edit it in Apple Notes\.$/, ['copy']],
  ];
  it.each(cases)('%s', (raw, byRemote, label, sentence, actions) => {
    const r = reasonForUser(raw, byRemote, label);
    expect(r.sentence).toMatch(sentence);
    expect(r.actions).toEqual(actions);
    // The backend's own words stay one click away, never dropped.
    expect(r.detail).toBe(raw.replace(/^permanent:\s*/, ''));
    expect(r.sentence).not.toMatch(/permanent:|HTTP|Exchange|os error|run\(s\)/);
    // Every action is one the table names for its state.
    const plan = byRemote ? EDITOR_STATUS.readonly : EDITOR_STATUS.notSynced;
    for (const a of r.actions) expect(plan.actions).toContain(LABEL[a]);
  });

  it('a folder-class refusal never offers a copy (it would land in the refused folder)', () => {
    for (const raw of ["permanent: no Exchange folder id for 'Notes/A'", "permanent: Gmail refused to create label 'Notes/B' (HTTP 409)"]) {
      expect(reasonForUser(raw, false, 'Notes/A').actions).not.toContain('copy');
    }
  });

  it('a note already in Notes is not told to move to Notes', () => {
    const r = reasonForUser("permanent: no Exchange folder id for 'Notes/Gone'", false, 'Notes');
    expect(r.actions).toEqual(['retry', 'copy']);
    expect(r.sentence).not.toMatch(/Move the note to Notes/);
  });

  it('an unknown refusal falls back to a plain sentence, keeps the raw text, and offers Try again and a copy', () => {
    expect(reasonForUser('permanent: HTTP 400: {"error":"weird"}')).toEqual({ sentence: UNKNOWN_REFUSAL, detail: 'HTTP 400: {"error":"weird"}', actions: ['retry', 'copy'] });
    const lock = reasonForUser('permanent: something new', true);
    expect(lock.sentence).toBe("Jodd can't write to this note. Edit it in Apple Notes.");
    expect(lock.actions).toEqual(['copy']);
  });

  it('a folder hold is written for the user already: shown as it is, no Details, and the note can leave for Notes', () => {
    const hold = "Not sent: folder 'Notes/A' could not be created on the server (busy). Rename that folder or press Try again on it, or move this note to another folder.";
    expect(reasonForUser(hold, false, 'Notes/A')).toEqual({ sentence: hold, detail: null, actions: ['moveToRoot'] });
  });
});

describe('the mounted editor', () => {
  const ACCOUNT = { id: 'microsoft:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'microsoft', status: 'active' };
  const NOTE = {
    uuid: 'uuid-one-voice', id: 'msg-1', account_id: ACCOUNT.id, title: 'One voice',
    body_html: '<div>first line</div>', date: '2026-11-07T00:00:00Z', label: 'Notes', local_version: 1,
  } as Note;
  let host: HTMLElement;
  let app: ReturnType<typeof mount> | null = null;
  let observers = new Map<Element, () => void>();
  const RealRO = globalThis.ResizeObserver;

  async function render(note: Note) {
    notes.set([note]);
    selectedNote.set(note);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host });
    flushSync();
    await tick();
    await tick();
    flushSync();
  }

  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'note_persistence') return Promise.resolve(null);
      return Promise.resolve([]);
    });
    accounts.set([ACCOUNT as never]);
    // jsdom has no ResizeObserver: capture the notice stack's callback.
    observers = new Map();
    (globalThis as Record<string, unknown>).ResizeObserver = class {
      cb: () => void;
      constructor(cb: () => void) { this.cb = cb; }
      observe(el: Element) { observers.set(el, this.cb); }
      unobserve() {}
      disconnect() {}
    };
  });
  afterEach(() => {
    if (app) unmount(app);
    app = null;
    host?.remove();
    selectedNote.set(null);
    notes.set([]);
    (globalThis as Record<string, unknown>).ResizeObserver = RealRO;
  });

  const buttons = (label: string) => Array.from(host.querySelectorAll('button')).filter((b) => b.textContent?.trim() === label);

  it('a refused push: each of its actions is on screen once, in its notice, and nothing else says it', async () => {
    await render({ ...NOTE, push_blocked_reason: "permanent: no Exchange folder id for 'Notes'", push_blocked_by_remote: false } as Note);
    const notice = host.querySelector('[data-testid="blocked-notice"]')!;
    for (const label of ['Try again', COPY_TO_NEW_NOTE]) {
      expect(buttons(label), label).toHaveLength(1);
      expect(notice.contains(buttons(label)[0]), label).toBe(true);
    }
    expect(buttons(MOVE_TO_NOTES)).toHaveLength(0); // already in Notes
    expect(host.querySelector('.meta-line .meta-status')).toBeNull();
    expect(host.querySelectorAll('[role="status"]').length).toBeGreaterThan(0);
    expect(host.querySelector('.editor-toolbar .save-status[role]')).toBeNull();
  });

  it('a folder the server refused offers Move to Notes only, and it moves the note there and re-arms it', async () => {
    const n = { ...NOTE, label: 'Notes/Ideas', push_blocked_reason: "permanent: no Exchange folder id for 'Notes/Ideas'", push_blocked_by_remote: false } as Note;
    await render(n);
    const notice = host.querySelector('[data-testid="blocked-notice"]')!;
    expect(Array.from(notice.querySelectorAll(':scope > button')).map((b) => b.textContent?.trim())).toEqual([MOVE_TO_NOTES]);
    expect(notice.textContent).toContain("Outlook can't find the folder \u201cIdeas\u201d yet.");
    buttons(MOVE_TO_NOTES)[0].click();
    flushSync();
    await tick();
    await tick();
    expect(invoke).toHaveBeenCalledWith('move_notes_batch', { accountId: ACCOUNT.id, uuids: [n.uuid], targetLabel: 'Notes' });
    flushSync();
    // The move re-armed the push: the notice stays, trying again, and says
    // what is happening — not "Move the note to Notes", which the user just
    // did (re-critique after #296) — with no button left to press. The
    // context notice ("You're viewing …") does not stack under it.
    const after = host.querySelector('[data-testid="blocked-notice"]')!;
    expect(after.querySelector('.blocked-notice-lead')!.textContent).toBe('Trying again. Your text is saved here.');
    expect(after.textContent).toContain('Moved to Notes. Sending it now.');
    expect(after.textContent).not.toContain('Move the note to Notes');
    expect(buttons(MOVE_TO_NOTES)).toHaveLength(0);
    expect(host.querySelector('.context-notice')).toBeNull();
  });

  it('a note Apple locked leads with where the last edit is, and offers the copy that keeps it', async () => {
    await render({ ...NOTE, push_blocked_reason: 'permanent: this note is password-protected in Apple Notes — Jodd can show its title but cannot write to it', push_blocked_by_remote: true } as Note);
    const notice = host.querySelector<HTMLElement>('[data-testid="blocked-notice"]')!;
    expect(notice.querySelector('.blocked-notice-lead')!.textContent).toBe('Read-only in Jodd. Your last edit is saved on this device only.');
    expect(Array.from(notice.querySelectorAll(':scope > button')).map((b) => b.textContent?.trim())).toEqual([COPY_TO_NEW_NOTE]);
    expect(buttons('Try again')).toHaveLength(0);
  });

  it('the title is named for a screen reader, not only by its placeholder', async () => {
    await render(NOTE);
    expect(host.querySelector('.title-input')!.getAttribute('aria-label')).toBe('Note title');
  });

  it('a notice arriving while typing scrolls the body by its height, so the caret line stays put', async () => {
    await render(NOTE);
    const stack = host.querySelector<HTMLElement>('.notice-stack')!;
    const body = host.querySelector<HTMLElement>('.editor-body')!;
    let scrollTop = 40;
    Object.defineProperty(body, 'scrollTop', { configurable: true, get: () => scrollTop, set: (v: number) => { scrollTop = v; } });
    let height = 0;
    Object.defineProperty(stack, 'offsetHeight', { configurable: true, get: () => height });
    const observed = observers.get(stack);
    expect(observed).toBeDefined();

    body.focus();
    expect(document.activeElement).toBe(body);
    height = 66; // "Edited on another device" arrives
    observed!();
    expect(scrollTop).toBe(106);
    height = 0; // and goes
    observed!();
    expect(scrollTop).toBe(40);

    // Not typing: a reader sees the notice push the text, as any arrival does.
    body.blur();
    height = 66;
    observed!();
    expect(scrollTop).toBe(40);
  });

  // Re-critique after #296: a keep-both renames the title "… (conflict copy
  // …)", which wraps at a narrow width; the title was outside the hold.
  it('the title growing above the body is held too', async () => {
    await render(NOTE);
    const stack = host.querySelector<HTMLElement>('.notice-stack')!;
    const title = host.querySelector<HTMLElement>('.title-input')!;
    const body = host.querySelector<HTMLElement>('.editor-body')!;
    let scrollTop = 100;
    Object.defineProperty(body, 'scrollTop', { configurable: true, get: () => scrollTop, set: (v: number) => { scrollTop = v; } });
    let titleH = 49;
    Object.defineProperty(title, 'offsetHeight', { configurable: true, get: () => titleH });
    Object.defineProperty(stack, 'offsetHeight', { configurable: true, get: () => 0 });
    // The action re-reads its baseline when the title element is handed over.
    const observed = observers.get(title) ?? observers.get(stack);
    expect(observed).toBeDefined();
    observed!(); // settle the baseline at 49
    body.focus();
    titleH = 82; // the title wraps to two lines
    observed!();
    expect(scrollTop).toBe(133);
  });

  it('a narrow editor wraps a notice\'s buttons under its text (container query on the stack)', () => {
    const src = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
    const style = src.slice(src.indexOf('<style>'));
    expect(style).toMatch(/\.notice-stack \{[^}]*container-type:\s*inline-size/);
    const q = style.indexOf('@container (max-width: 480px)');
    expect(q).toBeGreaterThan(0);
    expect(style.slice(q, q + 400)).toMatch(/flex-wrap:\s*wrap/);
  });
});
