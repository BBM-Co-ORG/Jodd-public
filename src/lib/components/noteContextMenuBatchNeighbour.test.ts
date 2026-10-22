// @vitest-environment jsdom
import './__fixtures__/dialogStub';
//
// Spec 2026-10-20 §5.3, batch form: deleting a multi-select that holds the
// open note opens the first note left below the deleted block (else the
// nearest above it) — in the step that removes the rows — and a refused
// batch reopens the note that was open.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import {
  setAccountCapabilities, notes, selectedNote, currentAccount, selectedFolder, selectedSmartFolder,
  selectedTags, searchQuery, pendingDeletedNoteKeys,
} from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';

const ACCT = 'gmail:a@b.com';
// List order (newest first): a, b, c, d.
const mk = (u: string, day: number): Note => ({
  uuid: u, id: `m-${u}`, account_id: ACCT, title: u.toUpperCase(), label: 'Notes',
  body_html: `<html><head></head><body><div>${u}</div></body></html>`, date: `2026-10-0${day}T00:00:00Z`,
} as Note);
const [A, B, C, D] = [mk('a', 9), mk('b', 8), mk('c', 7), mk('d', 6)];

async function deleteBatch(selection: Note[]) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(ContextMenuHost, { target, props: { note: selection[0], selection } });
  flushSync();
  await tick();
  const item = Array.from(target.querySelectorAll('button')).find((b) => /Delete \d+ notes/.test(b.textContent ?? ''));
  expect(item).toBeTruthy();
  item!.click();
  flushSync();
  for (let i = 0; i < 4; i++) await tick();
  unmount(host);
  target.remove();
}

describe('batch delete of the open note', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
    setAccountCapabilities(ACCT, { has_trash: true });
    currentAccount.set(ACCT);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    searchQuery.set('');
    pendingDeletedNoteKeys.set(new Set());
    notes.set([A, B, C, D]);
  });

  it('opens the first note below the deleted block, in the step that removes it', async () => {
    selectedNote.set(B);
    const seen: (string | null)[] = [];
    const stop = selectedNote.subscribe((n) => seen.push(n ? n.uuid : null));
    await deleteBatch([B, C]);
    stop();
    expect(seen).toEqual(['b', 'd']);
    expect(invoke).toHaveBeenCalledWith('delete_notes_batch', { accountId: ACCT, uuids: ['b', 'c'] });
  });

  it('with nothing below, the nearest note above', async () => {
    selectedNote.set(D);
    await deleteBatch([C, D]);
    expect(get(selectedNote)?.uuid).toBe('b');
  });

  it('an open note outside the batch stays open', async () => {
    selectedNote.set(A);
    await deleteBatch([C, D]);
    expect(get(selectedNote)?.uuid).toBe('a');
  });

  it('a refused batch reopens the note that was open', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'delete_notes_batch') throw new Error('refused');
      return [];
    });
    selectedNote.set(B);
    await deleteBatch([B, C]);
    expect(get(selectedNote)?.uuid).toBe('b');
  });
});
