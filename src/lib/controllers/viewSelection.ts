// View selection — what picking a folder, a tag or a Smart Folder does to the
// selection stores. One implementation shared by the Sidebar (desktop,
// tablet, and the phone Folders pane) and the phone Tags / Smart views
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
  selectedFolder, tagScope, type SmartFolderKind,
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
 * The scope a tag pick lists (spec 2026-11-13 §2): one account's notes, or
 * every account's (`null`, the cross-account filter). Returns whether the
 * pick starts a fresh selection: a change of account or of scope does,
 * because a tag set means different notes there.
 */
function useTagScope(accountId: string | null): boolean {
  const scope = accountId === null ? 'all' : 'account';
  const scopeChanged = get(tagScope) !== scope;
  tagScope.set(scope);
  const accountChanged = accountId !== null && useAccount(accountId);
  return scopeChanged || accountChanged;
}

/**
 * The desktop tag cloud's pick: toggle one tag in or out of the multi-tag
 * filter, in one account or (`accountId: null`) all of them. A change of
 * account or scope starts a fresh selection. A search stays (the cloud
 * narrows within it).
 */
export function toggleTag(accountId: string | null, tag: string): void {
  if (useTagScope(accountId)) clearSelectedTags();
  selectedSmartFolder.set(null);
  toggleSelectedTag(tag);
  afterPick(get(selectedTags).size > 0);
}

/**
 * The phone Tags pane's pick: exactly `tags` (in that order — the list title's
 * order) under `mode`, replacing any selection. Leaves a Smart Folder and a
 * search: the pane opens that tag's list, not search results.
 */
export function selectOnlyTags(accountId: string | null, tags: string[], mode: 'AND' | 'OR'): void {
  // App's tag paint (or, for all accounts, NoteList's cross-account read) is
  // keyed on the scope, the account and the tag set: any changing starts a
  // load this pick can wait for.
  const tagsBefore = tagKey(get(tagScope) === 'all' ? null : get(currentAccount), get(selectedTags));
  useTagScope(accountId);
  selectedSmartFolder.set(null);
  if (get(searchQuery)) searchQuery.set('');
  tagMatchMode.set(mode);
  selectedTags.set(new Set(tags));
  afterPick(tags.length > 0 && tagKey(accountId === null ? null : get(currentAccount), get(selectedTags)) !== tagsBefore);
}

/**
 * A Smart Folder. Fully virtual — no folders-table row — so selectedFolder's
 * VALUE is left untouched (App.svelte's folder watcher is keyed on changes to
 * it and must not re-fire); NoteList checks selectedSmartFolder first, so the
 * stale folder is simply never consulted while a Smart Folder is active.
 */
export function selectSmartFolder(accountId: string | null, kind: SmartFolderKind): void {
  const before = get(selectedSmartFolder);
  const changed = !before || before.account !== accountId || before.kind !== kind;
  // All accounts (`null`) leaves the current account as it is: the note that
  // opens brings its own (selection.ts).
  if (accountId !== null) useAccount(accountId);
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
