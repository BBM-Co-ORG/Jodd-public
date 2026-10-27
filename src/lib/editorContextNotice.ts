// The editor's context notice: one line under the editor header when the open
// note is NOT in the view the list shows — after a pick on desktop or tablet
// that left the note open, say — with a Close note button beside it.
//
// It names places the way the rest of the UI does: a folder by its leaf (the
// list header's form), the view by the list title (`#tag`, `All <account>`).
// It used to print raw paths and the folder under a tag filter ("Browsing
// Notes/_test_. This editor remains on Notes/Inbox · …"), which on a Galaxy
// Tab S7 (2026-10-21) named a folder the user was not looking at.
import type { Note } from './types';
import { getNoteTags } from './stores/notes';
import { viewName } from './viewName';

export interface ContextNoticeInput {
  note: Note;
  /** `selectedFolder`: a path, or the `__ALL__` / `__TRASH__` sentinels. */
  folder: string;
  smart: unknown | null;
  search: string;
  tags: Set<string>;
  tagMatchMode: 'AND' | 'OR';
  tagScope: 'account' | 'all';
  noteTags: Map<string, Map<string, string[]>>;
  currentAccount: string | null;
  /** The view's name as the list header and top bar show it (`listTitle`). */
  viewTitle: string;
  /** The note's account, as the UI names it; said only when it is not the current one. */
  noteAccountName: string;
}

/** Whether the open note is one of the rows the current view lists. */
function inView(i: ContextNoticeInput): boolean {
  const sameAccount = !i.currentAccount || i.note.account_id === i.currentAccount;
  if (i.tags.size > 0) {
    const have = getNoteTags(i.noteTags, i.note.account_id, i.note.uuid);
    const sel = [...i.tags];
    const matches = i.tagMatchMode === 'AND' ? sel.every((t) => have.includes(t)) : sel.some((t) => have.includes(t));
    return matches && (i.tagScope === 'all' || sameAccount);
  }
  if (i.folder === '__ALL__') return sameAccount;
  return sameAccount && i.note.label === i.folder;
}

/** A folder below the root, segment by segment: `Notes/Work/Inbox` → `Work › Inbox`. */
function pathBelowRoot(path: string): string {
  const segments = path.split('/');
  const below = segments.length > 1 ? segments.slice(1) : segments;
  return below.map((s) => viewName(null, s, '')).join(' › ');
}

/**
 * The notice's sentence, or null when there is nothing to say: the note is in
 * the view, or the view is a Smart Folder or a search (neither has a place a
 * note "is in" to compare against), or Recently Deleted (no editor there).
 */
export function contextNotice(i: ContextNoticeInput): string | null {
  if (i.smart || i.search.trim() || i.folder === '__TRASH__') return null;
  if (inView(i)) return null;
  const label = i.note.label ?? '';
  let noteFolder = viewName(null, label, '');
  let view = i.viewTitle;
  // Two folders with one leaf ("Inbox" and "Work/Inbox") would read as the
  // same place: name both by their path below the root instead.
  if (i.tags.size === 0 && i.folder !== '__ALL__' && noteFolder === view && label !== i.folder) {
    noteFolder = pathBelowRoot(label);
    view = pathBelowRoot(i.folder);
  }
  const otherAccount = !!i.currentAccount && i.note.account_id !== i.currentAccount;
  const where = otherAccount ? `${noteFolder} (${i.noteAccountName})` : noteFolder;
  return `This note is in ${where}. You're viewing ${view}.`;
}
