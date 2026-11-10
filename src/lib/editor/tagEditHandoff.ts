// Tag edits queued in the phone tags sheet (ruling R26) that outlive the
// editor they were made in.
//
// A rotation swaps App's phone layout for the tablet one (or back), and each
// layout mounts its own NoteEditor: the outgoing editor — and the sheet with
// it — is destroyed after the incoming one has been created (Svelte 5.56:
// init incoming → destroy outgoing → mount incoming). Live pass 2026-10-21:
// the sheet closed on rotation, and R26's queued adds and removes went with
// it. They are handed to the editor that is live for the SAME note instead,
// which applies them in order once its body has rendered. If no editor shows
// that note they are dropped, as R26 drops any sheet edit that can no longer
// land on the note it was made for.

export type TagEdit = { op: 'add' | 'remove'; tag: string };

export interface TagEditReceiver {
  /** Takes `ops` for the note `noteKey` if this editor shows it; true if taken. */
  accept(noteKey: string, ops: TagEdit[]): boolean;
}

const receivers = new Set<TagEditReceiver>();

/** Registers a live editor. Returns the unregister. */
export function registerTagEditReceiver(r: TagEditReceiver): () => void {
  receivers.add(r);
  return () => {
    receivers.delete(r);
  };
}

/**
 * Hands `ops` for `noteKey` to the newest live editor other than `from` that
 * shows that note. Returns false (and the edits are dropped) if none does.
 */
export function handOffTagEdits(from: TagEditReceiver | null, noteKey: string, ops: TagEdit[]): boolean {
  if (ops.length === 0) return false;
  for (const r of [...receivers].reverse()) {
    if (r !== from && r.accept(noteKey, ops)) return true;
  }
  return false;
}
