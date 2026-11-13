// The permanent-delete confirm's state, shared by the two menus that offer
// Delete (NoteContextMenu and NoteMoreMenu): `ask` is the `confirm` argument
// of noteActions.deleteNoteWithConfirm, and `pending` drives the menu's own
// <ConfirmDialog>, which reports its one decision through `decide`.
// Never window.confirm() — WKWebView drops it.
import { writable, type Readable } from 'svelte/store';

/**
 * How a confirm presents its confirm button. A destructive action (removing
 * an account, deleting a folder or tag) is never the primary: it gets danger
 * styling and a verb for a label instead of "OK" (spec 2026-10-20 copy style).
 */
export interface ConfirmOptions {
  destructive?: boolean;
  confirmLabel?: string;
}

export interface PendingConfirm {
  title: string;
  message: string;
}

export interface ConfirmGate {
  /** The question being asked, or null when none is open. */
  pending: Readable<PendingConfirm | null>;
  ask(title: string, message: string): Promise<boolean>;
  decide(ok: boolean): void;
}

export function createConfirmGate(): ConfirmGate {
  const pending = writable<PendingConfirm | null>(null);
  let resolve: ((ok: boolean) => void) | null = null;
  return {
    pending: { subscribe: pending.subscribe },
    ask(title, message) {
      pending.set({ title, message });
      return new Promise((r) => { resolve = r; });
    },
    decide(ok) {
      pending.set(null);
      resolve?.(ok);
      resolve = null;
    },
  };
}
