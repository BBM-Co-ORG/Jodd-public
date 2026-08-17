import { writable, derived } from 'svelte/store';
import type { Note, Folder, Account, MessageIndex, TrashedNote } from '../types';
import { isActive } from './accountStatus';

// ─── Account display helpers ─────────────────────────────────────────────────
// LocalFS accounts use `localfs:<vaultname>` format everywhere in the UI.
// Gmail accounts show their email address as before.
export function accountDisplay(acct: Account | undefined | null, fallbackId?: string): string {
  if (!acct) return fallbackId ?? '';
  return acct.backend_kind === 'local_fs' ? `localfs:${acct.email}` : acct.email;
}
export function accountDisplayById(accts: Account[], id: string | null | undefined): string {
  if (!id) return '';
  const a = accts.find((x) => x.id === id);
  return a ? accountDisplay(a) : id;
}

export const isAuthenticated = writable<boolean>(false);
export const notes = writable<Note[]>([]);
export const folders = writable<Folder[]>([]);
export const selectedFolder = writable<string>('Notes');
export const selectedNote = writable<Note | null>(null);

// Which Smart Folder view is active, if any — mutually exclusive with
// selectedFolder/selectedTags (see Sidebar's selectFolder/selectTag/
// selectSmartFolder for the mutual-clearing wiring). Fully virtual — no
// `folders` table row backs these (design spec decision 4).
export const selectedSmartFolder = writable<{ account: string; kind: 'orphaned' | 'stale' } | null>(null);
// The note set for the active Smart Folder, fetched by App.svelte whenever
// selectedSmartFolder changes. Kept separate from the main `notes` store
// (which is keyed by real folder `label` paths) rather than merged in.
export const smartFolderNotes = writable<Note[]>([]);

// Which row is selected in the "Recently Deleted" list (TrashList). Separate
// from selectedNote on purpose: a trashed note is read-only and must never
// flow through NoteEditor's autosave path, which would silently resurrect it.
export const selectedTrashedNote = writable<TrashedNote | null>(null);

// Shared search query — promoted from NoteList-local to a store so other
// components (Sidebar folder clicks, NoteEditor's ctx-folder breadcrumb)
// can clear it as part of a navigation gesture. Empty string = not searching.
export const searchQuery = writable<string>('');
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

/**
 * Accounts the app should show and operate on. `accounts` deliberately keeps
 * everything — the sidebar's Inactive group needs the dismissed ones — so
 * every other consumer reads this instead.
 */
export const activeAccounts = derived(accounts, ($a) => $a.filter(isActive));

// ─── Backend capabilities (Microsoft vertical) ────────────────────────────
// What the `backend_capabilities` Tauri command reports for one account:
// whether it has a Trash/"Recently Deleted" concept, and what it can be
// written to. Microsoft is the first backend with unequal capability across
// areas — live verification confirmed notes are writable (M2, 2026-08-15),
// found folder writes never reach Apple Notes (a measured, permanent
// negative — M3, 2026-08-15), and turned pin sidecars on via a named MAPI
// property on the note itself (M4, 2026-08-16) — so a single `can_write`
// boolean could not say it; `Writes` carries one flag per area instead.
export type WriteKind = 'notes' | 'folders' | 'sidecars';
export type Writes = { notes: boolean; folders: boolean; sidecars: boolean };
export type BackendCapabilities = { has_trash: boolean; writes?: Writes };

/**
 * Undefined means "not fetched yet" — default to showing Trash so the item
 * does not appear and then vanish on a slow IPC (the same flicker the
 * `credentialsAvailable` optimistic-true default in AuthScreen avoids).
 */
export function shouldShowTrash(caps: BackendCapabilities | undefined): boolean {
  return caps?.has_trash ?? true;
}

/**
 * Must a delete be confirmed before it happens?
 *
 * True only where the backend has no trash. On Microsoft a delete is a HARD
 * delete — measured: after an Apple-side delete, Deleted Items held zero
 * items and the note was absent from every scan — so there is nothing to
 * restore from and the user must be told before, not after.
 *
 * Gmail and LocalFs keep the one-click delete: both have a real restore
 * path. Capability-driven, never a backend_kind conditional.
 *
 * Optimistic default while capabilities load is `false` (no confirm) —
 * matching `shouldShowTrash`'s optimism, just pointed the other way: a
 * spurious confirm on a recoverable delete is friction, a missing confirm on
 * an irreversible one is data loss, but this default is only reachable for
 * the moment before capabilities arrive, and the backend refuses an
 * unauthorized write independently regardless of what this returns.
 */
