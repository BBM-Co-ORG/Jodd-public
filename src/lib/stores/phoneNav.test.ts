// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { get } from 'svelte/store';

// isPhoneNavActive() gates every exported function on `get(isAndroid) &&
// get(androidLayoutMode) === 'phone'`. Both are backed by a Tauri `invoke`
// call (platform.ts) / window.innerWidth (viewport.ts) that don't resolve to
// anything useful under vitest, so every navigateToPane/handlePopState call
// would silently no-op without this — the mocks make the module think it's
// always running on an Android phone.
vi.mock('./platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(true) };
});
vi.mock('./viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import {
  paneFromHistoryState,
  depthFromHistoryState,
  navigateToPane,
  initPhoneNavHistory,
  handlePopState,
  activePane,
} from './phoneNav';
// The mocked stores above are module-level singletons — importing them here
// gets the SAME writable instances `phoneNav.ts` reads via isPhoneNavActive(),
// so mutating androidLayoutMode from the test simulates a device rotation
// (phone <-> tablet breakpoint crossing) mid-navigation. The real module
// types this Readable (it's a `derived` store there); the mock substitutes a
// plain writable, so the cast reflects what's actually mocked in, not the
// real module's shape.
import { androidLayoutMode as androidLayoutModeReadonly } from './viewport';
import type { Writable } from 'svelte/store';
const androidLayoutMode = androidLayoutModeReadonly as unknown as Writable<'phone' | 'tablet'>;

describe('paneFromHistoryState', () => {
  it('reads a valid pane out of history.state', () => {
    expect(paneFromHistoryState({ pane: 'folders' })).toBe('folders');
    expect(paneFromHistoryState({ pane: 'list' })).toBe('list');
    expect(paneFromHistoryState({ pane: 'note' })).toBe('note');
  });

  it('defaults to list for missing or malformed state', () => {
    // null/undefined state happens on the very first history entry a page
    // ever had before any pushState — must not throw.
    expect(paneFromHistoryState(null)).toBe('list');
    expect(paneFromHistoryState(undefined)).toBe('list');
    expect(paneFromHistoryState({})).toBe('list');
    expect(paneFromHistoryState({ pane: 'not-a-real-pane' })).toBe('list');
    expect(paneFromHistoryState('a string, not an object')).toBe('list');
  });
});

describe('depthFromHistoryState', () => {
  it('reads a valid depth out of history.state', () => {
    expect(depthFromHistoryState({ depth: 0 })).toBe(0);
    expect(depthFromHistoryState({ depth: 2 })).toBe(2);
  });

  it('defaults to 0 for missing or malformed state', () => {
    expect(depthFromHistoryState(null)).toBe(0);
    expect(depthFromHistoryState(undefined)).toBe(0);
    expect(depthFromHistoryState({})).toBe(0);
    expect(depthFromHistoryState({ depth: -1 })).toBe(0);
    expect(depthFromHistoryState({ depth: 1.5 })).toBe(0);
    expect(depthFromHistoryState({ depth: 'two' })).toBe(0);
  });
});

/**
 * jsdom dispatches the popstate from history.go() asynchronously and, unlike
 * a real browser, not reliably within a single microtask/macrotask — polling
 * is the robust way to wait for it (confirmed empirically against this
 * project's jsdom 29.1.1: a bare `await Promise.resolve()` or a single
 * `setTimeout(fn, 0)` was not always enough).
 */
async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) {
      throw new Error('waitFor: condition did not become true in time');
    }
    await new Promise((r) => setTimeout(r, 10));
  }
}

