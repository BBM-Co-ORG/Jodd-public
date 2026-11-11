// A store that component instances publish into, where more than one
// instance can be alive at once. The list pane swaps NoteList for TrashList,
// and on Android a rotation swaps App's tablet layout for the phone one (or
// back): for a moment the outgoing and the incoming instance both exist, the
// incoming one publishes, and the outgoing one is destroyed afterwards. An
// unconditional `onDestroy(() => store.set(null))` then wipes the live
// instance's value — live pass 2026-10-21: the phone note header lost "Saved"
// and its ⋯ until the note was reopened.
//
// So each instance writes under its own owner. The store shows the latest
// write; when an instance releases, its value goes and the store falls back
// to the latest value of a writer still alive, or to `empty` once none is.
import type { Writable } from 'svelte/store';

export interface Publication<T> {
  /** Shows `value`; this instance becomes the latest writer. */
  set(value: T): void;
  /** Withdraws this instance's value. Call it from onDestroy. */
  release(): void;
}

/**
 * Returns a claim function for `store`: each call is one writer's handle
 * (call it once per component instance, at init).
 */
export function publication<T>(store: Writable<T>, empty: T): () => Publication<T> {
  // Live writers, in order of their latest write (oldest first).
  const writers = new Map<symbol, T>();
  return () => {
    const owner = Symbol('publication');
    return {
      set(value) {
        writers.delete(owner);
        writers.set(owner, value);
        store.set(value);
      },
      release() {
        if (!writers.has(owner)) return;
        const wasLatest = [...writers.keys()].pop() === owner;
        writers.delete(owner);
        // An older writer's withdrawal must not touch what the latest shows.
        if (!wasLatest) return;
        const rest = [...writers.values()];
        store.set(rest.length > 0 ? rest[rest.length - 1] : empty);
      },
    };
  };
}
