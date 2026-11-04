// What the phone list pane is called, and how many notes it shows (spec
// 2026-10-20 §5.1). One source for the list PaneHeader's title AND for the
// title every list push records in phoneNav's stack — the note pane's Back
// label reads that stack, so the two must agree or Back lies about where it
// goes.
import { derived, get, writable } from 'svelte/store';
import { publication } from './publication';
import type { Account, Note } from '../types';
import {
  accounts, activeAccounts, accountDisplay, backendLabel, currentAccount, searchQuery,
  selectedFolder, selectedSmartFolder, selectedTags, type SmartFolderKind,
} from './notes';
import { viewName } from '../viewName';

export interface ListViewSelection {
  smart: { kind: SmartFolderKind } | null;
  folder: string | null | undefined;
  tags: Iterable<string>;
  search: string;
  accountDisplay: string;
}

/**
 * The list view's name, in the order NoteList's `filteredNotes` picks what it
 * shows: a search wins ("Search" — the list holds search results, whatever
 * the view underneath), then a Smart Folder, then the tag filter — `#tag`, or
 * every selected tag in selection order, the same form NoteList's tag scope
 * line uses — then the folder, by `viewName` (folder leaf, "All <account>",
 * "Recently Deleted").
 */
export function listTitleOf(sel: ListViewSelection): string {
  if (sel.search.trim()) return 'Search';
  if (sel.smart) return viewName(sel.smart, sel.folder, sel.accountDisplay);
  const tags = [...sel.tags];
  if (tags.length > 0) return tags.map((t) => '#' + t).join(' ');
  return viewName(null, sel.folder, sel.accountDisplay);
}

/**
 * An account's short name for a one-line subtitle: the part of an address
 * before `@`; a Local Folder's last path segment (its `email` is a path); the
 * whole name for anything else (SSH's `host:folder`). A short name that names
 * two active accounts names neither, so on a clash it widens: to the full
 * address (the path, for a Local Folder), and when even that is shared —
 * the same address on two backends — to the address plus the backend label
 * ("a@b.com · iCloud").
 */
export function accountShortName(acct: Account, active: Account[]): string {
  const others = active.filter((o) => o.id !== acct.id);
  const mine = shortOf(acct);
  if (!others.some((o) => shortOf(o) === mine)) return mine;
  if (!others.some((o) => o.email === acct.email)) return acct.email;
  return `${acct.email} · ${backendLabel(acct.backend_kind ?? 'gmail')}`;
}

/**
 * The editor metadata line's account prefix (spec 2026-10-20 §6.1): the short
 * name and the backend, only when more than one account is active. When the
 * short name already had to name the backend to tell two same-address
 * accounts apart, it is not repeated.
 */
export function accountPrefixOf(acct: Account | undefined, active: Account[]): string | null {
  if (!acct || active.length <= 1) return null;
  const short = accountShortName(acct, active);
  const backend = acct.backend_kind ? backendLabel(acct.backend_kind) : '';
  if (!backend || short.endsWith(` · ${backend}`)) return short;
  return `${short} · ${backend}`;
}

function shortOf(a: Account): string {
  if (a.backend_kind === 'local_fs') {
    const segments = a.email.split(/[\\/]/).filter(Boolean);
    return segments[segments.length - 1] ?? a.email;
  }
  const at = a.email.indexOf('@');
  return at > 0 ? a.email.slice(0, at) : a.email;
}

/**
 * `N notes · <account short>` — the account only when more than one account
 * is active (with one there is nothing to tell apart). `count` null means the
 * list has not reported one; the subtitle then names only the account.
 */
export function listSubtitleOf(count: number | null, acct: Account | undefined, active: Account[]): string {
  const parts: string[] = [];
  if (count !== null) parts.push(`${count} ${count === 1 ? 'note' : 'notes'}`);
  if (acct && active.length > 1) parts.push(accountShortName(acct, active));
  return parts.join(' · ');
}

function currentAccountDisplay($accounts: Account[], id: string | null): string {
  return accountDisplay($accounts.find((a) => a.id === id), id ?? '');
}

/** The live list title, from the selection stores. */
export const listTitle = derived(
  [selectedSmartFolder, selectedFolder, selectedTags, searchQuery, accounts, currentAccount],
  ([$smart, $folder, $tags, $search, $accounts, $current]) =>
    listTitleOf({
      smart: $smart,
      folder: $folder,
      tags: $tags,
      search: $search,
      accountDisplay: currentAccountDisplay($accounts, $current),
    }),
);

/** The title a list push records, read at the moment of the push. */
export function currentListTitle(): string {
  return get(listTitle);
}

/**
 * How many notes the mounted list shows; null while no list reports one.
 * Written only through `claimListNoteCount`.
 */
export const listNoteCount = writable<number | null>(null);

/**
 * The rows the mounted NoteList shows, in its order; null while none is
 * mounted. The neighbour a delete opens is read from it (selection.ts).
 * Written only through `claimShownNotes`.
 */
export const shownNotes = writable<Note[] | null>(null);

/**
 * Whether the mounted NoteList is still loading what it will show (a search
 * or cross-account tag request in flight, or a folder the cache has not
 * hydrated) — so the editor's empty state does not say "No notes" about a
 * list that has not arrived. Null while none is mounted.
 */
export const listLoading = writable<boolean | null>(null);

/**
 * A writer's handle on one of the stores above. NoteList and TrashList both
 * write the count, and the list pane swaps between them (Recently Deleted ↔
 * a folder) — the outgoing list's destroy can run AFTER the incoming one has
 * reported; so can two NoteLists across a layout change. See publication.ts.
 */
export const claimListNoteCount = publication<number | null>(listNoteCount, null);
export const claimShownNotes = publication<Note[] | null>(shownNotes, null);
export const claimListLoading = publication<boolean | null>(listLoading, null);

/** The list pane's subtitle, live. */
export const listSubtitle = derived(
  [listNoteCount, activeAccounts, currentAccount],
  ([$count, $active, $current]) =>
    listSubtitleOf($count, $active.find((a) => a.id === $current), $active),
);
