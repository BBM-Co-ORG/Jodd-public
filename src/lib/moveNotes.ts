import { recordLocalSave, refreshNotePersistence } from './notePersistence';
import { sameNote } from './noteIdentity';
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { notes, selectedNote, indexUpsertOnSave, noteMutationRevision } from './stores/notes';

export type MoveResult = { ok: true } | { ok: false; error: unknown };

/**
 * Move one note: snapshot → optimistic relabel → `move_notes_batch` →
 * rollback on failure. Shared by the note context menu's "Move to" and the
 * editor's folder-suggestion chip, so both keep the local-first shape.
 *
 * The relabel runs synchronously, before this function's first `await` —
 * a caller that must close a menu first can build the request, call this,
 * and only then close. It never alerts: the menu shows the error, the chip
 * stays up.
 *
 * `move_notes_batch`, not `save_note`: iCloud can relocate a note without
 * being able to push its content (see the comment on NoteContextMenu's
 * `moveTo`).
 */
export async function moveNoteOptimistic(req: {
  aiResultId?: string;
  accountId: string;
  uuid: string;
  fromLabel: string;
  toLabel: string;
  remoteId: string;
}): Promise<MoveResult> {
  const relabel = (label: string) => {
    notes.update((ns) => {
      const idx = ns.findIndex((n) => sameNote(n, { account_id: req.accountId, uuid: req.uuid }));
      if (idx >= 0) ns[idx] = { ...ns[idx], label };
      return ns;
    });
    if (sameNote(get(selectedNote), { account_id: req.accountId, uuid: req.uuid })) {
      selectedNote.update((n) => (n ? { ...n, label } : n));
    }
  };

  const original = get(notes).find(n => sameNote(n, { account_id: req.accountId, uuid: req.uuid }));
  relabel(req.toLabel);
  if (original) recordLocalSave(original);
  try {
    await invoke<number>('move_notes_batch', {
      ...(req.aiResultId !== undefined ? { aiResultId: req.aiResultId } : {}),
      accountId: req.accountId,
      uuids: [req.uuid],
      targetLabel: req.toLabel,
    });
    // A move is a pure label rewrite in SQLite — the remote id is unchanged.
    indexUpsertOnSave(req.accountId, req.remoteId || null, req.remoteId || '', req.toLabel);
    return { ok: true };
  } catch (error) {
    relabel(req.fromLabel);
    return { ok: false, error };
  } finally {
    noteMutationRevision.update(n => n + 1);
    void refreshNotePersistence({ account_id: req.accountId, uuid: req.uuid });
  }
}
