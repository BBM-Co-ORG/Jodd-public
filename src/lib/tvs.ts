// The TVs linked on this device (spec 2026-11-02-tv-now-playing-ingest §5.3).
// `list_tvs` reads a flag file only (gotcha #15), so it is safe at startup and
// from a render path: the AI menu reads this store and never awaits IPC. The
// screen ID and lounge token never reach the frontend.
import { writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';

export type TvHealth = 'online' | 'unreachable' | 'needs_repair';

export type TvRow = {
  tv_id: string;
  label: string;
  model: string | null;
  is_default: boolean;
  needs_repair: boolean;
  last_status: TvHealth | null;
};

/** One TV that had a video, offered when no single answer won (spec §4). */
export type TvCandidate = {
  tv_id: string;
  label: string;
  url: string;
  /** Final copy, e.g. "Watched on Bedroom up to 2:27"; null when no time is known. */
  watched_label: string | null;
  playing: boolean;
};

/** What `tv_now_playing` resolves to. */
export type TvOutcome =
  | { kind: 'found'; tv_id: string; label: string; url: string; watched_label: string | null; via_default: boolean }
  | { kind: 'choose'; candidates: TvCandidate[] };

/** What `tv_now_playing` rejects with. */
export type TvError =
  | { code: 'no_tvs' }
  | { code: 'nothing_playing' }
  | { code: 'needs_repair'; label: string }
  | { code: 'network' }
  | { code: 'cancelled' };

export const tvs = writable<TvRow[]>([]);

/** Re-read the list. Keeps the last known list when the read fails. */
export async function loadTvs(): Promise<void> {
  try {
    const list = await invoke<TvRow[]>('list_tvs');
    if (Array.isArray(list)) tvs.set(list);
  } catch (e) {
    console.warn('list_tvs failed — keeping the last known TV list', e);
  }
}

/** The TV asked first: the default row, else the first one. */
export function defaultTv(list: TvRow[]): TvRow | null {
  return list.find((t) => t.is_default) ?? list[0] ?? null;
}
