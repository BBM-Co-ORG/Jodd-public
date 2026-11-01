import { invoke } from '@tauri-apps/api/core';
import { isRealFolderSelection } from './components/askScope';

/**
 * The one place a folder path becomes a `list_notes_in_folder` call.
 *
 * `$selectedFolder` is not always a folder label — Sidebar stores two
 * sentinels in it ('__ALL__', '__TRASH__'), and every DEFERRED refresh path
 * in App.svelte reads that store when its timer FIRES rather than when it was
 * armed: the folder settle (App.svelte:124), the focus settle
 * (App.svelte:110) and cold-start Phase C (App.svelte:423). So the path this
 * function receives is whatever the user had selected a beat later, not what
 * the caller had in mind when it scheduled the work.
 *
 * Guarding inside each caller is what produced the live defect on
 * 2026-09-20: all three guarded '__ALL__' and none guarded '__TRASH__', so
 * clicking Recently Deleted within a settle window sent '__TRASH__' to the
 * backend, which has no such label and no such `folders` row and answers
 * Err("Folder not found: __TRASH__") (lib.rs:4110). The banner then stuck,
 * because `error` is cleared only by a save or a sign-in.
 *
 * `null` means "not a fetchable folder — skip, nothing is wrong". It is
 * deliberately distinct from `[]`, which means the folder is real and empty:
 * a caller that prunes local rows against the fetched set must not treat an
 * un-fetchable sentinel as "the server says this folder is empty".
 */
export async function fetchFolderNotes(
  accountId: string,
  folderPath: string | null | undefined,
): Promise<any[] | null> {
  if (!isRealFolderSelection(folderPath)) return null;
  return await invoke<any[]>('list_notes_in_folder', { accountId, path: folderPath });
}

/**
 * True when `err` is `list_notes_in_folder` saying `folderPath` is no folder
 * at all — `Err(format!("Folder not found: {}", path))` in lib.rs, returned
 * only after it checked the label map AND the local folders table (a folder
 * still pending locally gets `[]`, not this). So it is an answer, not a
 * failure: the folder was removed elsewhere and the prune dropped its row.
 */
export function isFolderGone(err: unknown, folderPath: string): boolean {
  // Never the root: falling back to it is the remedy, so a root that is not
  // found (an account whose root is not literally `Notes`, gotcha #9) must
  // surface as an error rather than vanish into a re-select of itself.
  return folderPath !== 'Notes' && String(err) === `Folder not found: ${folderPath}`;
}
