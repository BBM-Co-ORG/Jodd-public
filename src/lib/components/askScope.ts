export type AskScope =
  | { kind: 'all_accounts' }
  | { kind: 'folder'; account_id: string; label: string }
  | { kind: 'account'; account_id: string }
  // Reached only from the note context menu (spec 2026-10-23). `title` is
  // display-only; the backend re-reads the live note.
  | { kind: 'note'; account_id: string; uuid: string; title: string; links: boolean; tags: boolean; depth: number }
  // Reached only from the note list's multi-select menu. `titles` is
  // display-only; the backend re-reads every live note.
  | { kind: 'notes'; account_id: string; uuids: string[]; titles: string[] };

export type NoteAskScope = Extract<AskScope, { kind: 'note' }>;

/** Mirrors `ask::NOTE_DEPTH_MAX`. */
export const NOTE_DEPTH_MAX = 3;

/** A note scope as the context menu opens it: both boxes off, depth 1. */
export function noteScope(n: { account_id: string; uuid: string; title: string }): AskScope {
  return { kind: 'note', account_id: n.account_id, uuid: n.uuid, title: n.title, links: false, tags: false, depth: 1 };
}

/**
 * The scope for "Ask Jodd about these notes": every saved note in the
 * multi-select, in list order. `null` when they span accounts (the backend
 * reads one account per scope) or none is saved. One note is a note scope.
 */
export function notesScope(ns: { account_id?: string | null; uuid: string; title: string }[]): AskScope | null {
  const saved = ns.filter((n) => !!n.uuid && !!n.account_id);
  if (saved.length === 0) return null;
  const account_id = saved[0].account_id as string;
  if (saved.some((n) => n.account_id !== account_id)) return null;
  if (saved.length === 1) return noteScope({ account_id, uuid: saved[0].uuid, title: saved[0].title });
  return { kind: 'notes', account_id, uuids: saved.map((n) => n.uuid), titles: saved.map((n) => n.title) };
}

/** A copy with the popup's related-notes controls applied; depth clamped to 1..NOTE_DEPTH_MAX. */
export function withNoteOptions(s: NoteAskScope, patch: Partial<Pick<NoteAskScope, 'links' | 'tags' | 'depth'>>): AskScope {
  const next = { ...s, ...patch };
  next.depth = Math.min(NOTE_DEPTH_MAX, Math.max(1, Math.round(next.depth)));
  return next;
}

// `$selectedFolder` is not always a real folder label — Sidebar.svelte also
// stores two sentinels in it: '__ALL__' for the per-account "All" row
// (Sidebar.svelte:1155) and '__TRASH__' for the Trash row (Sidebar.svelte:1247).
// Both are truthy strings, so a naive `if ($selectedFolder)` guard would ship
// either sentinel to the backend as a folder label, where nothing recognizes
// it, `count_notes_in_scope` / `list_notes_in_subtree` match zero rows, and
// the user gets a false "I couldn't find any notes in this scope" — the
// feature's honesty surface reporting a fabricated fact about the vault
// instead of a malformed request.
const FOLDER_SCOPE_SENTINELS = new Set(['__ALL__', '__TRASH__']);

/** True only when `selectedFolder` names a real folder, not a sentinel. */
export function isRealFolderSelection(selectedFolder: string | null | undefined): boolean {
  return !!selectedFolder && !FOLDER_SCOPE_SENTINELS.has(selectedFolder);
}

/**
 * Pure decision logic behind AskJoddModal's scope selector, extracted so it
 * is directly testable without mounting the component.
 *
 * Returns `null` when the selected scope cannot be expressed — which today
 * means only one thing: a non-'all' scope with no current account. Every
 * account-anchored variant of `AskScope` carries a required `account_id`, and
 * the Rust side deserializes it into a `String`, so `account_id: null` is not
 * a degraded request the backend can partially honour — serde rejects it.
 *
 * The caller must refuse to send on `null` rather than substitute something.
 * Both available substitutes are worse than not asking: `''` reaches the
 * backend as a real account id that matches no rows, and silently promoting to
 * `all_accounts` answers from accounts the user did not point at. Either way
 * the user gets a confidently-worded answer about the wrong set of notes —
 * the same failure the FOLDER_SCOPE_SENTINELS guard above exists to prevent.
 */
export function askScope(
  scopeKind: 'account' | 'folder' | 'all',
  currentAccount: string | null | undefined,
  selectedFolder: string | null | undefined,
): AskScope | null {
  if (scopeKind === 'all') return { kind: 'all_accounts' };
  if (!currentAccount) return null;
  if (scopeKind === 'folder' && isRealFolderSelection(selectedFolder)) {
    return { kind: 'folder', account_id: currentAccount, label: selectedFolder as string };
  }
  return { kind: 'account', account_id: currentAccount };
}
