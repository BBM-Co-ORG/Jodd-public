// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { fetchFolderNotes, isFolderGone } from './folderFetch';

describe('fetchFolderNotes — sentinel guard', () => {
  beforeEach(() => invoke.mockReset());

  // The live defect (2026-09-20): App.svelte's three deferred refresh paths
  // — the folder settle (App.svelte:124), the focus settle (App.svelte:110)
  // and cold-start Phase C (App.svelte:423) — all read `get(selectedFolder)`
  // when the timer FIRES, not when it was armed. Arm one on a real folder,
  // click Recently Deleted within the settle window, and the fire-time read
  // hands '__TRASH__' to the fetch. The Rust side has no such label and no
  // such folders row, so `list_notes_in_folder` returns
  // Err("Folder not found: __TRASH__") (lib.rs:4110) and the red banner
  // sticks — `error` is cleared only by a save or a sign-in.
  //
  // The guard that already existed covered '__ALL__' alone. This is the
  // assertion that would have failed: the sentinel must never reach the
  // backend at all.
  it('does not reach the backend for the Recently Deleted sentinel', async () => {
    const result = await fetchFolderNotes('gmail:a@b.com', '__TRASH__');
    expect(invoke).not.toHaveBeenCalled();
    expect(result).toBeNull();
  });

  it('does not reach the backend for the per-account All sentinel', async () => {
    const result = await fetchFolderNotes('gmail:a@b.com', '__ALL__');
    expect(invoke).not.toHaveBeenCalled();
    expect(result).toBeNull();
  });

  it('does not reach the backend for an empty folder path', async () => {
    expect(await fetchFolderNotes('gmail:a@b.com', '')).toBeNull();
    expect(await fetchFolderNotes('gmail:a@b.com', null)).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });

  it('fetches a real folder label through list_notes_in_folder', async () => {
    const fetched = [{ uuid: 'u1', label: 'Notes/Jodd-Demo' }];
    invoke.mockResolvedValueOnce(fetched);

    const result = await fetchFolderNotes('gmail:a@b.com', 'Notes/Jodd-Demo');

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('list_notes_in_folder', {
      accountId: 'gmail:a@b.com',
      path: 'Notes/Jodd-Demo',
    });
    expect(result).toEqual(fetched);
  });

  // A folder whose own name merely contains a sentinel is a real folder.
  // Matching by substring rather than equality would hide it from every
  // refresh path with no error to explain the staleness.
  it('treats a real folder whose name embeds a sentinel as fetchable', async () => {
    invoke.mockResolvedValueOnce([]);
    await fetchFolderNotes('gmail:a@b.com', 'Notes/__TRASH__ notes');
    expect(invoke).toHaveBeenCalledWith('list_notes_in_folder', {
      accountId: 'gmail:a@b.com',
      path: 'Notes/__TRASH__ notes',
    });
  });
});

describe('isFolderGone', () => {
  it('reads the backend answer for a selected subfolder', () => {
    expect(isFolderGone('Folder not found: Notes/Old', 'Notes/Old')).toBe(true);
    expect(isFolderGone('Folder not found: Notes/Old', 'Notes/Other')).toBe(false);
  });

  // The root is what a gone folder falls back to; swallowing its own
  // not-found would hide the error behind a re-select of itself.
  it('never treats the root as gone', () => {
    expect(isFolderGone('Folder not found: Notes', 'Notes')).toBe(false);
  });
});
