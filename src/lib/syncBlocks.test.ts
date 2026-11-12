// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import {
  syncBlocks,
  refreshSyncBlocks,
  pinBlockReason,
  folderBlockReason,
  forgetPinBlocks,
  forgetFolderBlocks,
  retryPinBlock,
  retryFolderBlock,
} from './syncBlocks';

const A = 'gmail:a@b.com';

function blocks(pins: [string, string][], folders: [string, string][]) {
  return {
    pins: pins.map(([uuid, reason]) => ({ uuid, reason })),
    folders: folders.map(([path, reason]) => ({ path, reason })),
  };
}

describe('syncBlocks', () => {
  beforeEach(() => {
    invoke.mockReset();
    syncBlocks.set({});
  });

  it('reads each account’s blocked pins and folders', async () => {
    invoke.mockResolvedValueOnce(blocks([['u1', 'meta refused']], [['Notes/Work', 'label refused']]));

    await refreshSyncBlocks([A]);

    expect(invoke).toHaveBeenCalledWith('list_sync_blocks', { accountId: A });
    const m = get(syncBlocks);
    expect(pinBlockReason(m, A, 'u1')).toBe('meta refused');
    expect(pinBlockReason(m, A, 'u2')).toBeNull();
    expect(folderBlockReason(m, A, 'Notes/Work')).toBe('label refused');
    expect(folderBlockReason(m, 'other', 'Notes/Work')).toBeNull();
  });

  // Local-first: the marker goes the moment the user re-pins, not after a
  // round trip — `set_pin` re-arms the row in SQLite synchronously.
  it('forgets a pin block when the user toggles the pin', async () => {
    invoke.mockResolvedValueOnce(blocks([['u1', 'r'], ['u2', 'r']], []));
    await refreshSyncBlocks([A]);

    forgetPinBlocks(A, ['u1']);

    const m = get(syncBlocks);
    expect(pinBlockReason(m, A, 'u1')).toBeNull();
    expect(pinBlockReason(m, A, 'u2')).toBe('r');
  });

  // `rename_subtree` re-arms every row it moves; a sibling sharing a name
  // prefix is not in the subtree.
  it('forgets the blocks of a renamed folder’s whole subtree, and only that', async () => {
    invoke.mockResolvedValueOnce(
      blocks([], [['Notes/Work', 'r'], ['Notes/Work/Q3', 'r'], ['Notes/Workshop', 'r']]),
    );
    await refreshSyncBlocks([A]);

    forgetFolderBlocks(A, 'Notes/Work');

    const m = get(syncBlocks);
    expect(folderBlockReason(m, A, 'Notes/Work')).toBeNull();
    expect(folderBlockReason(m, A, 'Notes/Work/Q3')).toBeNull();
    expect(folderBlockReason(m, A, 'Notes/Workshop')).toBe('r');
  });

  it('clears a pin block before the retry returns, and restores it if the retry fails', async () => {
    invoke.mockResolvedValueOnce(blocks([['u1', 'r']], []));
    await refreshSyncBlocks([A]);
    let reject!: (e: unknown) => void;
    invoke.mockReturnValueOnce(new Promise((_, rj) => (reject = rj)));

    const pending = retryPinBlock(A, 'u1');

    expect(pinBlockReason(get(syncBlocks), A, 'u1')).toBeNull();
    expect(invoke).toHaveBeenLastCalledWith('retry_blocked_pin', { accountId: A, uuid: 'u1' });
    reject(new Error('ipc down'));
    await expect(pending).rejects.toThrow('ipc down');
    expect(pinBlockReason(get(syncBlocks), A, 'u1')).toBe('r');
  });

  it('retries one folder block', async () => {
    invoke.mockResolvedValueOnce(blocks([], [['Notes/Work', 'r']]));
    await refreshSyncBlocks([A]);
    invoke.mockResolvedValueOnce(true);

    await retryFolderBlock(A, 'Notes/Work');

    expect(invoke).toHaveBeenLastCalledWith('retry_blocked_folder', { accountId: A, path: 'Notes/Work' });
    expect(folderBlockReason(get(syncBlocks), A, 'Notes/Work')).toBeNull();
  });

  // A refresh read SQLite before the user's re-pin landed; its answer is
  // older than the store and must not put the marker back.
  it('drops a refresh that started before a local clear', async () => {
    invoke.mockResolvedValueOnce(blocks([['u1', 'r']], []));
    await refreshSyncBlocks([A]);
    let resolve!: (v: unknown) => void;
    invoke.mockReturnValueOnce(new Promise((r) => (resolve = r)));

    const stale = refreshSyncBlocks([A]);
    forgetPinBlocks(A, ['u1']);
    resolve(blocks([['u1', 'r']], []));
    await stale;

    expect(pinBlockReason(get(syncBlocks), A, 'u1')).toBeNull();
  });
});
