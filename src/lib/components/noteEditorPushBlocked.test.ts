// @vitest-environment jsdom
//
// A permanently-refused push (gotcha #14) used to show its reason only as a
// hover tooltip on "Sync blocked" — invisible on Android, and easy to miss on
// desktop. On 2026-08-27 an iCloud note was refused five times in two minutes
// ("…open it in Apple Notes to edit"), each refusal right after another edit,
// because nothing on screen said editing here could not work.
//
// Rules, asserted on the mounted component (gotcha #28):
//   - the reason is visible text whenever the note is blocked: a plain
//     sentence with the next step, the backend's own words behind Details;
//   - it is said ONCE, by the notice above the title (the One Voice Rule,
//     editor/editorStatus.ts): no metadata chip, no popover, one Try again;
//   - a block by the remote's own document (`push_blocked_by_remote`) locks
//     the note, because no edit made here can get past it. Any other block
//     leaves the editor open — an edit or a move may be the fix.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { accounts, selectedNote, notes } from '../stores/notes';
import { persistenceByNote } from '../notePersistence';
import { noteKey } from '../noteIdentity';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = {
  id: 'icloud:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'icloud',
  status: 'active',
};

const REASON =
  "permanent: Jodd's own reading of this note does not reproduce it exactly, so it will not overwrite it — open it in Apple Notes to edit";

const NOTE: Note = {
  uuid: '4de5ef24-d5ad-4b7a-9ca0-db923e55634e',
  id: '4de5ef24-d5ad-4b7a-9ca0-db923e55634e',
  account_id: ACCOUNT.id,
  title: 'A note',
  body_html: '<div>A note</div><div>body</div>',
  date: '2026-08-27T00:00:00Z',
  label: 'Notes',
  local_version: 3,
} as Note;

function stubInvoke() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'note_persistence') return Promise.resolve(null);
    return Promise.resolve([]);
  });
}

let target: HTMLDivElement;
let host: ReturnType<typeof mount> | null = null;

async function render() {
  target = document.createElement('div');
  document.body.appendChild(target);
  host = mount(NoteEditor, { target });
  flushSync();
  await tick();
  await Promise.resolve();
  flushSync();
}

const notice = () => target.querySelector<HTMLElement>('[data-testid="blocked-notice"]');
const chip = () => target.querySelector('.meta-line .meta-status');
const popover = () => target.querySelector('[data-testid="meta-status-popover"]');
const buttonsLabelled = (label: string) =>
  Array.from(target.querySelectorAll('button')).filter((b) => b.textContent?.trim() === label);
const body = () => target.querySelector('.editor-body') as HTMLElement;
const titleInput = () => target.querySelector('.title-input') as HTMLInputElement;
const EXCHANGE = "permanent: no Exchange folder id for 'Notes'";

