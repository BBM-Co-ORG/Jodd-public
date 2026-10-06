// @vitest-environment jsdom
//
// Regression test for the "Re-extract" context-menu action.
//
// The menu unmounts itself (onClose() → parent sets `menuNote = null`)
// before its async work runs. Any field of `note` read AFTER that point
// resolves against a null prop and throws
// `TypeError: null is not an object (evaluating 'note().uuid')`
// (minified: `P().uuid`). moveTo() and linkIntoWiki() already snapshot
// their fields before onClose() for exactly this reason; reExtractLessons()
// did not, so the action failed instantly on every invocation and never
// reached the backend.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { notes, selectedFolder } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';

const SOURCE_NOTE: Note = {
  uuid: 'uuid-under-test',
  id: 'msg-1',
  account_id: 'localfs:testlocaljoddfolder',
  title: 'An extract note',
  body_html:
    '<h2>Lessons</h2><p>body</p>' +
    '<details><summary>Source (verbatim)</summary><pre>raw source text</pre></details>',
  date: '2026-07-28T00:00:00Z',
  label: 'Notes/Research',
} as Note;

function findButton(root: HTMLElement, label: string): HTMLButtonElement {
  const match = Array.from(root.querySelectorAll('button')).find((b) =>
    (b.textContent ?? '').includes(label),
  );
  if (!match) throw new Error(`no "${label}" button in menu`);
  return match as HTMLButtonElement;
}

describe('NoteContextMenu → Re-extract', () => {
  beforeEach(() => {
    invoke.mockReset();
    // onMount fetches folders for every signed-in account; no accounts are
    // seeded here, but keep a safe default for any incidental call.
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: 'x', label: 'Notes/Research' });
      return Promise.resolve([]);
    });
    vi.stubGlobal('alert', vi.fn());
  });

  it('invokes the backend with the note uuid after the menu closes', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();

    findButton(target, 'Re-extract').click();
    flushSync();
    await tick();
    await Promise.resolve();

    const call = invoke.mock.calls.find((c) => c[0] === 're_extract_note');
    expect(call, `re_extract_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({
      accountId: 'localfs:testlocaljoddfolder',
      uuid: 'uuid-under-test',
    });
    expect(call![1].requestId).toBeTruthy();
    expect(globalThis.alert).not.toHaveBeenCalled();

    unmount(host);
  });

  // Re-extract files the new note BESIDE its source (extract filing,
  // 2026-09-15) and the backend returns where. The frontend must repaint
  // that folder — explicitly, because selectedFolder.set() is a no-op when
  // the user is already viewing it (Svelte's safe_not_equal), which is the
  // normal case: Re-extract is invoked from a note in the open folder.
  it('repaints the folder the backend filed the new note into', async () => {
    const NEW_NOTE = {
      uuid: 'freshly-extracted',
      id: '',
      account_id: 'localfs:testlocaljoddfolder',
      title: 'Freshly extracted note',
      body_html: '<p>new</p>',
      date: '2026-07-28T09:35:00Z',
      label: 'Notes/Research',
    } as Note;

    notes.set([SOURCE_NOTE]);
    selectedFolder.set('Notes/Research');

    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: NEW_NOTE.uuid, label: 'Notes/Research' });
      if (cmd === 'list_cached_notes_in_folder') return Promise.resolve([SOURCE_NOTE, NEW_NOTE]);
      if (cmd === 'list_note_tags') return Promise.resolve([{ uuid: NEW_NOTE.uuid, tag: 'tauri' }]);
      return Promise.resolve([]);
    });

    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();

    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
    await tick();

    const paint = invoke.mock.calls.find((c) => c[0] === 'list_cached_notes_in_folder');
    expect(paint?.[1]).toMatchObject({ path: 'Notes/Research' });
    expect(JSON.stringify(invoke.mock.calls)).not.toContain('__Extracts__');
    expect(get(selectedFolder)).toBe('Notes/Research');

    const painted = get(notes);
    expect(painted.find((n) => n.uuid === NEW_NOTE.uuid)).toBeTruthy();
    expect(painted.find((n) => n.uuid === SOURCE_NOTE.uuid)).toBeTruthy();
    expect(globalThis.alert).not.toHaveBeenCalled();

    unmount(host);
  });
});
