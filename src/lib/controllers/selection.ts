// What becomes the open note after a pick or a delete (spec 2026-10-20 §5.3).
//
// | Action                        | Desktop / tablet                       | Phone                 |
// |-------------------------------|----------------------------------------|-----------------------|
// | Pick folder / tag / Smart     | open the FIRST note in list order      | nothing auto-opens    |
// | Cold start (the starting view)| the same, once, after the cache paint  | nothing auto-opens    |
// | Delete the open note          | open the next note, else the previous, | pop to `list`         |
// |                               | else the empty state                   |                       |
//
// Three rules hold everywhere here:
//
// - **One synchronous step (gotcha #28).** A pick sets the view and the note
//   in the same call when the view's listing is already in memory — never an
//   intermediate `null` the editor could render.
// - **Never the remote.** A listing that is not loaded yet waits for the
//   EXISTING local load (App's SQLite paint, or NoteList's cross-account tag
//   read) to report through `listingLoaded`, and applies only if the view is
//   still the one picked (`viewUnchanged`) and nothing was opened meanwhile.
// - **Only explicit picks (gotcha #6).** Nothing here reacts to a refresh: a
//   `remote-changed` reload that reorders the list leaves the open note open.
//   A pending pick is consumed by the first matching load, so a later reload
//   finds nothing waiting.
import { get, writable } from 'svelte/store';
import type { Note } from '../types';
import { noteKey } from '../noteIdentity';
import { listedNotes } from '../noteListing';
import {
  crossAccountTagNotes, currentAccount, noteTagsByAccount, notes, pendingDeletedNoteKeys,
  searchQuery, selectedFolder, selectedNote, selectedSmartFolder, selectedTags, smartFolderNotes,
  tagMatchMode, tagScope, viewUnchanged,
} from '../stores/notes';
import { shownNotes } from '../stores/listView';
import { isPhoneNavActive, popPane } from '../stores/phoneNav';

/**
 * A Gmail `tmp:` draft nobody typed into: the blank + and ⌘N create (title
 * "New Note", empty body) or one with nothing at all. NoteEditor drops these
 * from the list when the editor leaves them, so opening one would open a note
 * that is about to vanish.
 */
export function isEmptyDraft(n: Note): boolean {
  if (!n.uuid?.startsWith('tmp:')) return false;
  if (n.title && n.title !== 'New Note') return false;
  const body = n.body_html ?? '';
  const inner = body.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? body.replace(/<head[\s\S]*?<\/head>/i, '');
  return inner.replace(/<[^>]*>/g, '').replace(/\s|&nbsp;/g, '') === '';
}

function openable(n: Note, pendingDeleted: Set<string>): boolean {
  return !pendingDeleted.has(noteKey(n)) && !isEmptyDraft(n);
}

/** The first row a pick opens: skips rows pending deletion and empty drafts. */
export function firstOpenableNote(list: Note[], pendingDeleted: Set<string>): Note | null {
  return list.find((n) => openable(n, pendingDeleted)) ?? null;
}

/**
 * What opens after deleting `deletedKeys` from `list` (in list order): the
 * first openable row after the LAST deleted one, else the nearest openable
 * row before it, else null. For one note that is "next, else previous"; for a
 * batch it is the first note left below the deleted block.
 */
export function neighbourAfterDeleting(list: Note[], deletedKeys: Set<string>): Note | null {
  let last = -1;
  list.forEach((n, i) => { if (deletedKeys.has(noteKey(n))) last = i; });
  if (last < 0) return null;
  const keep = (n: Note) => !deletedKeys.has(noteKey(n)) && openable(n, new Set());
  for (let i = last + 1; i < list.length; i++) if (keep(list[i])) return list[i];
  for (let i = last - 1; i >= 0; i--) if (keep(list[i])) return list[i];
  return null;
}

/** One note: the next row in the list, else the previous, else null. */
export function neighbourAfterDelete(list: Note[], deletedKey: string): Note | null {
  return neighbourAfterDeleting(list, new Set([deletedKey]));
}

/**
 * The listing NoteList shows for the CURRENT view, computed now from the
 * stores (lib/noteListing.ts — NoteList's own function). No search results:
 * a pick that leaves a search active does not auto-open (see `listingKind`).
 */
export function currentListing(): Note[] {
  return listedNotes({
    query: get(searchQuery),
    searchResults: [],
    smartFolder: get(selectedSmartFolder),
    smartFolderNotes: get(smartFolderNotes),
    selectedTags: get(selectedTags),
    tagMatchMode: get(tagMatchMode),
    noteTags: get(noteTagsByAccount),
    tagScope: get(tagScope),
    crossAccountTagNotes: get(crossAccountTagNotes),
    notes: get(notes),
    currentAccount: get(currentAccount),
    selectedFolder: get(selectedFolder),
    pendingDeleted: get(pendingDeletedNoteKeys),
  });
}

