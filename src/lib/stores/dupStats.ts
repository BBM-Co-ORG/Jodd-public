// Gmail-side duplicate counts per account, as `get_dup_stats` reports them.
// One store so the Sidebar's dup pill and Settings › Accounts show the same
// number from the same source. The backend's list_notes pipeline records the
// count; reading it is a Mutex read, so refreshing is cheap.
import { get, writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import type { DedupSummary } from '../types';

export const dupStats = writable<Record<string, DedupSummary>>({});

export async function refreshDupStats(accountId: string): Promise<void> {
  try {
    const s = await invoke<DedupSummary>('get_dup_stats', { accountId });
    // Unchanged → no write: every subscriber (the Sidebar pill, App's
    // duplicate check) would otherwise re-run for nothing. Checked before
    // `update`, because a store notifies even when `update` returns the same
    // object (objects always count as changed).
    const cur = get(dupStats)[accountId];
    if (cur && cur.collapsed === s.collapsed && cur.uuids_affected === s.uuids_affected) return;
    dupStats.update((m) => ({ ...m, [accountId]: s }));
  } catch {
    // get_dup_stats can't really fail — but if it does, just leave the
    // previous value in place rather than zeroing the pill mid-flight.
  }
}

// The Sidebar asks on every `$notes` change, which is every editor keystroke.
// The count only moves when a listing pass finishes, so a burst collapses to
// one read per account once the notes go quiet for a moment.
const DUP_STATS_SETTLE_MS = 1000;
let pending = new Set<string>();
let timer: ReturnType<typeof setTimeout> | null = null;

export function scheduleDupStatsRefresh(accountIds: string[]): void {
  for (const id of accountIds) pending.add(id);
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    const ids = pending;
    pending = new Set();
    for (const id of ids) void refreshDupStats(id);
  }, DUP_STATS_SETTLE_MS);
}
