// After an AI run created a note (Extract, Ingest, Research Studio): go to
// where the backend filed it, paint it from cache, refresh tags and select
// it. Shared by LessonExtractModal and StudioModal; what else a caller does
// with the note (post-extract suggestions, retiring a capture) stays with it.
import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { notes, selectedNote, navigateToFolder, setAccountNoteTags, indexUpsertOnSave } from './stores/notes';
import type { Note } from './types';

/** Returns the selected note's row, or undefined when the cache did not list it. */
export async function openCreatedNote(accountId: string, created: { uuid: string; label: string }): Promise<Note | undefined> {
  const acct = accountId;
  // Navigate to the folder the backend actually filed the note into —
  // Notes/Inbox, or the root when the backend cannot create folders.
  // The backend already wrote the row synchronously to SQLite; setting
  // selectedFolder triggers App.svelte's paintFolderFromCache watcher
  // (doctrine: cache-only, no Gmail touch on a normal navigation/write
  // path). Then merge the new note row explicitly so we can select it
  // without racing the watcher.
  navigateToFolder(created.label);
  try {
    const cached = await invoke<Note[]>('list_cached_notes_in_folder', {
      accountId: acct,
      path: created.label,
    });
    notes.update((ns) => {
      const others = ns.filter(
        (n) => !(n.account_id === acct && n.label === created.label),
      );
      return [...others, ...cached];
    });
  } catch (e) {
    console.warn('post-extract cache paint failed', e);
  }
  // Refresh the tag store so the LLM-suggested tags appear in the
  // sidebar tag cloud and editor chip row without waiting for the next
  // app startup or unrelated tag change.
  try {
    const rows = await invoke<{ uuid: string; tag: string }[]>(
      'list_note_tags',
      { accountId: acct },
    );
    setAccountNoteTags(acct, rows);
  } catch (e) {
    console.warn('post-extract tag refresh failed', e);
  }
  const found = get(notes).find((n: Note) => n.account_id === acct && n.uuid === created.uuid);
  if (found) {
    selectedNote.set(found);
    // Bump $noteIndex via indexUpsertOnSave. Sidebar's reactive
    // refreshFolders block fires on $noteIndex changes, which is how it
    // discovers a freshly-created Notes/Inbox. Inbox is an ordinary
    // `user`-kind folder, so it stays under Folders — the bump only makes
    // it appear. Without it, the new folder is missing from the sidebar
    // until the next unrelated note save or app restart.
    indexUpsertOnSave(acct, null, found.id, found.label);
  }
  return found;
}
