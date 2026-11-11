// Rename / Delete a tag across every note in an account. Shared by the
// Sidebar's tag menu (desktop and tablet) and the phone Tags pane's sheet, so
// the two cannot drift (the same rule as accountActions.ts). Each caller owns
// its own prompt and confirm (PromptDialogs) and passes them in.
import { invoke } from '@tauri-apps/api/core';
import { renameTagInStore, deleteTagFromStore, setAccountNoteTags } from '../stores/notes';
import type { ConfirmOptions } from './confirmGate';

/** Deleting a tag edits every note that carries it: a destructive confirm. */
export const DELETE_TAG_CONFIRM: ConfirmOptions = { destructive: true, confirmLabel: 'Delete' };

/**
 * Client normalization mirrors the backend (trim, lowercase, drop spaces /
 * control characters / `#`). Unicode-friendly so Thai survives.
 */
export function normalizeTag(input: string): string {
  return input.trim().toLowerCase().replace(/[#\p{White_Space}\p{Cc}]/gu, '');
}

// Re-pull an account's tag map from SQLite — rolls back the optimistic store
// mutation when the backend op fails (the DB is unchanged on failure).
async function reloadAccountTags(accountId: string): Promise<void> {
  try {
    const rows = await invoke<{ uuid: string; tag: string }[]>('list_note_tags', { accountId });
    setAccountNoteTags(accountId, rows);
  } catch (e) {
    console.error('reloadAccountTags failed', e);
  }
}

export async function renameTag(
  accountId: string,
  tag: string,
  askName: (title: string, defaultValue: string) => Promise<string | null>,
): Promise<void> {
  const input = await askName(`Rename tag "${tag}"`, tag);
  if (input === null) return;
  const next = normalizeTag(input);
  if (!next || next === tag) return;
  renameTagInStore(accountId, tag, next); // optimistic
  try {
    await invoke('rename_tag', { accountId, oldTag: tag, newTag: next });
  } catch (e) {
    await reloadAccountTags(accountId); // rollback from DB
    alert(`Failed to rename tag: ${e}`);
  }
}

export async function deleteTag(
  accountId: string,
  tag: string,
  askConfirm: (title: string, message: string, options: ConfirmOptions) => Promise<boolean>,
): Promise<void> {
  const ok = await askConfirm(
    'Delete tag?',
    `Remove the tag #${tag} from every note in this account? The word stays in the text.`,
    DELETE_TAG_CONFIRM,
  );
  if (!ok) return;
  deleteTagFromStore(accountId, tag); // optimistic
  try {
    await invoke('delete_tag', { accountId, tag });
  } catch (e) {
    await reloadAccountTags(accountId); // rollback from DB
    alert(`Failed to delete tag: ${e}`);
  }
}
