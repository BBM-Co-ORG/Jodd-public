// Blocked pin and folder pushes (migration 23, gotcha #14): the backend
// refused them permanently, so the worker stopped trying. Unlike a blocked
// note — whose reason rides on the note itself (`push_blocked_reason`) —
// these have no field on anything the frontend holds, so they are read
// separately (`list_sync_blocks`) and kept here, keyed by account.
//
// Local-first: every local action that re-arms a block in SQLite (re-pinning,
// renaming or deleting a folder, "Try again") clears the marker here in the
// same step, before any IPC returns.
import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';

export interface AccountSyncBlocks {
  /** note uuid → reason */
  pins: Record<string, string>;
  /** folder path → reason */
  folders: Record<string, string>;
}

interface SyncBlocksWire {
  pins: { uuid: string; reason: string }[];
  folders: { path: string; reason: string }[];
}

export const syncBlocks = writable<Record<string, AccountSyncBlocks>>({});

// Bumped by every local clear. A refresh that read SQLite before the clear
// carries an older answer than the store, and is dropped rather than letting
// it put a marker back.
let localEpoch = 0;
let refreshSeq = 0;

export async function refreshSyncBlocks(accountIds: string[]): Promise<void> {
  const seq = ++refreshSeq;
  const epoch = localEpoch;
  const results = await Promise.allSettled(
    accountIds.map((id) => invoke<SyncBlocksWire>('list_sync_blocks', { accountId: id })),
  );
  if (seq !== refreshSeq || epoch !== localEpoch) return;
  const next: Record<string, AccountSyncBlocks> = {};
  results.forEach((r, i) => {
    const id = accountIds[i];
    if (r.status === 'fulfilled') {
      next[id] = {
        pins: Object.fromEntries((r.value?.pins ?? []).map((p) => [p.uuid, p.reason])),
        folders: Object.fromEntries((r.value?.folders ?? []).map((f) => [f.path, f.reason])),
      };
    } else {
      console.error(`[jodd] list_sync_blocks failed for ${id}:`, r.reason);
      // Keep what we had rather than hide a block on a failed read.
      const prev = get(syncBlocks)[id];
      if (prev) next[id] = prev;
    }
  });
  syncBlocks.set(next);
}

export function pinBlockReason(
  m: Record<string, AccountSyncBlocks>,
  accountId: string | null | undefined,
  uuid: string | null | undefined,
): string | null {
  if (!accountId || !uuid) return null;
  return m[accountId]?.pins[uuid] ?? null;
}

export function folderBlockReason(
  m: Record<string, AccountSyncBlocks>,
  accountId: string | null | undefined,
  path: string | null | undefined,
): string | null {
  if (!accountId || !path) return null;
  return m[accountId]?.folders[path] ?? null;
}

function edit(accountId: string, f: (b: AccountSyncBlocks) => AccountSyncBlocks): void {
  localEpoch++;
  syncBlocks.update((m) => (m[accountId] ? { ...m, [accountId]: f(m[accountId]) } : m));
}

/** `set_pin` / `set_pin_batch` re-arm these notes' pin pushes. */
export function forgetPinBlocks(accountId: string, uuids: string[]): void {
  const drop = new Set(uuids);
  edit(accountId, (b) => ({
    ...b,
    pins: Object.fromEntries(Object.entries(b.pins).filter(([u]) => !drop.has(u))),
  }));
}

/** `rename_subtree` / `mark_folder_deleted` re-arm this folder and everything under it. */
export function forgetFolderBlocks(accountId: string, path: string): void {
  const prefix = `${path}/`;
  edit(accountId, (b) => ({
    ...b,
    folders: Object.fromEntries(
      Object.entries(b.folders).filter(([p]) => p !== path && !p.startsWith(prefix)),
    ),
  }));
}

function restore(accountId: string, key: 'pins' | 'folders', k: string, reason: string): void {
  syncBlocks.update((m) =>
    m[accountId] ? { ...m, [accountId]: { ...m[accountId], [key]: { ...m[accountId][key], [k]: reason } } } : m,
  );
}

/** "Try again" on one blocked pin. An unchanged cause re-blocks on the worker's next tick. */
export async function retryPinBlock(accountId: string, uuid: string): Promise<void> {
  const reason = pinBlockReason(get(syncBlocks), accountId, uuid);
  forgetPinBlocks(accountId, [uuid]);
  try {
    await invoke<boolean>('retry_blocked_pin', { accountId, uuid });
  } catch (e) {
    if (reason) restore(accountId, 'pins', uuid, reason);
    throw e;
  }
}

/** "Try again" on one blocked folder. */
export async function retryFolderBlock(accountId: string, path: string): Promise<void> {
  const reason = folderBlockReason(get(syncBlocks), accountId, path);
  // Only this row is re-armed server-side, so only this row's marker goes.
  edit(accountId, (b) => ({
    ...b,
    folders: Object.fromEntries(Object.entries(b.folders).filter(([p]) => p !== path)),
  }));
  try {
    await invoke<boolean>('retry_blocked_folder', { accountId, path });
  } catch (e) {
    if (reason) restore(accountId, 'folders', path, reason);
    throw e;
  }
}