describe('NoteEditor shows why a push is blocked, and locks only when editing cannot help', () => {
  beforeEach(() => {
    invoke.mockReset();
    stubInvoke();
    accounts.set([ACCOUNT]);
  });
  afterEach(() => {
    if (host) unmount(host);
    host = null;
    target?.remove();
  });

  it('a block by the remote document says where to edit, as text, and locks the note', async () => {
    const blocked = { ...NOTE, push_blocked_reason: REASON, push_blocked_by_remote: true };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(notice()?.textContent).toContain('Read-only in Jodd.');
    expect(notice()?.textContent).toContain('Edit it in Apple Notes.');
    expect(body().getAttribute('contenteditable')).toBe('false');
    expect(titleInput().readOnly).toBe(true);
    // Tags take no edits either. The tag field lives in the tags popover now
    // (spec 2026-10-20 §6.2); with no tags and none addable there is no #N to
    // open it, and no field anywhere. The popover's own read-only rule for a
    // blocked note is pinned in tagsPopover.test.ts.
    expect(target.querySelector('.meta-line .meta-tags')).toBeNull();
    expect(document.querySelector('.tag-input')).toBeNull();
  });

  it('an ordinary block says what to do, as text, but leaves the editor open', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(notice()?.textContent).toContain("Outlook can't reach its Notes folder yet. Try again later, or copy your text to a new note.");
    expect(body().getAttribute('contenteditable')).toBe('true');
    expect(titleInput().readOnly).toBe(false);
  });

  it('a note syncing normally has no notice, no chip, and is editable', async () => {
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    await render();

    expect(notice()).toBeNull();
    expect(chip()).toBeNull();
    expect(body().getAttribute('contenteditable')).toBe('true');
  });

  it('"Try again" re-arms this note only, not every blocked note in the account', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    const other = {
      ...NOTE,
      uuid: 'other-note',
      id: 'other-note',
      push_blocked_reason: REASON,
      push_blocked_by_remote: true,
    };
    notes.set([blocked, other]);
    selectedNote.set(blocked);
    await render();

    buttonsLabelled('Try again')[0].click();
    flushSync();
    await tick();

    expect(invoke).toHaveBeenCalledWith('retry_blocked_push', { accountId: ACCOUNT.id, uuid: NOTE.uuid });
    expect(invoke).not.toHaveBeenCalledWith('retry_blocked_pushes', expect.anything());
    const after = get(notes);
    expect(after.find((n) => n.uuid === NOTE.uuid)?.push_blocked_reason).toBeNull();
    expect(after.find((n) => n.uuid === 'other-note')?.push_blocked_reason).toBe(REASON);
  });

  // The re-critique after #291: the refusal was said three times — the
  // chip, its popover and the notice — with two Try again buttons. Now the
  // notice is its one voice; the toolbar word keeps its slot, invisible and
  // silent, so the formatting row does not reflow.
  it('is said once: no chip, no popover, one Try again, and the toolbar word held invisible', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(chip()).toBeNull();
    expect(popover()).toBeNull();
    expect(buttonsLabelled('Try again')).toHaveLength(1);
    expect(notice()!.contains(buttonsLabelled('Try again')[0])).toBe(true);
    const word = target.querySelector<HTMLElement>('.editor-toolbar .save-status')!;
    expect(word.classList.contains('held')).toBe(true);
    expect(word.getAttribute('aria-hidden')).toBe('true');
    expect(word.hasAttribute('role')).toBe(false);
  });

  it('a remote-locked note offers no Try again anywhere', async () => {
    const blocked = { ...NOTE, push_blocked_reason: REASON, push_blocked_by_remote: true };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(chip()).toBeNull();
    expect(buttonsLabelled('Try again')).toEqual([]);
  });

  // Fix round 1: Try again clears the block optimistically; when the worker
  // refuses the push again, the notice returns.
  // Re-critique after #293: Try again (or an edit) used to unmount the notice
  // and the refusal brought it back, so the text jumped twice. It now stays,
  // saying it is trying again, until the push is refused again or succeeds.
  it('Try again keeps the notice up, trying again, until the answer: refused again', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();
    const before = notice();

    buttonsLabelled('Try again')[0].click();
    flushSync();
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    flushSync();
    expect(notice()).toBe(before); // the same element: nothing remounted
    expect(notice()!.querySelector('.blocked-notice-lead')!.textContent).toBe('Trying again. Your text is saved here.');
    expect(buttonsLabelled('Try again')[0].disabled).toBe(true);
    expect(target.querySelector('.editor-toolbar .save-status')!.classList.contains('held')).toBe(true);

    selectedNote.set({ ...blocked }); // the worker's nudge: refused again
    flushSync();
    await tick();
    flushSync();
    expect(notice()).toBe(before);
    expect(notice()!.querySelector('.blocked-notice-lead')!.textContent).toBe('Not synced. Your text is saved on this device.');
    expect(buttonsLabelled('Try again')[0].disabled).toBe(false);
    expect(chip()).toBeNull();
  });

  it('…and goes only when the push succeeds', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();
    buttonsLabelled('Try again')[0].click();
    flushSync();
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    expect(notice()).not.toBeNull();

    persistenceByNote.set({ [noteKey(blocked)]: { account_id: ACCOUNT.id, uuid: NOTE.uuid, local_version: NOTE.local_version!, sync_state: 'clean', push_blocked_reason: null, push_blocked_by_remote: false } });
    flushSync();
    expect(notice()).toBeNull();
    persistenceByNote.set({});
  });

  // After #294 the notice flipped its lead and greyed its buttons on every
  // typing pause. Typing changes nothing now: same words, buttons usable,
  // the save word held. Only an asked-for retry says it is trying.
  it('typing in a refused note changes nothing: same notice, same words, buttons usable, word held', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();
    const before = notice();
    const text = before!.textContent;
    body().innerHTML = '<div>A note</div><div>body, edited</div>';
    body().dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    expect(notice()).toBe(before);
    expect(notice()!.textContent).toBe(text);
    expect(buttonsLabelled('Try again')[0].disabled).toBe(false);
    const word = target.querySelector<HTMLElement>('.editor-toolbar .save-status')!;
    expect(word.classList.contains('held')).toBe(true);
  });

  it('the lock lifts when the block clears', async () => {
    const blocked = { ...NOTE, push_blocked_reason: REASON, push_blocked_by_remote: true };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();
    expect(body().getAttribute('contenteditable')).toBe('false');

    // What a pull that replaced the document delivers (`upsert_from_remote`
    // clears both fields).
    selectedNote.set({ ...NOTE, push_blocked_reason: null, push_blocked_by_remote: false });
    flushSync();
    await tick();

    expect(notice()).toBeNull();
    expect(body().getAttribute('contenteditable')).toBe('true');
  });

  // Final re-critique: a refused push was a 12px red chip, its reason behind
  // a click, silent to screen readers, with only "Try again". Now a notice
  // says it where the user is looking, as a live region, with a safe exit.
  // After #291: the reason in plain words; the transport's own text (which
  // the user cannot act on) behind Details, for a bug report.
  it('a refused push shows a live notice: text safe, a plain reason, Details, Try again and Copy to a new note', async () => {
    const blocked = { ...NOTE, push_blocked_reason: EXCHANGE, push_blocked_by_remote: false };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    const n = notice()!;
    // Announced by the notices' one live region (always mounted, so the
    // arrival is a change it announces), by its lead and sentence only.
    expect(n.hasAttribute('role')).toBe(false);
    const announcer = target.querySelector('[data-testid="notice-announcer"]')!;
    expect(announcer.getAttribute('role')).toBe('status');
    expect(announcer.textContent).toContain('Not synced. Your text is saved on this device.');
    expect(announcer.textContent).not.toContain('Copy to a new note'); // a button label
    expect(announcer.textContent).not.toContain('Details');
    expect(n.textContent).toContain('Not synced. Your text is saved on this device.');
    expect(n.querySelector('.touched-notice-detail')!.textContent).not.toContain('Exchange');
    const details = n.querySelector('details.notice-details')!;
    expect(details.querySelector('summary')!.textContent!.trim()).toBe('Details');
    expect(details.textContent).toContain("no Exchange folder id for 'Notes'");
    expect(n.textContent).not.toContain('permanent:');
    const labels = Array.from(n.querySelectorAll('button')).map((b) => b.textContent?.trim());
    expect(labels).toEqual(['Try again', 'Copy to a new note']);

    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'save_note') return Promise.resolve({ id: 'copy-id', uuid: 'copy-uuid', local_version: 1 });
      if (cmd === 'note_persistence') return Promise.resolve(null);
      return Promise.resolve([]);
    });
    n.querySelectorAll('button')[1].click();
    flushSync();
    await tick();
    await tick();
    expect(invoke.mock.calls.some(([c, a]) => c === 'save_note' && (a as { existingUuid: string | null }).existingUuid === null)).toBe(true);
    // The editor is on the copy now, and says so, with the way back
    // (re-critique after #296: it switched notes silently).
    for (let i = 0; i < 4; i++) await tick();
    flushSync();
    const copied = target.querySelector('[data-testid="conflict-copy-notice"]')!;
    expect(copied.textContent).toContain("Copied to a new note. The original still isn't synced.");
    expect(Array.from(copied.querySelectorAll('button')).map((b) => b.textContent?.trim())).toContain('Open original');
  });

  it('a note Apple has locked says the last edit is only here, and offers a copy, never a retry', async () => {
    const blocked = { ...NOTE, push_blocked_reason: REASON, push_blocked_by_remote: true };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    // The lock is only left by a refused push of an edit made here: that edit
    // is on this device alone, so the notice says so first and offers the
    // copy that keeps it (re-critique after #292). Never a retry.
    const n = notice()!;
    expect(n.textContent).toContain('Read-only in Jodd. Your last edit is saved on this device only.');
    expect(n.textContent).toContain('Edit it in Apple Notes.');
    expect(n.querySelector('details')!.textContent).toContain('open it in Apple Notes to edit');
    expect(Array.from(n.querySelectorAll('button')).map((b) => b.textContent?.trim())).toEqual(['Copy to a new note']);
  });

  it('a note syncing normally has no blocked notice', async () => {
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    await render();
    expect(notice()).toBeNull();
  });
});
