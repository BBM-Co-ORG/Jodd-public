// @vitest-environment jsdom
//
// Copy to / Move to another account (spec 2026-11-13-cross-account-move-copy):
// the preview decides whether to ask; a move hides its originals in the step
// that sends the write and puts back what the write refused; the open note
// follows its copy and its account becomes current.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const moveNoteTo = vi.fn(async () => {});
vi.mock('./noteActions', () => ({ moveNoteTo: (...a: unknown[]) => moveNoteTo(...(a as [])) }));
vi.mock('../editorFlush', () => ({ flushEditors: vi.fn(async () => {}) }));

import { calledOffText, confirmLines, lossText, transferNotes, transferQuestion, type TransferReport } from './noteTransfer';
import { accounts, currentAccount, error, notes, pendingDeletedNoteKeys, selectedNote } from '../stores/notes';
import type { Note } from '../types';

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '', backend_kind: 'gmail', status: 'active' };
const B = { id: 'icloud:c@icloud.com', email: 'c@icloud.com', added_at: '', backend_kind: 'icloud', status: 'active' };
const M = { id: 'microsoft:m@live.com', email: 'm@live.com', added_at: '', backend_kind: 'microsoft', status: 'active' };
const note = (uuid: string, account: string, title = uuid): Note =>
  ({ uuid, id: `m-${uuid}`, account_id: account, title, body_html: '<p>x</p>', date: '', label: 'Notes' }) as Note;

function planned(n: Note, extra: Partial<TransferReport['notes'][number]> = {}) {
  return { source: { accountId: n.account_id!, uuid: n.uuid }, title: n.title, refused: null, losses: [], ...extra };
}

beforeEach(() => {
  invoke.mockReset();
  moveNoteTo.mockClear();
  accounts.set([A, B, M] as never);
  currentAccount.set(A.id);
  notes.set([]);
  selectedNote.set(null);
  error.set(null);
  transferQuestion.set(null);
  pendingDeletedNoteKeys.set(new Set());
});

describe('noteTransfer', () => {
  it('a move inside one account is the old relabel, with no transfer', async () => {
    const n = note('S1', A.id);
    await transferNotes([n], A.id, 'Notes/Work', 'move');
    expect(moveNoteTo).toHaveBeenCalledWith(n, 'Notes/Work');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('a move with nothing lost goes on the click: hidden, written, and the open note follows its copy', async () => {
    const n = note('S1', A.id, 'Trip');
    notes.set([n]);
    selectedNote.set(n);
    let hiddenWhenSent: boolean | undefined;
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'preview_note_transfer') return { notes: [planned(n)] };
      hiddenWhenSent = !get(notes).some((x) => x.uuid === 'S1');
      return { notes: [planned(n, { targetUuid: 'c-1' })] };
    });
    await transferNotes([n], B.id, 'Notes/Trips', 'move');
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(['preview_note_transfer', 'transfer_notes']);
    expect(invoke.mock.calls[1][1]).toEqual({
      sources: [{ accountId: A.id, uuid: 'S1' }], targetAccount: B.id, targetFolder: 'Notes/Trips', mode: 'move',
    });
    expect(hiddenWhenSent).toBe(true);
    expect(get(transferQuestion)).toBeNull();
    expect(get(selectedNote)).toMatchObject({ uuid: 'c-1', account_id: B.id, label: 'Notes/Trips' });
    expect(get(currentAccount)).toBe(B.id);
    expect(get(pendingDeletedNoteKeys).size).toBe(0);
  });

  it('a loss asks first; Cancel writes nothing and hides nothing', async () => {
    const n = note('S1', A.id, 'Trip');
    notes.set([n]);
    invoke.mockResolvedValueOnce({ notes: [planned(n, { losses: [{ kind: 'tagsAsKeywords' }] })] });
    const done = transferNotes([n], B.id, 'Notes', 'copy');
    await vi.waitFor(() => expect(get(transferQuestion)).not.toBeNull());
    const q = get(transferQuestion)!;
    expect(q.confirmLabel).toBe('Copy');
    expect(q.message).toBe('“Trip”: tags become a Keywords line.');
    q.answer(false);
    await done;
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(get(notes)).toHaveLength(1);
  });

  it('a note the write refused comes back to its list, and the error bar says why', async () => {
    const [n1, n2] = [note('S1', A.id, 'One'), note('S2', A.id, 'Two')];
    notes.set([n1, n2]);
    invoke.mockImplementation(async (cmd: string) =>
      cmd === 'preview_note_transfer'
        ? { notes: [planned(n1), planned(n2)] }
        : { notes: [planned(n1, { targetUuid: 'c-1' }), planned(n2, { refused: 'This note changed while it was being moved. Try again.' })] },
    );
    await transferNotes([n1, n2], B.id, 'Notes', 'move');
    expect(get(notes).map((n) => n.uuid)).toEqual(['S2']);
    expect(get(error)).toBe('“Two” was not moved: This note changed while it was being moved. Try again.');
  });

  it('a failed write puts every original back', async () => {
    const n = note('S1', A.id);
    notes.set([n]);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'preview_note_transfer') return { notes: [planned(n)] };
      throw new Error('database is locked');
    });
    await transferNotes([n], B.id, 'Notes', 'move');
    expect(get(notes).map((x) => x.uuid)).toEqual(['S1']);
    expect(get(error)).toMatch(/^Couldn't move the note/);
  });

  it('a move out of Outlook always asks, and says the original goes for good', () => {
    const n = note('S1', M.id, 'Plan');
    const lines = confirmLines({ notes: [planned(n)] }, 'move', B.id);
    expect(lines).toHaveLength(1);
    expect(lines[0]).toMatch(/deleted from m · Outlook for good once the copy reaches c · iCloud\. Outlook has no Recently Deleted\./);
    expect(confirmLines({ notes: [planned(n)] }, 'copy', B.id)).toEqual([]);
  });

  it('every loss and call-off has words', () => {
    expect(lossText({ kind: 'links' })).toBe('links to and from notes left behind stop working');
    expect(lossText({ kind: 'attachment', filename: 'a.png' })).toBe('the attachment a.png becomes a placeholder');
    expect(calledOffText({ sourceAccount: A.id, sourceUuid: 'S1', targetAccount: M.id, reason: 'targetRefused' }))
      .toMatch(/could not be moved to m · Outlook.*back in a · Gmail/);
    expect(calledOffText({ sourceAccount: A.id, sourceUuid: 'S1', targetAccount: M.id, reason: 'sourceChanged' }))
      .toMatch(/changed in a · Gmail while it was being moved/);
  });
});
