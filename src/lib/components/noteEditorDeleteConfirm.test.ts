// @vitest-environment jsdom
//
// Editor-path equivalent of noteContextMenuDeleteConfirm.test.ts (Task 10b):
// the trash-icon delete in NoteEditor.svelte reached the same hard delete as
// the context-menu single-note path with no warning at all. Same assertion
// shape — on invoke.mock.calls, not on a dialog function having been called —
// so this proves the delete command itself was (or wasn't) issued.
//
// Unlike NoteContextMenu, NoteEditor takes no props: it reads $selectedNote
// directly, so the fixture is just the store + the component, no host wrapper
// needed.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes, setAccountCapabilities } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'microsoft:test@example.com';

const SOURCE_NOTE: Note = {
  uuid: 'uuid-under-test',
  id: 'msg-1',
  account_id: ACCOUNT_ID,
  title: 'A note with no undo',
  body_html: '<p>body</p>',
  date: '2026-08-14T00:00:00Z',
  label: 'Notes',
} as Note;

function findButton(root: HTMLElement, titleOrLabel: string): HTMLButtonElement {
  const match = Array.from(root.querySelectorAll('button')).find(
    (b) => b.getAttribute('title') === titleOrLabel || (b.textContent ?? '').includes(titleOrLabel),
  );
  if (!match) {
    const seen = Array.from(root.querySelectorAll('button')).map(
      (b) => b.getAttribute('title') ?? b.textContent,
    );
    throw new Error(`no "${titleOrLabel}" button; found: ${JSON.stringify(seen)}`);
  }
  return match as HTMLButtonElement;
}

// NoteEditor fetches more than delete_note on mount (note_connections,
// note_citations, get_note_attachments) — unlike NoteContextMenu's tests,
// a blanket `mockResolvedValue([])` breaks note_connections, whose caller
// destructures `{ outgoing, backlinks }` and throws on an array. Give each
// command a shape it can actually consume.
function stubInvoke() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
}

describe('NoteEditor → permanent-delete confirm (has_trash: false)', () => {
  beforeEach(() => {
    invoke.mockReset();
    stubInvoke();
    notes.set([SOURCE_NOTE]);
    selectedNote.set(SOURCE_NOTE);
    setAccountCapabilities(ACCOUNT_ID, { has_trash: false });
  });

  it('does not call delete_note when the user cancels', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();

    findButton(target, 'Delete note').click();
    flushSync();
    await tick();

    expect(target.textContent).toContain('A note with no undo');
    expect(target.textContent).toMatch(/permanent/i);
    expect(target.textContent).toMatch(/no way to undo/i);

    findButton(target, 'Cancel').click();
    flushSync();
    await tick();
    await Promise.resolve();

    expect(invoke.mock.calls.find((c) => c[0] === 'delete_note')).toBeUndefined();

    unmount(host);
  });

  it('calls delete_note when the user confirms', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();

    findButton(target, 'Delete note').click();
    flushSync();
    await tick();

    findButton(target, 'Delete permanently').click();
    flushSync();
    await tick();
    await Promise.resolve();

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_note');
    expect(call, `delete_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({ accountId: ACCOUNT_ID, uuid: 'uuid-under-test' });

    unmount(host);
  });
});

describe('NoteEditor → trash icon (has_trash: true, unchanged one-click behavior)', () => {
  beforeEach(() => {
    invoke.mockReset();
    stubInvoke();
    notes.set([SOURCE_NOTE]);
    selectedNote.set(SOURCE_NOTE);
    setAccountCapabilities(ACCOUNT_ID, { has_trash: true });
  });

  it('calls delete_note directly with no confirm dialog', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();

    findButton(target, 'Delete note').click();
    flushSync();
    await tick();
    await Promise.resolve();

    expect(target.textContent).not.toMatch(/permanent/i);

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_note');
    expect(call, `delete_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({ accountId: ACCOUNT_ID, uuid: 'uuid-under-test' });

    unmount(host);
  });
});
