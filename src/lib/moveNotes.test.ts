// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { notes, selectedNote } from './stores/notes';
import type { Note } from './types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { moveNoteOptimistic } from './moveNotes';

const NOTE: Note = {
  uuid: 'u1', id: 'm1', account_id: 'gmail:a@b.com', title: 'T',
  body_html: '<p>x</p>', date: '2026-09-15T00:00:00Z', label: 'Notes/Inbox',
} as Note;

const REQ = { accountId: 'gmail:a@b.com', uuid: 'u1', fromLabel: 'Notes/Inbox', toLabel: 'Notes/Trading', remoteId: 'm1' };

describe('moveNoteOptimistic', () => {
  beforeEach(() => {
    invoke.mockReset();
    notes.set([NOTE]);
    selectedNote.set(NOTE);
  });

  it('relabels the store before the backend answers', () => {
    let release!: () => void;
    invoke.mockImplementation(() => new Promise<number>((r) => { release = () => r(1); }));
    const pending = moveNoteOptimistic(REQ);
    // No await yet: local-first means the label moved already.
    expect(get(notes)[0].label).toBe('Notes/Trading');
    expect(get(selectedNote)?.label).toBe('Notes/Trading');
    expect(invoke).toHaveBeenCalledWith('move_notes_batch', {
      accountId: 'gmail:a@b.com', uuids: ['u1'], targetLabel: 'Notes/Trading',
    });
    release();
    return pending.then((r) => expect(r.ok).toBe(true));
  });

  it('rolls the label back when the move fails', async () => {
    invoke.mockRejectedValue('refused');
    const r = await moveNoteOptimistic(REQ);
    expect(r).toEqual({ ok: false, error: 'refused' });
    expect(get(notes)[0].label).toBe('Notes/Inbox');
    expect(get(selectedNote)?.label).toBe('Notes/Inbox');
  });
});
