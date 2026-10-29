/**
 * The merge step of App.svelte's `loadNotes` (the All view's refresh), pulled
 * out so it can be tested without mounting App.
 *
 * `loadNotes` fans `list_notes` out over every account with
 * Promise.allSettled. A rejected listing used to go only to console.error, and
 * the merge kept only the fulfilled accounts — so an unreadable Local Folder
 * vault's notes vanished from the list with no message (gotcha #6). Now:
 *
 * - a non-auth failure KEEPS that account's notes from the pre-refresh store
 *   and returns its reason in `errors`, for the same ErrorBar that
 *   `loadFolderNotes` uses;
 * - auth loss is unchanged: the account goes in `authLost`, its notes are not
 *   kept (recovery drops them), and no error message is produced;
 * - `tmp:` blanks and recently-saved notes are protected exactly as before.
 */
export interface ListingMerge<N> {
  merged: N[];
  authLost: string[];
  errors: string[];
}

export function mergeAccountListings<N extends { uuid?: string; account_id?: string | null }>(
  accountIds: string[],
  results: PromiseSettledResult<N[]>[],
  before: N[],
  isAuthLost: (reason: unknown) => boolean,
  isRecentlySaved: (n: N) => boolean,
): ListingMerge<N> {
  const fetched: N[] = [];
  const authLost: string[] = [];
  const errors: string[] = [];
  const failed = new Set<string>();
  results.forEach((r, i) => {
    if (r.status === 'fulfilled') {
      fetched.push(...r.value);
    } else if (isAuthLost(r.reason)) {
      authLost.push(accountIds[i]);
    } else {
      failed.add(accountIds[i]);
      errors.push(String(r.reason));
    }
  });
  const kept = before.filter((n) => n.account_id != null && failed.has(n.account_id));
  const keptSet = new Set(kept);
  const fetchedUuids = new Set(fetched.map((n) => n.uuid));
  const survivors = before.filter(
    (n) =>
      !keptSet.has(n) &&
      n.uuid &&
      !fetchedUuids.has(n.uuid) &&
      (n.uuid.startsWith('tmp:') || isRecentlySaved(n)),
  );
  return { merged: [...survivors, ...kept, ...fetched], authLost, errors };
}
