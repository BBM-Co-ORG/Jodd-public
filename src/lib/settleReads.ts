import { isRealFolderSelection } from './components/askScope';

// Which folders a settle (focus, visible, folder) reads from the backend.
//
// It used to be `$selectedFolder` and nothing else, and `loadFolderNotes`
// returns at once for the `__ALL__` sentinel. So with "All <account>"
// selected, coming back to the app read NOTHING — and Gmail has no minute
// detector (`incremental_pull_supported`), so the open note's cache stayed
// as it was until the ten-minute poll, which every return to the app restarts.
// Measured 2026-11-12 on Android: a Gmail note opened from All, edited on
// another device, typed into here → "(conflict copy 2026-11-12 19:46)" with
// the app open well past the 1 s visible settle.
//
// The note the user has open is the one a stale base costs a conflict copy
// on, so its folder is read too — whatever the view: All, a tag, a smart
// folder, or a folder other than the note's own.
export interface FolderRead {
  accountId: string;
  folder: string;
}

export interface OpenNoteLike {
  uuid: string;
  account_id?: string | null;
  label?: string | null;
}

export function folderReadKey(r: FolderRead): string {
  return `${r.accountId}\u0000${r.folder}`;
}

/** The open note's own folder, when it has one the backend can list. */
export function openNoteFolder(note: OpenNoteLike | null | undefined, currentAccount: string | null | undefined): FolderRead | null {
  if (!note || !note.uuid || note.uuid.startsWith('tmp:')) return null; // never saved: nothing remote to read
  const accountId = note.account_id || currentAccount;
  if (!accountId || !isRealFolderSelection(note.label)) return null;
  return { accountId, folder: note.label as string };
}

/** The selected folder (if real) and the open note's folder, deduped. */
export function settleReads(
  selectedFolder: string | null | undefined,
  currentAccount: string | null | undefined,
  openNote: OpenNoteLike | null | undefined,
): FolderRead[] {
  // Recently Deleted has its own loader, and a note open there is in the trash.
  if (selectedFolder === '__TRASH__') return [];
  const reads: FolderRead[] = [];
  if (currentAccount && isRealFolderSelection(selectedFolder)) {
    reads.push({ accountId: currentAccount, folder: selectedFolder as string });
  }
  const own = openNoteFolder(openNote, currentAccount);
  if (own && !reads.some((r) => folderReadKey(r) === folderReadKey(own))) reads.push(own);
  return reads;
}
