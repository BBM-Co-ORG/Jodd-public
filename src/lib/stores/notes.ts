import { writable } from 'svelte/store';
import type { Note, Folder, Account, MessageIndex } from '../types';

export const isAuthenticated = writable<boolean>(false);
export const notes = writable<Note[]>([]);
export const folders = writable<Folder[]>([]);
export const selectedFolder = writable<string>('Notes');
export const selectedNote = writable<Note | null>(null);
export const isLoading = writable<boolean>(false);
export const isSaving = writable<boolean>(false);
export const error = writable<string | null>(null);

// Multi-account state:
//   accounts        — every signed-in Gmail account, loaded from backend
//   currentAccount  — id (= email) of the account currently being viewed/edited
// The single-account UI uses currentAccount as the implicit scope for
// list/save/delete. Multi-account UI (later) will let the user switch.
export const accounts = writable<Account[]>([]);
export const currentAccount = writable<string | null>(null);

// Multi-select set in the note list. Stores uuids (not Notes) so it stays
// stable across the polls that mutate $notes array references. Cleared on
// folder switch, account switch, or explicit clearSelectedUuids() call.
// $selectedNote remains the "primary" — what the editor renders — so the
// existing single-note flows keep working unchanged.
export const selectedUuids = writable<Set<string>>(new Set());

export function clearSelectedUuids() {
  selectedUuids.set(new Set());
}

// Per-account stub index — { msg_id, label } for every Notes message on the
// server. Populated by `index_account` on cold start (or refresh) and used
// for FOLDER COUNTS so they're accurate even before bodies hydrate. Without
// this, counts would lag behind the real Gmail state until every folder
// gets fetched.
export const noteIndex = writable<Map<string, MessageIndex[]>>(new Map());

// Per-account set of folders that have completed at least one hydration
// pass since cold-start (i.e. `list_notes_in_folder` returned). Used to
// drive a tiny "loading" hint in the NoteList header and to schedule the
// background sweep — folders NOT in this set are next in line.
export const hydratedFolders = writable<Map<string, Set<string>>>(new Map());

export function markFolderHydrated(accountId: string, folderPath: string) {
  hydratedFolders.update((m) => {
    const set = m.get(accountId) ?? new Set<string>();
    set.add(folderPath);
    m.set(accountId, set);
    return m;
  });
}

// Function pointer set by App.svelte; lets any component trigger a refresh
// without prop-drilling. Default is a no-op so callers don't need null checks.
export const refreshNotes = writable<() => Promise<void> | void>(() => {});

// Recently-saved UUIDs, scoped per-account: accountId → (uuid → timestamp ms).
// Used to protect just-saved notes from being overwritten by a background poll
// that fires before Gmail's index has propagated the insert. NoteEditor adds
// to this on successful save; App.svelte's loadNotes merges these into fetched
// results for a short window.
//
// Per-account scoping is critical: the same uuid CAN legitimately exist in
// two accounts (e.g. a note that was copied between mailboxes, or two devices
// signed into both accounts). Without per-account scoping, Account A's recent
// save could suppress Account B's legitimate remote change for the same uuid,
// silently dropping data.
export const recentlySavedUuids = writable<Map<string, Map<string, number>>>(new Map());

export function markRecentlySaved(accountId: string, uuid: string) {
  if (!accountId || !uuid) return;
  recentlySavedUuids.update((m) => {
    const inner = m.get(accountId) ?? new Map<string, number>();
    inner.set(uuid, Date.now());
    m.set(accountId, inner);
    // Garbage-collect entries older than 60 seconds across all accounts.
    // Drop empty inner maps so a long-running session doesn't accumulate
    // entries for accounts the user has signed out of.
    const cutoff = Date.now() - 60_000;
    for (const [aid, sub] of m) {
      for (const [u, t] of sub) if (t < cutoff) sub.delete(u);
      if (sub.size === 0) m.delete(aid);
    }
    return m;
  });
}

// Helper for the merge guards in App.svelte. Returns the timestamp of the
// last save for (accountId, uuid), or 0 if none. Encapsulates the nested
// lookup so callers don't have to know the map shape.
export function recentSaveTimestamp(
  recents: Map<string, Map<string, number>>,
  accountId: string | undefined | null,
  uuid: string,
): number {
  if (!accountId) return 0;
  return recents.get(accountId)?.get(uuid) ?? 0;
}

// Keep the per-account stub index in sync with local save/delete so the
// sidebar folder counts stay correct between sign-in snapshots. Without
// these patches, the index reflects only the state at sign-in: a new note
// created in this session doesn't bump its folder's count, and a deleted
// note doesn't decrement.
//
// `previousGmailId` is the Gmail message id BEFORE this save. Gmail save =
// insert-new + trash-old, so the new id replaces the old one in the index.
// Pass null for a brand-new note that had no prior id.
export function indexUpsertOnSave(
  accountId: string,
  previousGmailId: string | null,
  newGmailId: string,
  label: string,
) {
  noteIndex.update((m) => {
    const list = m.get(accountId);
    if (!list) return m;  // index not yet loaded for this account — nothing to keep in sync
    const filtered = previousGmailId
      ? list.filter((s) => s.id !== previousGmailId)
      : list.slice();
    filtered.push({ id: newGmailId, label });
    m.set(accountId, filtered);
    return m;
  });
}

export function indexRemoveOnDelete(accountId: string, gmailId: string) {
  if (!gmailId) return;
  noteIndex.update((m) => {
    const list = m.get(accountId);
    if (!list) return m;
    m.set(accountId, list.filter((s) => s.id !== gmailId));
    return m;
  });
}

// Subtree-rewrite the per-account stub index when a folder is renamed or
// moved. Without this, folderCountsByAccount in Sidebar still sees the old
// path via index stubs, and buildRows leaves the old folder visible in the
// tree until the next index refresh — a doctrine-breaking lag on a pure
// local op.
export function indexRewriteOnFolderRename(
  accountId: string,
  oldPath: string,
  newPath: string,
) {
  if (!accountId || !oldPath || !newPath || oldPath === newPath) return;
  noteIndex.update((m) => {
    const list = m.get(accountId);
    if (!list) return m;
    const prefix = `${oldPath}/`;
    const rewritten = list.map((s) => {
      if (s.label === oldPath) return { ...s, label: newPath };
      if (s.label.startsWith(prefix)) return { ...s, label: newPath + s.label.slice(oldPath.length) };
      return s;
    });
    m.set(accountId, rewritten);
    return m;
  });
}
