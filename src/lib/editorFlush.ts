// Writing what the editor holds to SQLite before Jodd closes on purpose.
//
// The editor saves 1.5 s after the last keystroke. Normally nothing is lost
// in that window: a note switch or the editor's own destruction saves first.
// Windows' updater ends the process with exit(0), which runs neither, so the
// update path locks every editor, asks each to save now, and waits until the
// save has reached SQLite (spec 2026-11-11-app-updater-design.md §6).

import { writable } from 'svelte/store';

type Flush = () => Promise<void>;

/**
 * True while Jodd is about to close for an update: every editor is read-only,
 * so nothing typed after the flush can miss SQLite. Set only by appUpdate.ts,
 * and cleared again if the install does not go ahead.
 */
export const editorsLocked = writable(false);

export function setEditorsLocked(locked: boolean): void {
  editorsLocked.set(locked);
}

const flushes = new Set<Flush>();

/** An editor registers on mount; call the returned function on destroy. */
export function registerEditorFlush(fn: Flush): () => void {
  flushes.add(fn);
  return () => {
    flushes.delete(fn);
  };
}

/** Save every open editor's pending edit. Rejects if any save fails. */
export async function flushEditors(): Promise<void> {
  await Promise.all([...flushes].map((f) => f()));
}
