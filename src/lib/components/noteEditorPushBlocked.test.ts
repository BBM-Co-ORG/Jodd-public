// @vitest-environment jsdom
//
// A permanently-refused push (gotcha #14) used to show its reason only as a
// hover tooltip on "Sync blocked" — invisible on Android, and easy to miss on
// desktop. On 2026-08-27 an iCloud note was refused five times in two minutes
// ("…open it in Apple Notes to edit"), each refusal right after another edit,
// because nothing on screen said editing here could not work.
//
// Two rules, asserted on the mounted component (gotcha #28):
//   - the reason is visible text whenever the note is blocked;
//   - a block by the remote's own document (`push_blocked_by_remote`) locks
//     the note, because no edit made here can get past it. Any other block
//     leaves the editor open — an edit or a move may be the fix.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { accounts, selectedNote, notes } from '../stores/notes';
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

const banner = () => target.querySelector('.push-blocked-banner');
const body = () => target.querySelector('.editor-body') as HTMLElement;
const titleInput = () => target.querySelector('.title-input') as HTMLInputElement;

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

  it('a block by the remote document shows the reason as text and locks the note', async () => {
    const blocked = { ...NOTE, push_blocked_reason: REASON, push_blocked_by_remote: true };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(banner()?.textContent).toContain('open it in Apple Notes to edit');
    expect(body().getAttribute('contenteditable')).toBe('false');
    expect(titleInput().readOnly).toBe(true);
    expect(target.querySelector('.tag-input')).toBeNull();
  });

  it('an ordinary block shows the reason as text but leaves the editor open', async () => {
    const blocked = {
      ...NOTE,
      push_blocked_reason: "permanent: no Exchange folder id for 'Notes'",
      push_blocked_by_remote: false,
    };
    notes.set([blocked]);
    selectedNote.set(blocked);
    await render();

    expect(banner()?.textContent).toContain("no Exchange folder id for 'Notes'");
    expect(body().getAttribute('contenteditable')).toBe('true');
    expect(titleInput().readOnly).toBe(false);
  });

  it('a note syncing normally has no banner and is editable', async () => {
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    await render();

    expect(banner()).toBeNull();
    expect(body().getAttribute('contenteditable')).toBe('true');
  });

  it('"Try again" re-arms this note only, not every blocked note in the account', async () => {
    const blocked = {
      ...NOTE,
      push_blocked_reason: "permanent: no Exchange folder id for 'Notes'",
      push_blocked_by_remote: false,
    };
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

    (target.querySelector('.retry-sync-btn') as HTMLButtonElement).click();
    flushSync();
    await tick();

    expect(invoke).toHaveBeenCalledWith('retry_blocked_push', { accountId: ACCOUNT.id, uuid: NOTE.uuid });
    expect(invoke).not.toHaveBeenCalledWith('retry_blocked_pushes', expect.anything());
    const after = get(notes);
    expect(after.find((n) => n.uuid === NOTE.uuid)?.push_blocked_reason).toBeNull();
    expect(after.find((n) => n.uuid === 'other-note')?.push_blocked_reason).toBe(REASON);
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

    expect(banner()).toBeNull();
    expect(body().getAttribute('contenteditable')).toBe('true');
  });
});
