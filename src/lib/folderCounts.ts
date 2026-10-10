import type { MessageIndex, Note } from './types';

// Per-account, per-folder note counts for the sidebar — and, since
// `buildRows` gives every key a row, which folders the notes put there.
//   - A folder that has been hydrated this session has its notes in
//     `notes`, so the live notes are the truth — for its count AND for
//     whether the notes put it in the tree at all. SQLite is authoritative;
//     the index can carry ghost stubs from server-side cleanup events the
//     frontend didn't observe (e.g. cleanup_stale_uuid_duplicates).
//   - Otherwise use the server-side INDEX. It lands within seconds of
//     sign-in even for 6k-note mailboxes, so counts are accurate long
//     before bodies arrive — but it never updates after that snapshot,
//     so for hydrated folders the live count beats it.
export function folderCounts(
  notes: Note[],
  index: Map<string, MessageIndex[]>,
  hydratedByAcct: Map<string, Set<string>>,
): Record<string, Record<string, number>> {
  // Live counts from notes (per account, per label).
  const liveByAcct: Record<string, Record<string, number>> = {};
  for (const n of notes) {
    const aid = n.account_id ?? '';
    if (!aid) continue;
    if (!liveByAcct[aid]) liveByAcct[aid] = {};
    liveByAcct[aid][n.label] = (liveByAcct[aid][n.label] || 0) + 1;
  }
  // Index counts (per account, per label).
  const indexByAcct: Record<string, Record<string, number>> = {};
  for (const [accountId, idx] of index) {
    const m: Record<string, number> = {};
    for (const stub of idx) m[stub.label] = (m[stub.label] || 0) + 1;
    indexByAcct[accountId] = m;
  }
  // Per-folder choice. Iterate every (account, label) seen in either map.
  const out: Record<string, Record<string, number>> = {};
  const allAccts = new Set<string>([
    ...Object.keys(liveByAcct),
    ...Object.keys(indexByAcct),
  ]);
  for (const aid of allAccts) {
    const hydrated = hydratedByAcct.get(aid) ?? new Set<string>();
    const live = liveByAcct[aid] ?? {};
    const idx = indexByAcct[aid] ?? {};
    const allPaths = new Set<string>([
      ...Object.keys(live),
      ...Object.keys(idx),
    ]);
    const m: Record<string, number> = {};
    for (const path of allPaths) {
      if (!hydrated.has(path)) {
        m[path] = idx[path] ?? live[path] ?? 0;
      } else if (live[path]) {
        m[path] = live[path];
      }
      // Hydrated and empty: no key. The index is a snapshot the frontend
      // keeps by remote id, and a Gmail note's id changes on every push, so
      // a note saved, pushed and deleted can leave stubs no delete removes —
      // which kept a deleted folder in the sidebar at 0 (measured
      // 2026-10-08). A real empty folder still gets its row, from the
      // folders table (`list_folders` → `foldersByAccount`).
    }
    out[aid] = m;
  }
  return out;
}
