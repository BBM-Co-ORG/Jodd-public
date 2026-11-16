// @vitest-environment jsdom
//
// "Suggest folder" is the EXPLICIT caller of suggest_note_folder, so unlike
// the automatic post-Extract call it must say what happened — including that
// no provider is configured (spec "Suggestion" table).
//
// What the action says and records for each outcome is asserted on the
// action itself, in controllers/noteActions.test.ts (the cases moved there).
// This file keeps the menu wiring: the item renders, a click reaches the
// action with this note after the menu has closed, and the action runs to
// its outcome message.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { folderSuggestions, selectedNote } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';

const NOTE: Note = {
  uuid: 'uuid-under-test', id: 'msg-1', account_id: 'gmail:a@b.com', title: 'A note',
  body_html: '<p>body</p>', date: '2026-09-15T00:00:00Z', label: 'Notes/Inbox',
} as Note;

function findButton(root: HTMLElement, label: string): HTMLButtonElement {
  const match = Array.from(root.querySelectorAll('button')).find((b) => (b.textContent ?? '').includes(label));
  if (!match) throw new Error(`no "${label}" button in menu`);
  return match as HTMLButtonElement;
}

async function clickSuggest(answer: () => Promise<unknown>) {
  invoke.mockImplementation((cmd: string) => (cmd === 'suggest_note_folder' ? answer() : Promise.resolve([])));
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(ContextMenuHost, { target, props: { note: NOTE } });
  flushSync();
  await tick();
  findButton(target, 'Suggest a folder').click();
  for (let i = 0; i < 10; i++) await Promise.resolve();
  flushSync();
  const call = invoke.mock.calls.find((c) => c[0] === 'suggest_note_folder');
  expect(call?.[1]).toMatchObject({ accountId: 'gmail:a@b.com', uuid: 'uuid-under-test' });
  unmount(host);
  target.remove();
}

describe('NoteContextMenu → Suggest folder', () => {
  beforeEach(() => {
    invoke.mockReset();
    folderSuggestions.set({});
    selectedNote.set(null);
    vi.stubGlobal('alert', vi.fn());
  });
  afterEach(() => vi.unstubAllGlobals());

  it('says when nothing better was found', async () => {
    await clickSuggest(() => Promise.resolve({ kind: 'none_fits' }));
    expect(globalThis.alert).toHaveBeenCalledWith('No better folder found');
  });
});
