// Gmail-side duplicate counts per account, as `get_dup_stats` reports them.
// One store so the Sidebar's dup pill and Settings › Accounts show the same
// number from the same source. The backend's list_notes pipeline records the
// count; reading it is a Mutex read, so refreshing is cheap.
import { writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import type { DedupSummary } from '../types';

export const dupStats = writable<Record<string, DedupSummary>>({});

export async function refreshDupStats(accountId: string): Promise<void> {
  try {
    const s = await invoke<DedupSummary>('get_dup_stats', { accountId });
    dupStats.update((m) => ({ ...m, [accountId]: s }));
  } catch {
    // get_dup_stats can't really fail — but if it does, just leave the
    // previous value in place rather than zeroing the pill mid-flight.
  }
}