/**
 * Which local load fills the current view's listing — the `kind` its loader
 * reports through `listingLoaded`. Null for a view no pick opens a note in:
 * a search (typing never auto-opens, and its results live in NoteList) and
 * Recently Deleted (its own list; the editor is not shown there).
 */
export type ListingKind = 'folder' | 'tags' | 'crossTags' | 'smart';

export function listingKind(): ListingKind | null {
  if (get(searchQuery).trim()) return null;
  if (get(selectedSmartFolder)) return 'smart';
  if (get(selectedTags).size > 0) return get(tagScope) === 'all' ? 'crossTags' : 'tags';
  return get(selectedFolder) === '__TRASH__' ? null : 'folder';
}

/** True while a pick waits for its listing; the editor's empty state stays neutral meanwhile. */
export const autoOpenPending = writable(false);

let pending: { snapshot: string; kind: ListingKind } | null = null;

function clearPending(): void {
  pending = null;
  autoOpenPending.set(false);
}

// A waiting pick is void the moment the view leaves it or a note opens — by
// any route, not only another pick — so a later load of the same view (a
// return to it by Reveal folder, say) can never open a note nobody asked for.
// The pick's own writes all land before `pending` is set, so they never
// cancel it.
for (const store of [currentAccount, selectedFolder, selectedSmartFolder, selectedTags, searchQuery] as const) {
  (store as { subscribe(fn: () => void): unknown }).subscribe(() => {
    if (pending && (!viewUnchanged(pending.snapshot) || get(searchQuery).trim())) clearPending();
  });
}
selectedNote.subscribe((n) => {
  if (pending && n !== null) clearPending();
});

/**
 * The desktop/tablet step after a pick set the view (`snapshot` is
 * `viewSnapshot()` right after it). Opens the listing's first note in this
 * same call when it is in memory; else, when `expectLoad` (the pick started a
 * local load for this view), clears the editor and waits for that load. A
 * view with no listing to open from clears the editor.
 */
export function openFirstNoteFor(snapshot: string, expectLoad = false): void {
  clearPending();
  const kind = viewUnchanged(snapshot) ? listingKind() : null;
  if (kind === null) {
    selectedNote.set(null);
    return;
  }
  const first = firstOpenableNote(currentListing(), get(pendingDeletedNoteKeys));
  if (first || !expectLoad) {
    selectedNote.set(first);
    return;
  }
  selectedNote.set(null);
  pending = { snapshot, kind };
  autoOpenPending.set(true);
}

/**
 * A local listing load finished. `snapshot` is `viewSnapshot()` from when it
 * STARTED, so a load begun for another view never answers this pick. Called
 * by App's cache paints and Smart Folder load and by NoteList's cross-account
 * tag read, in `finally`, so a failed read settles the pick as empty.
 */
export function listingLoaded(kind: ListingKind, snapshot: string): void {
  if (!pending || pending.kind !== kind || pending.snapshot !== snapshot) return;
  clearPending();
  // The user moved, or opened something, while the load ran: theirs wins.
  if (!viewUnchanged(snapshot) || get(selectedNote) !== null) return;
  selectedNote.set(firstOpenableNote(currentListing(), get(pendingDeletedNoteKeys)));
}

/**
 * Whether a pick opens a note at all: desktop and tablet do; the phone stack
 * pushes the list and opens nothing.
 */
export function pickOpensFirstNote(): boolean {
  return !isPhoneNavActive();
}

/**
 * Cold start on desktop and tablet: the view Jodd starts on counts as a pick,
 * once, when App's first cache paint lands — the editor half of the screen
 * no longer sits on "Select a note" beside a full list (Galaxy Tab S7,
 * 2026-10-21). Only that one call: a later refresh never opens anything
 * (gotcha #6). A note opened before the paint landed (an Ask citation, a
 * share) stays, a search typed meanwhile opens nothing, and the phone keeps
 * its stack at the list.
 */
export function openFirstNoteAtStartup(): void {
  if (!pickOpensFirstNote() || get(selectedNote) !== null || listingKind() === null) return;
  const first = firstOpenableNote(currentListing(), get(pendingDeletedNoteKeys));
  if (first) selectedNote.set(first);
}

/** The phone's pick step, and any pick that must not auto-open. */
export function clearSelectionAfterPick(): void {
  clearPending();
  selectedNote.set(null);
}

/**
 * The list as shown, for a delete's neighbour: NoteList's own rows when it is
 * mounted (search results included), else the listing computed from stores.
 */
export function listAsShown(): Note[] {
  return get(shownNotes) ?? currentListing();
}

/**
 * The note to open after deleting the open note (`deletedKeys` holds it), read
 * from `listBefore` — the list as shown BEFORE the rows were removed. On the
 * phone the note pane pops to the list instead and nothing opens.
 */
export function selectionAfterDeleting(listBefore: Note[], deletedKeys: Set<string>): Note | null {
  clearPending();
  if (isPhoneNavActive()) {
    popPane('note');
    return null;
  }
  return neighbourAfterDeleting(listBefore, deletedKeys);
}
