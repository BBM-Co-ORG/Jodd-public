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
import { notes, selectedFolder, selectedSmartFolder, selectedTags, currentAccount, selectedNote } from '../stores/notes';
import { AI_CONSENT_NEEDED, answerConsent, consentRequest } from '../aiConsent';
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

  // PR #140 review: Re-extract from a Smart Folder view (Extracts is the
  // natural place to find a note to re-extract) set selectedFolder but left
  // the Smart Folder, which NoteList shows first.
  it('leaves a Smart Folder for the folder the new note was filed in', async () => {
    selectedFolder.set('Notes');
    selectedSmartFolder.set({ account: SOURCE_NOTE.account_id!, kind: 'extracts' });
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: 'x', label: 'Notes/Research' });
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

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes/Research');

    unmount(host);
  });

  it('leaves a tag filter for the folder the new note was filed in', async () => {
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set(['research']));
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: 'x', label: 'Notes/Research' });
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

    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes/Research');

    unmount(host);
  });

  // PR #145 review: Re-extract runs in the background after the menu closes.
  // A view the user opened while it ran must survive its completion.
  it('does not pull the user out of a view they opened while it ran', async () => {
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    let finish!: (v: { uuid: string; label: string }) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return new Promise((r) => (finish = r));
      return Promise.resolve([]);
    });

    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 5; i++) await Promise.resolve();

    selectedSmartFolder.set({ account: SOURCE_NOTE.account_id!, kind: 'unreviewed' });
    finish({ uuid: 'x', label: 'Notes/Research' });
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();

    expect(get(selectedSmartFolder)).toEqual({ account: SOURCE_NOTE.account_id!, kind: 'unreviewed' });
    expect(get(selectedFolder)).toBe('Notes');

    unmount(host);
  });

  // PR #145 review: the note can be in another account than the one on screen
  // (a cross-account search or tag result). Its folder path means nothing in
  // the current account, and the new note was never selected either.
  it("opens the new note's folder in the note's own account and selects the note", async () => {
    const NEW_NOTE = {
      uuid: 'freshly-extracted',
      id: '',
      account_id: SOURCE_NOTE.account_id,
      title: 'Freshly extracted note',
      body_html: '<p>new</p>',
      date: '2026-07-28T09:35:00Z',
      label: 'Notes/Research',
    } as Note;
    currentAccount.set('gmail:other@example.com');
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    selectedNote.set(SOURCE_NOTE);
    notes.set([SOURCE_NOTE]);
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: NEW_NOTE.uuid, label: 'Notes/Research' });
      if (cmd === 'list_cached_notes_in_folder') return Promise.resolve([SOURCE_NOTE, NEW_NOTE]);
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

    expect(get(currentAccount)).toBe(SOURCE_NOTE.account_id);
    expect(get(selectedFolder)).toBe('Notes/Research');
    expect(get(selectedNote)?.uuid).toBe(NEW_NOTE.uuid);

    unmount(host);
  });

  // Selecting is navigation too: a note the user opened while the cache paint
  // ran stays open.
  it('does not replace a note the user opened while the cache paint ran', async () => {
    const OTHER = { ...SOURCE_NOTE, uuid: 'opened-meanwhile', title: 'Opened meanwhile' } as Note;
    currentAccount.set(SOURCE_NOTE.account_id!);
    selectedFolder.set('Notes/Research');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    selectedNote.set(SOURCE_NOTE);
    notes.set([SOURCE_NOTE, OTHER]);
    let paint!: (v: Note[]) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: 'freshly-extracted', label: 'Notes/Research' });
      if (cmd === 'list_cached_notes_in_folder') return new Promise((r) => (paint = r));
      return Promise.resolve([]);
    });

    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 5; i++) await Promise.resolve();

    selectedNote.set(OTHER);
    paint([SOURCE_NOTE, OTHER, { ...SOURCE_NOTE, uuid: 'freshly-extracted', title: 'New' } as Note]);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();

    expect(get(selectedNote)?.uuid).toBe('opened-meanwhile');

    unmount(host);
  });
  // App's own folder paint re-reads the open note (reconcileSelection) and may
  // swap in a fresh object for it. That is the same note, not a new choice.
  it('still selects the new note when the open note was merely refreshed', async () => {
    currentAccount.set(SOURCE_NOTE.account_id!);
    selectedFolder.set('Notes/Research');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    selectedNote.set(SOURCE_NOTE);
    notes.set([SOURCE_NOTE]);
    let paint!: (v: Note[]) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 're_extract_note') return Promise.resolve({ uuid: 'freshly-extracted', label: 'Notes/Research' });
      if (cmd === 'list_cached_notes_in_folder') return new Promise((r) => (paint = r));
      return Promise.resolve([]);
    });

    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 5; i++) await Promise.resolve();

    selectedNote.set({ ...SOURCE_NOTE, local_version: 2 } as Note);
    paint([SOURCE_NOTE, { ...SOURCE_NOTE, uuid: 'freshly-extracted', title: 'New' } as Note]);
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();

    expect(get(selectedNote)?.uuid).toBe('freshly-extracted');

    unmount(host);
  });

  // Privacy PR3: the menu has already closed itself, so the consent question
  // goes to the app-level in-DOM prompt — never native confirm(), never the
  // generic "Re-extract failed" alert.
  it('a consent refusal opens the in-app consent prompt instead of alerting', async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === 're_extract_note' ? Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`) : Promise.resolve([]));
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
    expect(get(consentRequest)?.accountId).toBe('localfs:testlocaljoddfolder');
    expect(globalThis.alert).not.toHaveBeenCalled();
    answerConsent(false);
    for (let i = 0; i < 5; i++) await Promise.resolve();
    expect(invoke.mock.calls.some((c) => c[0] === 'allow_ai_for_account')).toBe(false);
    unmount(host);
  });

  it('any other failure still alerts and opens no consent prompt', async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === 're_extract_note' ? Promise.reject('boom') : Promise.resolve([]));
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    findButton(target, 'Re-extract').click();
    for (let i = 0; i < 10; i++) await Promise.resolve();
    flushSync();
    expect(globalThis.alert).toHaveBeenCalledWith('Re-extract failed: boom');
    expect(get(consentRequest)).toBeNull();
    unmount(host);
  });
});
