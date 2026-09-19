// @vitest-environment jsdom
//
// "Suggest folder" is the EXPLICIT caller of suggest_note_folder, so unlike
// the automatic post-Extract call it must say what happened — including that
// no provider is configured (spec "Suggestion" table).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { folderSuggestions, folderSuggestionKey, selectedNote } from '../stores/notes';
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
  findButton(target, 'Suggest folder').click();
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

  it('says when no LLM provider is configured', async () => {
    await clickSuggest(() => Promise.reject('provider not configured: no LLM provider configured for this account'));
    expect(globalThis.alert).toHaveBeenCalledWith('No LLM provider is configured for this account');
  });

  it('says when nothing better was found', async () => {
    await clickSuggest(() => Promise.resolve({ kind: 'none_fits' }));
    expect(globalThis.alert).toHaveBeenCalledWith('No better folder found');
  });

  it('feeds the editor chip, silently, when the note is open', async () => {
    selectedNote.set(NOTE);
    await clickSuggest(() => Promise.resolve({ kind: 'suggested', uuid: 'uuid-under-test', path: 'Notes/Trading', reason: 'r' }));
    expect(get(folderSuggestions)[folderSuggestionKey('gmail:a@b.com', 'uuid-under-test')])
      .toEqual({ path: 'Notes/Trading', reason: 'r' });
    expect(globalThis.alert).not.toHaveBeenCalled();
  });

  it('treats the note as open when the outcome names a rekeyed uuid, and records under both', async () => {
    // Gotcha #16: the editor holds the uuid the menu asked about; the backend
    // rekeyed the row mid-call, so the outcome carries a different one.
    selectedNote.set(NOTE);
    await clickSuggest(() => Promise.resolve({ kind: 'suggested', uuid: '<rekeyed@exchange>', path: 'Notes/Trading', reason: null }));
    expect(globalThis.alert).not.toHaveBeenCalled();
    const m = get(folderSuggestions);
    expect(m[folderSuggestionKey('gmail:a@b.com', 'uuid-under-test')]).toEqual({ path: 'Notes/Trading', reason: null });
    expect(m[folderSuggestionKey('gmail:a@b.com', '<rekeyed@exchange>')]).toEqual({ path: 'Notes/Trading', reason: null });
  });
});
