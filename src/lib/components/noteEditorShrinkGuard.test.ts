// @vitest-environment jsdom
//
// The editor refuses a save that would shrink a note to under a quarter of
// its text ("Autosave blocked: would shrink note from N to M chars"). Found in
// the 2026-10-29 Tauri 2.12 live pass, two ways it went wrong:
//
// 1. It counted the stored body by stripping tags from the HTML, but the
//    edited body by the editor's `innerText`, which leaves out a closed
//    <details>. Every AI-made note keeps its verbatim source in one, so an
//    edit to a note whose source dwarfs its summary was always refused.
// 2. The save that runs when you switch notes (or the editor goes) skipped
//    the check entirely, so the refused edit landed anyway one click later —
//    and so would a real wipe.
//
// Both paths now count both bodies the same way, through one check.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { accounts, currentAccount, selectedNote, selectedFolder, selectedSmartFolder, selectedTags, notes, error } from '../stores/notes';
import { pendingLocalEdits } from '../notePersistence';
import { noteKey } from '../noteIdentity';
import type { Note } from '../types';
import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = { id: 'ssh:abc', email: 'host:Notes', added_at: '2026-09-26T07:32:25Z', backend_kind: 'ssh', status: 'active' };
const SOURCE = 'verbatim source text '.repeat(300); // ~6,300 chars, folded away
const SUMMARY = '<p>A short summary of the page.</p>';
const aiBody = (summary: string) =>
  `<html><head></head><body>${summary}<hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>${SOURCE}</pre>\n</details>\n</body></html>`;
const A: Note = {
  uuid: 'uuid-a', id: 'Notes/Inbox/a.html', account_id: ACCOUNT.id, title: 'AI note',
  body_html: aiBody(SUMMARY), date: '2026-10-29T00:00:00Z', label: 'Notes/Inbox', local_version: 1,
} as Note;
const B: Note = {
  uuid: 'uuid-b', id: 'Notes/Inbox/b.html', account_id: ACCOUNT.id, title: 'Other',
  body_html: '<html><head></head><body><div>bravo text here</div></body></html>', date: '2026-10-29T00:00:00Z', label: 'Notes/Inbox', local_version: 1,
} as Note;

interface SaveArgs { existingUuid: string | null; bodyHtml: string }
const savesForA = () =>
  invoke.mock.calls.filter(([c, a]) => c === 'save_note' && (a as SaveArgs).existingUuid === 'uuid-a').map(([, a]) => a as SaveArgs);

let target: HTMLElement;
let host: ReturnType<typeof mount>;

async function openA(): Promise<HTMLElement> {
  target = document.createElement('div');
  document.body.appendChild(target);
  host = mount(NoteEditor, { target });
  flushSync();
  await vi.advanceTimersByTimeAsync(0);
  await tick();
  return target.querySelector('.editor-body') as HTMLElement;
}

function edit(editor: HTMLElement, mutate: (el: HTMLElement) => void) {
  mutate(editor);
  editor.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

const appendWord = (el: HTMLElement) => { el.querySelector('p')!.append(' smoketest212'); };
const wipe = (el: HTMLElement) => { el.innerHTML = '<div>x</div>'; };

describe('the shrink guard', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'save_note') return Promise.resolve({ id: `id-${args.existingUuid}`, uuid: args.existingUuid, local_version: 2 });
      return Promise.resolve([]);
    });
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    selectedFolder.set('Notes/Inbox');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    notes.set([A, B]);
    selectedNote.set(A);
    error.set(null);
  });
  afterEach(() => {
    try { unmount(host); } catch { /* already unmounted */ }
    target?.remove();
    vi.useRealTimers();
    selectedNote.set(null);
    notes.set([]);
    error.set(null);
  });

  it('autosave saves an edit to an AI-made note whose source is folded away', async () => {
    const editor = await openA();
    edit(editor, appendWord);
    await vi.advanceTimersByTimeAsync(2000);
    await tick();
    expect(get(error)).toBeNull();
    expect(savesForA()).toHaveLength(1);
    expect(savesForA()[0].bodyHtml).toContain('smoketest212');
    expect(savesForA()[0].bodyHtml).toContain('Source (verbatim)');
  });

  it('autosave still refuses a save that would wipe the note', async () => {
    const editor = await openA();
    edit(editor, wipe);
    await vi.advanceTimersByTimeAsync(2000);
    await tick();
    expect(savesForA()).toEqual([]);
    expect(get(error)).toMatch(/^Autosave blocked: would shrink note/);
  });

  it('switching notes does not slip a wipe past the guard', async () => {
    const editor = await openA();
    edit(editor, wipe);
    selectedNote.set(B); // before the 1.5 s debounce
    flushSync();
    await vi.advanceTimersByTimeAsync(2000);
    await tick();
    expect(savesForA()).toEqual([]);
    expect(get(error)).toMatch(/^Autosave blocked: would shrink note/);
    // The refused body is not left behind for the next save to pick up.
    expect(get(notes).find((n) => n.uuid === 'uuid-a')?.body_html).toContain('Source (verbatim)');
    expect(Object.keys(get(pendingLocalEdits))).not.toContain(noteKey(A));
  });

  it('switching notes still saves an ordinary edit to an AI-made note', async () => {
    const editor = await openA();
    edit(editor, appendWord);
    selectedNote.set(B);
    flushSync();
    await vi.advanceTimersByTimeAsync(2000);
    await tick();
    expect(get(error)).toBeNull();
    expect(savesForA()).toHaveLength(1);
    expect(savesForA()[0].bodyHtml).toContain('smoketest212');
  });
});