export function needsPermanentDeleteConfirm(caps: BackendCapabilities | undefined): boolean {
  return caps?.has_trash === false;
}

/**
 * Can the user perform this class of write on this account?
 *
 * This is only the affordance gate — the enforcement is `refuse_write` in
 * lib.rs, which refuses the command before anything reaches SQLite. That
 * matters because a write that DOES reach SQLite leaves a row the worker can
 * never push, and the account can then never be drained or removed.
 *
 * Optimistic `true` while capabilities load, and `true` for a capabilities
 * object cached before `writes` existed — safe for the same reason: the
 * backend refuses independently, so a stale-optimistic frontend can never
 * turn into a stuck account, only a button that answers with an error. Before
 * M4 this is why Pin could render briefly on a fresh Microsoft account before
 * its capabilities arrived, then disappear once `sidecars: false` landed — a
 * flash, not a bug, since clicking it during that window was refused
 * server-side before it reached SQLite. As of M4 Microsoft's own `sidecars`
 * flag is `true`, but the same optimistic-default reasoning still applies to
 * any backend whose capabilities have not loaded yet.
 */
export function canWrite(caps: BackendCapabilities | undefined, kind: WriteKind): boolean {
  return caps?.writes?.[kind] ?? true;
}

/**
 * Whole-account write gate for components that lock or unlock as a unit on
 * the `notes` area specifically (today: NoteList's New Note button/prompt,
 * via `currentAccountCanWrite` below). The editor and the note context menu
 * used to read this too, until Task 11 split them onto their own per-kind
 * `canWrite` calls — Pin is `sidecars`, everything else there is `notes`,
 * and a single coarse gate would have shown Pin as available the moment
 * Microsoft's notes/folders flipped on while sidecars stayed refused.
 * See `canWrite` for the underlying per-kind check.
 */
export function canWriteAccount(caps: BackendCapabilities | undefined): boolean {
  return canWrite(caps, 'notes');
}

// account_id → capabilities. Keyed per account (not global) because the
// sidebar renders one folder tree — and one Trash entry — per active
// account, not just the current one.
export const capabilitiesByAccount = writable<Record<string, BackendCapabilities>>({});

export function setAccountCapabilities(accountId: string, caps: BackendCapabilities) {
  capabilitiesByAccount.update((m) => ({ ...m, [accountId]: caps }));
}

/**
 * Whether the account the user is currently looking at accepts note writes.
 * NoteList gates its New Note button/prompt on this. See `canWriteAccount`.
 */
export const currentAccountCanWrite = derived(
  [capabilitiesByAccount, currentAccount],
  ([$caps, $cur]) => canWriteAccount($cur ? $caps[$cur] : undefined)
);

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

// Folder-tree refresh signal. Sidebar's folder tree used to re-read only
// when $noteIndex moved, which happens on cold start and on this instance's
// own note saves — never on the poll. A change touching ONLY the `folders`
// table (jodd-mcp writing straight to SQLite, or the sync worker draining a
// dirty_new row) therefore stayed invisible until the app restarted.
//
// A counter, not a boolean: Svelte's safe_not_equal suppresses set(true)
// over an existing `true`, so a flag would notify once and then go quiet for
// every subsequent poll tick.
export const folderRefreshSignal = writable(0);

export function requestFolderRefresh() {
  folderRefreshSignal.update((n) => n + 1);
}

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

// ─── Tags (Jodd-local) ──────────────────────────────────────────────────
// selectedTags drives a multi-tag-filtered NoteList view, mutually exclusive
// with folder navigation: selecting a folder clears it (see
// Sidebar.selectFolder), and a non-empty set takes precedence in NoteList's
// filter. tagMatchMode picks how multiple selected tags combine: AND (note has
// every selected tag) or OR (note has any). Single tag → both modes coincide.
export const selectedTags = writable<Set<string>>(new Set());
export const tagMatchMode = writable<'AND' | 'OR'>('AND');

// Toggle a tag in/out of the selection. New Set each time so Svelte's
// reference-equality reactivity fires.
export function toggleSelectedTag(tag: string) {
  selectedTags.update((s) => {
    const next = new Set(s);
    if (next.has(tag)) next.delete(tag);
    else next.add(tag);
    return next;
  });
}

export function clearSelectedTags() {
  selectedTags.set(new Set());
}

// account → (uuid → tags[]). The frontend's single source of truth for tags,
// loaded from the `list_note_tags` command — complete for the account even
// before note bodies hydrate (the backend index lives in SQLite). Chips read
// from it; the tag cloud below derives from it.
export const noteTagsByAccount = writable<Map<string, Map<string, string[]>>>(new Map());

