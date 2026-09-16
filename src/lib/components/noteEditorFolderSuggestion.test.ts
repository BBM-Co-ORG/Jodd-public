// @vitest-environment jsdom
//
// The chip's visibility is an identity comparison: "is there a proposal for
// the note that is open NOW". Gotcha #28 — an identity and what is compared
// against it must change in the same reactive turn. Pinned by MOUNTING the
// editor: a pure-function test passes with the split present.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes, folderSuggestions, setFolderSuggestion, folderSuggestionKey } from '../stores/notes';
import { recordFolderSuggestion } from '../folderSuggestion';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';

function noteOf(uuid: string, label: string): Note {
  return {
    uuid, id: `msg-${uuid}`, account_id: ACCOUNT_ID, title: `Title ${uuid}`,
    body_html: `<div>Title ${uuid}</div><div>body</div>`, date: '2026-09-15T00:00:00Z', label,
  } as Note;
}

const A = noteOf('uuid-a', 'Notes/Inbox');
const B = noteOf('uuid-b', 'Notes/Inbox');

function chip(host: HTMLElement): HTMLElement | null {
  return host.querySelector('[data-testid="folder-suggestion"]');
}

function button(host: HTMLElement, label: string): HTMLButtonElement {
  const b = Array.from(chip(host)?.querySelectorAll('button') ?? []).find((x) => x.textContent?.trim() === label);
  if (!b) throw new Error(`no "${label}" button in the chip`);
  return b as HTMLButtonElement;
}

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('folder suggestion chip', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'list_folders') return Promise.resolve(['Notes', 'Notes/Inbox', 'Notes/Trading']);
      if (cmd === 'move_notes_batch') return Promise.resolve(1);
      return Promise.resolve([]);
    });
    folderSuggestions.set({});
    notes.set([A, B]);
    selectedNote.set(A);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await settle();
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    folderSuggestions.set({});
    vi.clearAllMocks();
  });

  it('shows a proposal for the open note', async () => {
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: 'About trading.' });
    flushSync();
    expect(chip(host)?.textContent).toContain('Notes/Trading');
    expect(chip(host)?.getAttribute('title')).toBe('About trading.');
  });

  it("hides A's proposal in the same reactive pass that opens B, and shows it again on A", async () => {
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: null });
    flushSync();
    expect(chip(host)).not.toBeNull();

    selectedNote.set(B);
    flushSync(); // deliberately no await: the gap is what gotcha #28 is about
    expect(chip(host)).toBeNull();

    selectedNote.set(A);
    flushSync();
    expect(chip(host)).not.toBeNull();
  });

  it('shows nothing when the proposal is the folder the note is already in', async () => {
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Inbox', reason: null });
    flushSync();
    expect(chip(host)).toBeNull();
  });

  it('Keep here drops the proposal', async () => {
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: null });
    flushSync();
    button(host, 'Keep here').click();
    flushSync();
    expect(chip(host)).toBeNull();
    expect(get(folderSuggestions)[folderSuggestionKey(ACCOUNT_ID, A.uuid)]).toBeUndefined();
  });

  it('shows a proposal recorded while the note was being rekeyed, and Keep here drops both keys', async () => {
    // Gotcha #16: the editor still holds the pre-rekey uuid (A) while the
    // outcome names the live one. The proposal must reach the chip anyway.
    recordFolderSuggestion(ACCOUNT_ID, A.uuid, { kind: 'suggested', uuid: 'uuid-rekeyed', path: 'Notes/Trading', reason: null });
    flushSync();
    expect(chip(host)?.textContent).toContain('Notes/Trading');

    button(host, 'Keep here').click();
    flushSync();
    expect(chip(host)).toBeNull();
    expect(get(folderSuggestions)[folderSuggestionKey(ACCOUNT_ID, A.uuid)]).toBeUndefined();
    expect(get(folderSuggestions)[folderSuggestionKey(ACCOUNT_ID, 'uuid-rekeyed')]).toBeUndefined();
  });

  it('Move relabels the note and clears the proposal', async () => {
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: null });
    await settle(); // let the chip's folder list load
    button(host, 'Move').click();
    await settle();
    expect(get(selectedNote)?.label).toBe('Notes/Trading');
    expect(invoke).toHaveBeenCalledWith('move_notes_batch', {
      accountId: ACCOUNT_ID, uuids: [A.uuid], targetLabel: 'Notes/Trading',
    });
    expect(get(folderSuggestions)[folderSuggestionKey(ACCOUNT_ID, A.uuid)]).toBeUndefined();
  });

  it('a failed move rolls the note back and leaves the chip up', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'list_folders') return Promise.resolve(['Notes', 'Notes/Inbox', 'Notes/Trading']);
      if (cmd === 'move_notes_batch') return Promise.reject('refused');
      return Promise.resolve([]);
    });
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: null });
    await settle();
    button(host, 'Move').click();
    await settle();
    expect(get(selectedNote)?.label).toBe('Notes/Inbox');
    expect(chip(host)).not.toBeNull();
  });

  it('a proposed folder that has disappeared removes the chip instead of moving', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'list_folders') return Promise.resolve(['Notes', 'Notes/Inbox']);
      return Promise.resolve([]);
    });
    setFolderSuggestion(ACCOUNT_ID, A.uuid, { path: 'Notes/Trading', reason: null });
    await settle();
    button(host, 'Move').click();
    await settle();
    expect(invoke).not.toHaveBeenCalledWith('move_notes_batch', expect.anything());
    expect(chip(host)).toBeNull();
    expect(get(selectedNote)?.label).toBe('Notes/Inbox');
  });
});