describe('navigateToPane / handlePopState — Folders-as-root invariant', () => {
  beforeEach(() => {
    window.addEventListener('popstate', handlePopState);
    initPhoneNavHistory();
  });

  it('a single Folders visit replaces the root entry in place (no push)', () => {
    const lengthBefore = history.length;
    navigateToPane('folders');
    expect(get(activePane)).toBe('folders');
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(history.length).toBe(lengthBefore); // replaceState, not pushState
  });

  it(
    'REGRESSION: a second Folders visit after drilling in collapses back to ' +
      'the single root entry instead of stacking a duplicate — reproduces the ' +
      'code-review trace (browse folder A, back to list, browse folder B) that ' +
      'cost a silent 3rd back-press before the 4th finally exited',
    async () => {
      // 1. Folders (root, depth 0) — replaceState.
      navigateToPane('folders');
      expect(depthFromHistoryState(history.state)).toBe(0);

      // 2. Pick folder A -> list (depth 1) — pushState.
      navigateToPane('list');
      expect(get(activePane)).toBe('list');
      expect(depthFromHistoryState(history.state)).toBe(1);

      // 3. Tap Folders again from depth 1. Must NOT just replaceState the
      //    current (list) entry — that would leave the true root entry
      //    untouched underneath, producing two folders-shaped stack levels.
      //    Instead this triggers an async history.go(-1) unwind.
      navigateToPane('folders');
      await waitFor(() => get(activePane) === 'folders');
      expect(depthFromHistoryState(history.state)).toBe(0);
      expect(paneFromHistoryState(history.state)).toBe('folders');

      // 4. Pick folder B -> list (depth 1) -> note (depth 2).
      navigateToPane('list');
      expect(depthFromHistoryState(history.state)).toBe(1);
      navigateToPane('note');
      expect(depthFromHistoryState(history.state)).toBe(2);

      // 5. Back three times must land: note->list, list->folders, and the
      //    THIRD press must be the one that exits the app (i.e. there is no
      //    surviving second `folders` entry to silently absorb a press).
      //    We can't observe "the app exited" directly in jsdom, but we can
      //    assert the two presses land exactly on list then folders, and
      //    that folders is once again the sole depth-0 entry — so a 3rd
      //    press is guaranteed to leave application-tracked history
      //    entirely rather than hit a second, indistinguishable folders
      //    entry first.
      history.back();
      await waitFor(() => get(activePane) === 'list');
      expect(depthFromHistoryState(history.state)).toBe(1);

      history.back();
      await waitFor(() => get(activePane) === 'folders');
      expect(depthFromHistoryState(history.state)).toBe(0);
    },
  );

  it('re-entrant navigation to the already-active pane is a no-op (no push, no replace)', () => {
    navigateToPane('folders');
    const stateBefore = history.state;
    const lengthBefore = history.length;
    navigateToPane('folders'); // already active — must return early
    expect(history.state).toBe(stateBefore);
    expect(history.length).toBe(lengthBefore);
  });
});

describe('foldersResetInFlight does not latch permanently across a missed popstate', () => {
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
    initPhoneNavHistory();
  });

  it(
    'REGRESSION: a device-mode switch (phone -> tablet) while a Folders ' +
      'history.go() unwind is still in flight must not leave ' +
      'foldersResetInFlight stuck true forever — otherwise every later ' +
      'navigateToPane("folders") silently no-ops once back in phone mode',
    async () => {
      // Drill to depth 2 so a Folders tap takes the async go(-depth) branch.
      navigateToPane('folders'); // depth 0, replaceState
      navigateToPane('list'); // depth 1
      navigateToPane('note'); // depth 2
      expect(depthFromHistoryState(history.state)).toBe(2);

      // Trigger the unwind — sets the internal foldersResetInFlight guard and
      // fires a real (async, under jsdom) history.go(-2).
      navigateToPane('folders');

      // The device rotates mid-flight, crossing the tablet breakpoint,
      // BEFORE the go(-2)'s popstate lands. isPhoneNavActive() now reads
      // false for every handler invocation until it rotates back.
      androidLayoutMode.set('tablet');

      // The real popstate from go(-2) still fires — layout mode doesn't
      // stop the browser's own history mechanics — but handlePopState's
      // early return (isPhoneNavActive() false) used to skip clearing the
      // flag entirely, leaving it stuck true.
      await waitFor(() => depthFromHistoryState(history.state) === 0);

      // Rotate back to phone before re-testing the flag.
      androidLayoutMode.set('phone');

      // Build back up to depth > 0 so the next Folders tap again exercises
      // the foldersResetInFlight-gated async branch (the depth===0 fast
      // path wouldn't have exposed a stuck flag).
      navigateToPane('list');
      navigateToPane('note');
      expect(depthFromHistoryState(history.state)).toBe(2);

      // Without the fix, foldersResetInFlight is still (wrongly) true from
      // the missed landing above, so this call would take the
      // `!foldersResetInFlight` branch's false side and silently no-op —
      // activePane would never reach 'folders' and waitFor would time out.
      navigateToPane('folders');
      await waitFor(() => get(activePane) === 'folders');
      expect(depthFromHistoryState(history.state)).toBe(0);
    },
  );
});