// Derived tag cloud: per account, every tag with the count of notes carrying
// it, sorted alphabetically. Recomputed automatically whenever the map above
// changes (optimistic add/remove included), so the sidebar stays consistent
// without a separate count structure to keep in sync.
export const tagsByAccount = derived(noteTagsByAccount, ($m) => {
  const out = new Map<string, { tag: string; count: number }[]>();
  for (const [accountId, uuidMap] of $m) {
    const counts = new Map<string, number>();
    for (const tags of uuidMap.values())
      for (const t of tags) counts.set(t, (counts.get(t) ?? 0) + 1);
    out.set(
      accountId,
      [...counts.entries()]
        .map(([tag, count]) => ({ tag, count }))
        .sort((a, b) => a.tag.localeCompare(b.tag)),
    );
  }
  return out;
});

// Replace the full tag map for one account — cold-start / refresh load from
// the `list_note_tags` command's flat (uuid, tag) rows.
export function setAccountNoteTags(
  accountId: string,
  entries: { uuid: string; tag: string }[],
) {
  noteTagsByAccount.update((m) => {
    const inner = new Map<string, string[]>();
    for (const { uuid, tag } of entries) {
      const arr = inner.get(uuid) ?? [];
      arr.push(tag);
      inner.set(uuid, arr);
    }
    m.set(accountId, inner);
    return m;
  });
}

// Set one note's full tag list (used for optimistic add/remove AND rollback —
// pass the prior list to undo). Counts re-derive automatically.
export function setNoteTags(accountId: string, uuid: string, tags: string[]) {
  noteTagsByAccount.update((m) => {
    const inner = m.get(accountId) ?? new Map<string, string[]>();
    if (tags.length === 0) inner.delete(uuid);
    else inner.set(uuid, [...tags].sort((a, b) => a.localeCompare(b)));
    m.set(accountId, inner);
    return m;
  });
}

// Rename a tag across every note in an account (optimistic). Merges into an
// existing tag if a note already carries newTag. Also rewrites selectedTags so
// an active filter follows the rename. Rollback on backend failure is handled
// by the caller re-loading the account's tags from SQLite.
export function renameTagInStore(accountId: string, oldTag: string, newTag: string) {
  noteTagsByAccount.update((m) => {
    const inner = m.get(accountId);
    if (inner) {
      for (const [uuid, tags] of inner) {
        if (!tags.includes(oldTag)) continue;
        const next = tags.filter((t) => t !== oldTag);
        if (!next.includes(newTag)) next.push(newTag);
        inner.set(uuid, next.sort((a, b) => a.localeCompare(b)));
      }
      m.set(accountId, inner);
    }
    return m;
  });
  selectedTags.update((s) => {
    if (!s.has(oldTag)) return s;
    const n = new Set(s);
    n.delete(oldTag);
    n.add(newTag);
    return n;
  });
}

// Delete a tag from every note in an account (optimistic) and from any active
// selection. Rollback handled by the caller (re-load from SQLite).
export function deleteTagFromStore(accountId: string, tag: string) {
  noteTagsByAccount.update((m) => {
    const inner = m.get(accountId);
    if (inner) {
      for (const [uuid, tags] of inner) {
        if (tags.includes(tag)) inner.set(uuid, tags.filter((t) => t !== tag));
      }
      m.set(accountId, inner);
    }
    return m;
  });
  selectedTags.update((s) => {
    if (!s.has(tag)) return s;
    const n = new Set(s);
    n.delete(tag);
    return n;
  });
}

// Read one note's current tags out of a noteTagsByAccount snapshot.
export function getNoteTags(
  map: Map<string, Map<string, string[]>>,
  accountId: string | undefined | null,
  uuid: string,
): string[] {
  if (!accountId) return [];
  return map.get(accountId)?.get(uuid) ?? [];
}

// Function pointer set by App.svelte; lets any component trigger a refresh
// without prop-drilling. Default is a no-op so callers don't need null checks.
export const refreshNotes = writable<() => Promise<void> | void>(() => {});

// Same function-pointer idiom as refreshNotes, but pointing the other way:
// NoteList sets it, App.svelte's global ⌘N handler calls it. Creating a note
// depends on $selectedFolder/$currentAccount scoping that lives in NoteList
// (the '__ALL__' → 'Notes' fallback in particular), so the logic stays there
// rather than being lifted into App. NoteList clears it on destroy so the
// pointer can't outlive the component it closes over.
export const newNoteFn = writable<() => void>(() => {});

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
