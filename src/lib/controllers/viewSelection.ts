// View selection — what picking a folder, a tag or a Smart Folder does to the
// selection stores. One implementation shared by the Sidebar (desktop,
// tablet, and the phone Folders pane) and the phone Tags / Smart folders
// panes, so the surfaces cannot drift.
//
// Each function is ONE synchronous step (gotcha #28: the view's identity and
// the values compared against it change in the same turn), and each is pure
// of pane navigation: a phone caller adds its own `navigateToPane('list', …)`
// after it — the push that makes a re-tap of the view already shown still
// open the list (ruling R21), which App's change-keyed watchers cannot see.
// What a pick does to the open note is selection.ts's rule (spec §5.3):
// desktop and tablet open the view's first note in the same synchronous step
// as the view change (gotcha #28: no intermediate null for the editor to
// render); the phone clears it and opens nothing.
import { get } from 'svelte/store';
import {
  currentAccount, searchQuery, navigateToFolder, selectedNote, selectedTags, tagMatchMode,
  clearSelectedTags, toggleSelectedTag, selectedSmartFolder, notes, smartFolderNotes, viewSnapshot,
  selectedFolder, type SmartFolderKind,
} from '../stores/notes';
import { clearSelectionAfterPick, openFirstNoteFor, pickOpensFirstNote } from './selection';
import type { Note } from '../types';

// Account first: switching it before the view lets downstream observers
// (NoteList's filter, loadFolderNotes' scope) see the right context by the
// time the view store fires.
function useAccount(accountId: string): boolean {
  if (get(currentAccount) === accountId) return false;
  currentAccount.set(accountId);
  return true;
}

/**
 * The note step after a pick's view writes. `expectLoad`: the pick changed
 * what App's watchers key their local listing loads on (the folder value, the
 * tag set, the Smart Folder), so a listing not yet in memory is on its way.
 */
function afterPick(expectLoad: boolean): void {
  if (pickOpensFirstNote()) openFirstNoteFor(viewSnapshot(), expectLoad);
  else clearSelectionAfterPick();
}

const tagKey = (account: string | null, tags: Set<string>) => JSON.stringify([account, [...tags].sort()]);

/**
 * A folder (or the `__ALL__` / `__TRASH__` sentinels). Leaves any Smart
 * Folder and tag filter (`navigateToFolder`), and exits search: a "results
 * across folders" view is incoherent with "show me this folder", and leaving
 * search active made the tap feel like it did nothing.
 */
export function selectFolder(accountId: string, path: string): void {
  const folderBefore = get(selectedFolder);
  useAccount(accountId);
  if (get(searchQuery)) searchQuery.set('');
  navigateToFolder(path);
  afterPick(folderBefore !== path);
}

/**
 * The desktop tag cloud's pick: toggle one tag in or out of the multi-tag
 * filter. Tags are per account, so switching account starts a fresh
 * selection. A search stays (the cloud narrows within it).
 */
export function toggleTag(accountId: string, tag: string): void {
  if (useAccount(accountId)) clearSelectedTags();
  selectedSmartFolder.set(null);
  toggleSelectedTag(tag);
  afterPick(get(selectedTags).size > 0);
}

/**
 * The phone Tags pane's pick: exactly `tags` (in that order — the list title's
 * order) under `mode`, replacing any selection. Leaves a Smart Folder and a
 * search: the pane opens that tag's list, not search results.
 */
export function selectOnlyTags(accountId: string, tags: string[], mode: 'AND' | 'OR'): void {
  // App's tag paint is keyed on the account and the tag set: either changing
  // starts a load this pick can wait for.
  const tagsBefore = tagKey(get(currentAccount), get(selectedTags));
  useAccount(accountId);
  selectedSmartFolder.set(null);
  if (get(searchQuery)) searchQuery.set('');
  tagMatchMode.set(mode);
  selectedTags.set(new Set(tags));
  afterPick(tags.length > 0 && tagKey(get(currentAccount), get(selectedTags)) !== tagsBefore);
}

/**
 * A Smart Folder. Fully virtual — no folders-table row — so selectedFolder's
 * VALUE is left untouched (App.svelte's folder watcher is keyed on changes to
 * it and must not re-fire); NoteList checks selectedSmartFolder first, so the
 * stale folder is simply never consulted while a Smart Folder is active.
 */
export function selectSmartFolder(accountId: string, kind: SmartFolderKind): void {
  const before = get(selectedSmartFolder);
  const changed = before?.account !== accountId || before?.kind !== kind;
  useAccount(accountId);
  clearSelectedTags();
  if (get(searchQuery)) searchQuery.set('');
  // Another Smart Folder's rows are not this one's: drop them now, so neither
  // the list nor the first-note step reads them while this one loads.
  if (changed) smartFolderNotes.set([]);
  selectedSmartFolder.set({ account: accountId, kind });
  afterPick(changed);
}

/**
 * A fresh blank `tmp:` note in `path` of `accountId`, opened: the same
 * lifecycle as NoteList's + (autosave on the first keystroke, dropped if
 * abandoned). Opens the folder too, so the note shows in the list it went to.
 */
export function openNewNote(accountId: string, path: string): Note {
  const blank: Note = {
    id: '',
    uuid: 'tmp:' + Math.random().toString(36).slice(2, 10),
    title: 'New Note',
    body_html: '<html><head></head><body></body></html>',
    date: new Date().toISOString(),
    label: path,
    x_mail_created_date: null,
    account_id: accountId,
  };
  useAccount(accountId);
  navigateToFolder(path);
  notes.update((ns) => [blank, ...ns]);
  selectedNote.set(blank);
  return blank;
}
