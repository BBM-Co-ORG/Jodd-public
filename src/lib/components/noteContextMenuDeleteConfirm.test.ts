// @vitest-environment jsdom
//
// End-to-end check that the permanent-delete confirm dialog actually gates
// the invoke() call, not just that needsPermanentDeleteConfirm() returns the
// right boolean in isolation (confirmPermanentDelete.test.ts covers that).
// Uses the same ContextMenuHost mount/unmount contract as reExtract.test.ts
// so onClose() really does null the menu prop, the same as NoteList.svelte.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { setAccountCapabilities } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';

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

function findButton(root: HTMLElement, label: string): HTMLButtonElement {
  const match = Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent ?? '').includes(label),
  );
  if (!match) {
    const seen = Array.from(root.querySelectorAll('button')).map((b) => b.textContent);
    throw new Error(`no "${label}" button in menu; found: ${JSON.stringify(seen)}`);
  }
  return match as HTMLButtonElement;
}

describe('NoteContextMenu → permanent-delete confirm (has_trash: false)', () => {
  beforeEach(() => {
    invoke.mockReset();
    // onMount fetches folders for every signed-in account; no accounts are
    // seeded here, but keep a safe default for any incidental call.
    invoke.mockResolvedValue([]);
    setAccountCapabilities(ACCOUNT_ID, { has_trash: false });
  });

  it('does not call delete_note when the user cancels', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();

    findButton(target, 'Delete').click();
    flushSync();
    await tick();

    // The dialog names the note and says the delete is permanent.
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
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();

    findButton(target, 'Delete').click();
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

describe('NoteContextMenu → batch permanent-delete confirm (has_trash: false)', () => {
  // Task 10b: deleteBatch() reached the same hard delete as the single-note
  // path with no warning at all — the most dangerous of the three delete
  // surfaces, since one click destroys every selected note. Same fixture,
  // now with a multi-note `selection` so isMulti is true and the menu
  // renders the batch "Delete N notes" item instead of single "Delete".
  const NOTE_A: Note = { ...SOURCE_NOTE, uuid: 'uuid-a', id: 'msg-a', title: 'Note A' };
  const NOTE_B: Note = { ...SOURCE_NOTE, uuid: 'uuid-b', id: 'msg-b', title: 'Note B' };
  const NOTE_C: Note = { ...SOURCE_NOTE, uuid: 'uuid-c', id: 'msg-c', title: 'Note C' };
  const BATCH: Note[] = [NOTE_A, NOTE_B, NOTE_C];

  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
    setAccountCapabilities(ACCOUNT_ID, { has_trash: false });
  });

  it('does not delete the batch when the user cancels', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: NOTE_A, selection: BATCH } });
    flushSync();
    await tick();

    findButton(target, 'Delete 3 notes').click();
    flushSync();
    await tick();

    // The count is the fact that makes the warning worth reading — assert
    // it's actually in the dialog, not just that some dialog appeared.
    expect(target.textContent).toMatch(/permanent/i);
    expect(target.textContent).toMatch(/no way to undo/i);
    expect(target.textContent).toContain('3 notes');

    findButton(target, 'Cancel').click();
    flushSync();
    await tick();
    await Promise.resolve();

    expect(invoke.mock.calls.find((c) => c[0] === 'delete_notes_batch')).toBeUndefined();

    unmount(host);
  });

  it('deletes the batch when the user confirms', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: NOTE_A, selection: BATCH } });
    flushSync();
    await tick();

    findButton(target, 'Delete 3 notes').click();
    flushSync();
    await tick();

    findButton(target, 'Delete permanently').click();
    flushSync();
    await tick();
    await Promise.resolve();

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_notes_batch');
    expect(call, `delete_notes_batch never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({
      accountId: ACCOUNT_ID,
      uuids: ['uuid-a', 'uuid-b', 'uuid-c'],
    });

    unmount(host);
  });
});

describe('NoteContextMenu → Delete (has_trash: true, unchanged one-click behavior)', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
    setAccountCapabilities(ACCOUNT_ID, { has_trash: true });
  });

  it('calls delete_note directly with no confirm dialog', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();

    findButton(target, 'Delete').click();
    flushSync();
    await tick();
    await Promise.resolve();

    // No confirm dialog text should have appeared — the one-click Gmail/
    // LocalFs path (a real Trash exists) must stay exactly as it was.
    expect(target.textContent).not.toMatch(/permanent/i);

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_note');
    expect(call, `delete_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({ accountId: ACCOUNT_ID, uuid: 'uuid-under-test' });

    unmount(host);
  });
});
