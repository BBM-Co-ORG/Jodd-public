// @vitest-environment jsdom
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
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
  pushOverlay,
  popOverlay,
  closeOverlayInto,
  overlayFromHistoryState,
  registerOverlayCloser,
  backLabel,
  setPaneTitle,
  popPane,
  hardwareBackFallback,
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

/**
 * A fresh page load: history.state is null, so init seeds synchronously. A
 * re-run init over an existing stack unwinds first (asynchronously) — that
 * path has its own test.
 */
function freshInit(listTitle?: string): void {
  history.replaceState(null, '', location.href);
  initPhoneNavHistory(listTitle);
}

describe('paneFromHistoryState', () => {
  it('reads a valid pane out of history.state', () => {
    expect(paneFromHistoryState({ pane: 'folders' })).toBe('folders');
    expect(paneFromHistoryState({ pane: 'tags' })).toBe('tags');
    expect(paneFromHistoryState({ pane: 'smart' })).toBe('smart');
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
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
    freshInit();
  });
  afterEach(async () => {
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it('Folders from the cold-start list unwinds to the single root, adding no entry', async () => {
    const lengthBefore = history.length;
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    expect(history.length).toBe(lengthBefore); // go(-1) + replaceState, no push
  });

  it(
    'REGRESSION: a second Folders visit after drilling in collapses back to ' +
      'the single root entry instead of stacking a duplicate — reproduces the ' +
      'code-review trace (browse folder A, back to list, browse folder B) that ' +
      'cost a silent 3rd back-press before the 4th finally exited',
    async () => {
      // 1. Folders (root, depth 0) — the cold start's list sits at depth 1,
      //    so this unwinds with go(-1).
      navigateToPane('folders');
      await waitFor(() => get(activePane) === 'folders');
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
      //    THIRD press must be the one that backgrounds the app (i.e. there
      //    is no surviving second `folders` entry to silently absorb a
      //    press). We can't observe the backgrounding directly in jsdom,
      //    but we can assert the two presses land exactly on list then
      //    folders, and that folders is once again the sole depth-0 entry —
      //    so a 3rd press is guaranteed to leave application-tracked history
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

  it('re-entrant navigation to the already-active pane is a no-op (no push, no replace)', async () => {
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
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
    freshInit();
  });
  afterEach(async () => {
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it(
    'REGRESSION: a device-mode switch (phone -> tablet) while a Folders ' +
      'history.go() unwind is still in flight must not leave ' +
      'foldersResetInFlight stuck true forever — otherwise every later ' +
      'navigateToPane("folders") silently no-ops once back in phone mode',
    async () => {
      // Drill to depth 2 so a Folders tap takes the async go(-depth) branch.
      // Cold start already sits on list at depth 1.
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
      // the missed landing above, so this call would be queued behind a
      // jump that never lands — activePane would show 'folders'
      // optimistically but history would never unwind.
      navigateToPane('folders');
      await waitFor(() => depthFromHistoryState(history.state) === 0);
      expect(get(activePane)).toBe('folders');
      expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    },
  );
});

// Task 4.1 (spec §5.1): the stack is always folders → [tags | smart] → list
// → note, and every Back names the entry below.
describe('cold start and the pane title stack', () => {
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
  });
  afterEach(async () => {
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it('cold start: folders(0) under list(1)', async () => {
    // A fresh page has no forward entries; an earlier test's Back leaves
    // some, and a push would discard them. Start at the tail.
    history.pushState(null, '', location.href);
    const lengthBefore = history.length;
    freshInit();
    expect(history.length).toBe(lengthBefore + 1);
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
    // Back from the cold-start list shows Folders — not the background.
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    expect(get(backLabel)).toBeNull();
  });

  it('cold start records the list title the caller passes', () => {
    freshInit('Inbox');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Inbox');
  });

  it('back label names the entry below', async () => {
    freshInit();
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    expect(get(backLabel)).toBeNull();
    navigateToPane('list', 'Projects');
    expect(get(backLabel)).toBe('Folders');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Projects');
    // popstate truncates the stack to the landed depth.
    history.back();
    await waitFor(() => get(activePane) === 'list');
    expect(get(backLabel)).toBe('Folders');
  });

  it('tags pane sits between folders and list', async () => {
    freshInit();
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    navigateToPane('tags', 'Tags');
    expect(history.state).toEqual({ pane: 'tags', depth: 1 });
    expect(get(backLabel)).toBe('Folders');
    navigateToPane('list', '#study');
    expect(history.state).toEqual({ pane: 'list', depth: 2 });
    expect(get(backLabel)).toBe('Tags');
    history.back();
    await waitFor(() => get(activePane) === 'tags');
    expect(depthFromHistoryState(history.state)).toBe(1);
    expect(get(backLabel)).toBe('Folders');
  });

  it('smart pane sits between folders and list', async () => {
    freshInit();
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    navigateToPane('smart', 'Smart folders');
    navigateToPane('list', 'Stale');
    expect(get(backLabel)).toBe('Smart folders');
    history.back();
    await waitFor(() => get(activePane) === 'smart');
    expect(depthFromHistoryState(history.state)).toBe(1);
  });

  it('go(-depth) from note via tags lands on folders', async () => {
    freshInit();
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    navigateToPane('tags', 'Tags');
    navigateToPane('list', '#study');
    navigateToPane('note');
    expect(depthFromHistoryState(history.state)).toBe(3);
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    expect(get(backLabel)).toBeNull();
    // The titles above the root are gone: a fresh drill-in starts clean.
    navigateToPane('list');
    navigateToPane('note');
    expect(get(backLabel)).toBe('');
  });

  it('setPaneTitle renames an entry and the back label follows', () => {
    freshInit('Notes');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Notes');
    setPaneTitle(1, 'Renamed');
    expect(get(backLabel)).toBe('Renamed');
  });

  it('an open modal does not change the back label: overlay depths are skipped', async () => {
    freshInit('Projects');
    navigateToPane('note'); // depth 2
    pushOverlay('ov-title'); // depth 3, an overlay
    expect(get(backLabel)).toBe('Projects');
    pushOverlay('ov-title-2'); // depth 4, a stacked overlay
    expect(get(backLabel)).toBe('Projects');
    popOverlay('ov-title-2');
    popOverlay('ov-title');
    await waitFor(() => depthFromHistoryState(history.state) === 2);
    expect(get(backLabel)).toBe('Projects');
  });

  it('closeOverlayInto turns the overlay depth into a titled pane', async () => {
    freshInit('Projects');
    pushOverlay('ov-into'); // over list, depth 2
    expect(get(backLabel)).toBe('Folders');
    closeOverlayInto('ov-into', 'note', 'Draft');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(backLabel)).toBe('Projects');
    // Depth 2 is now a pane titled 'Draft': whatever is pushed above it
    // names it on its Back.
    navigateToPane('list'); // depth 3
    expect(get(backLabel)).toBe('Draft');
  });

  it('a pane push queued behind an in-flight back records its title when it runs', async () => {
    freshInit();
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    pushOverlay('ov-q'); // depth 1
    popOverlay('ov-q'); // back in flight
    navigateToPane('list', 'Queued'); // queued
    expect(get(activePane)).toBe('list'); // optimistic
    await waitFor(() => paneFromHistoryState(history.state) === 'list' && overlayFromHistoryState(history.state) === null);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(backLabel)).toBe('Folders');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Queued');
  });

  it('a title passed to the already-active pane renames it in place', () => {
    freshInit('Old');
    const lengthBefore = history.length;
    navigateToPane('list', 'New'); // already active: no entry
    expect(history.length).toBe(lengthBefore);
    navigateToPane('note');
    expect(get(backLabel)).toBe('New');
  });

  it('a rename of the showing pane while a modal back is in flight lands on its entry', async () => {
    freshInit('Old');
    pushOverlay('m'); // depth 2 over the list
    popOverlay('m'); // back in flight
    navigateToPane('list', 'New'); // list is showing: a rename, queued
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 }); // nothing pushed
    navigateToPane('note');
    expect(get(backLabel)).toBe('New');
  });

  it('a queued rename is dropped when the jump lands on another pane', async () => {
    freshInit('Old');
    navigateToPane('note'); // depth 2
    navigateToPane('folders'); // unwind in flight; activePane still 'note'
    navigateToPane('note', 'Renamed'); // showing pane: a rename, queued
    await waitFor(() => get(activePane) === 'folders');
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'folders', depth: 0 }); // no push
    expect(get(backLabel)).toBeNull();
  });

  it('a second init over an existing stack unwinds to one root, not [folders, folders, list]', async () => {
    freshInit('First'); // seeds [folders, list] at the tail of history
    const seededLength = history.length;
    navigateToPane('note'); // depth 2
    pushOverlay('stale'); // depth 3
    initPhoneNavHistory('Second'); // e.g. sign-out → sign-in without reload
    expect(get(activePane)).toBe('list'); // shown at once
    expect(get(backLabel)).toBe('Folders');
    await waitFor(() => paneFromHistoryState(history.state) === 'list' && depthFromHistoryState(history.state) === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    // Same two entries as the first seed: seeding on top of the old stack
    // would have left [folders, list, note, folders, list].
    expect(history.length).toBe(seededLength);
    expect(get(backLabel)).toBe('Folders');
    navigateToPane('note');
    expect(get(backLabel)).toBe('Second');
    // Exactly one root below the list: Back twice reaches it, at depth 0.
    history.back();
    await waitFor(() => get(activePane) === 'list');
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
  });

  it('a pane op issued during a re-init unwind replays above the seeded list', async () => {
    freshInit();
    navigateToPane('note'); // depth 2
    initPhoneNavHistory('Again');
    navigateToPane('note'); // queued behind the unwind
    expect(get(activePane)).toBe('note');
    await waitFor(() => depthFromHistoryState(history.state) === 2);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(backLabel)).toBe('Again');
  });

  it('closeOverlayInto the pane beneath records its title', async () => {
    freshInit('Old');
    pushOverlay('ov-same'); // over the list, depth 2
    closeOverlayInto('ov-same', 'list', 'New');
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    navigateToPane('note');
    expect(get(backLabel)).toBe('New');
  });

  it('Folders at depth 0 relabels the root in place (replaceState fast path)', async () => {
    // The current entry is the root (depth 0) while another pane shows.
    // (This used to be reached by a tablet start rotated to phone; that
    // rotation now builds the cold-start stack — see 'rotation between
    // phone and tablet'.)
    freshInit();
    history.back();
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    activePane.set('list');
    const lengthBefore = history.length;
    navigateToPane('folders');
    expect(get(activePane)).toBe('folders'); // synchronous: no go()
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    expect(history.length).toBe(lengthBefore);
    expect(get(backLabel)).toBeNull();
  });

  it('an entry deeper than any recorded title reads as an untitled pane', () => {
    // A phone entry reached before init ever ran (e.g. state restored at
    // depth 3): the slots below it are filled with '', not left as holes.
    freshInit();
    history.replaceState({ pane: 'note', depth: 3 }, '', location.href);
    activePane.set('note');
    setPaneTitle(0, 'Folders');
    setPaneTitle(3, 'Deep');
    expect(get(backLabel)).not.toBeUndefined();
    navigateToPane('list', 'Pushed'); // depth 4
    expect(get(backLabel)).toBe('Deep');
    pushOverlay('ov-hole'); // depth 5: skipped
    expect(get(backLabel)).toBe('Deep');
    popOverlay('ov-hole');
  });
});

// Overlay entries (Modal, Task 2.1). An open phone modal owns one history
// entry above the pane beneath it, so Android Back closes the modal and not
// the pane (gotcha #31: the root Back path stays moveTaskToBack, untouched).
describe('overlay history entries', () => {
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
    freshInit();
  });
  afterEach(async () => {
    // Let any in-flight history.back() land before the next test re-inits.
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it('pushOverlay stacks {pane beneath, depth + 1, overlay} and leaves activePane alone', () => {
    navigateToPane('list'); // no-op: already list at depth 1
    navigateToPane('note'); // depth 2
    pushOverlay('ov-a');
    expect(history.state).toEqual({ pane: 'note', depth: 3, overlay: 'ov-a' });
    expect(get(activePane)).toBe('note');
    expect(paneFromHistoryState(history.state)).toBe('note');
    expect(overlayFromHistoryState(history.state)).toBe('ov-a');
    popOverlay('ov-a');
  });

  it('overlayFromHistoryState reads only a string overlay id', () => {
    expect(overlayFromHistoryState({ pane: 'list', depth: 1, overlay: 'x' })).toBe('x');
    expect(overlayFromHistoryState({ pane: 'list', depth: 1 })).toBeNull();
    expect(overlayFromHistoryState({ overlay: 3 })).toBeNull();
    expect(overlayFromHistoryState(null)).toBeNull();
  });

  it('popOverlay goes back exactly once and the resulting popstate reaches no closer', async () => {
    const closer = vi.fn(async () => true);
    const off = registerOverlayCloser('ov-b', closer);
    pushOverlay('ov-b');
    expect(depthFromHistoryState(history.state)).toBe(2);
    popOverlay('ov-b');
    popOverlay('ov-b'); // second call is a no-op: the entry is already gone
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(depthFromHistoryState(history.state)).toBe(1);
    expect(closer).not.toHaveBeenCalled();
    off();
  });

  it('a Back that pops the overlay entry routes to its closer; false re-pushes the entry', async () => {
    let allow = false;
    const closer = vi.fn(async () => allow);
    const off = registerOverlayCloser('ov-c', closer);
    pushOverlay('ov-c');
    history.back();
    await waitFor(() => closer.mock.calls.length === 1);
    await waitFor(() => overlayFromHistoryState(history.state) === 'ov-c');
    expect(depthFromHistoryState(history.state)).toBe(2);
    allow = true;
    history.back();
    await waitFor(() => closer.mock.calls.length === 2);
    await new Promise((r) => setTimeout(r, 30));
    expect(depthFromHistoryState(history.state)).toBe(1);
    expect(overlayFromHistoryState(history.state)).toBeNull();
    off();
  });

  it('two popOverlay calls in one turn unwind both entries, one back at a time', async () => {
    pushOverlay('outer');
    pushOverlay('inner');
    expect(depthFromHistoryState(history.state)).toBe(3);
    popOverlay('inner');
    popOverlay('outer');
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(depthFromHistoryState(history.state)).toBe(1);
  });

  it('a pushOverlay issued while our own back is in flight lands after it, not under it', async () => {
    pushOverlay('alert');
    popOverlay('alert'); // back in flight
    pushOverlay('again'); // must wait for the back to land
    await waitFor(() => overlayFromHistoryState(history.state) === 'again');
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 2, overlay: 'again' });
    popOverlay('again');
  });

  it('closeOverlayInto replaces the overlay entry with {pane, same depth}; Back lands beneath the modal', async () => {
    // Cold start: list at depth 1.
    pushOverlay('ask'); // depth 2
    const lengthBefore = history.length;
    closeOverlayInto('ask', 'note');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(history.length).toBe(lengthBefore);
    expect(get(activePane)).toBe('note');
    popOverlay('ask'); // no-op: the entry is no longer an overlay
    history.back();
    await waitFor(() => get(activePane) === 'list');
    expect(depthFromHistoryState(history.state)).toBe(1);
  });

  it('navigateToPane("folders") from under an overlay unwinds to depth 0', async () => {
    navigateToPane('note'); // depth 2
    pushOverlay('ov-d'); // depth 3
    expect(get(activePane)).toBe('note');
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders');
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(overlayFromHistoryState(history.state)).toBeNull();
  });

  it('rotation to tablet: a Back that pops the overlay entry still reaches its closer', async () => {
    const closer = vi.fn(async () => true);
    const off = registerOverlayCloser('ov-e', closer);
    pushOverlay('ov-e');
    androidLayoutMode.set('tablet');
    history.back();
    await waitFor(() => closer.mock.calls.length === 1);
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    // Closed, so no re-push; and the tablet keeps no pane entries, so the
    // list entry beneath went too (live pass 2026-10-21) — the next Back
    // backgrounds the app instead of being a dead press.
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(overlayFromHistoryState(history.state)).toBeNull();
    androidLayoutMode.set('phone');
    off();
  });

  it('popOverlay then navigateToPane in the same turn: the push waits for the back', async () => {
    pushOverlay('ov-f'); // {list,1} → overlay depth 2
    popOverlay('ov-f'); // back in flight
    navigateToPane('note');
    expect(get(activePane)).toBe('note'); // optimistic
    await waitFor(() => overlayFromHistoryState(history.state) === null && depthFromHistoryState(history.state) === 2);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
  });

  it('popOverlay then navigateToPane("folders") in the same turn unwinds from the landed depth', async () => {
    navigateToPane('note'); // depth 2
    pushOverlay('ov-g'); // depth 3
    popOverlay('ov-g'); // back in flight → depth 2
    navigateToPane('folders'); // must use depth 2, not the stale 3
    await waitFor(() => paneFromHistoryState(history.state) === 'folders');
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
    expect(get(activePane)).toBe('folders');
    // Not latched: the next drill-in pushes normally.
    navigateToPane('list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
  });

  it('closeOverlayInto after a rotation off the phone removes the entry instead of leaving it', async () => {
    pushOverlay('ov-h'); // depth 2
    androidLayoutMode.set('tablet');
    closeOverlayInto('ov-h', 'note');
    // The overlay's entry goes, and with it the phone panes beneath (the
    // tablet keeps none).
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    expect(overlayFromHistoryState(history.state)).toBeNull();
    expect(depthFromHistoryState(history.state)).toBe(0);
    // The destination is remembered: back on the phone, the note shows
    // above its list.
    expect(get(activePane)).toBe('note');
    androidLayoutMode.set('phone');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
  });
});

// Final review 2026-10-20, Important #1: phone Settings › Accounts › an
// account › Remove. removeAccount's selectedFolder.set('Notes') fires App's
// folder watcher (navigateToPane('list')) while three Settings pages own the
// top three entries, and the Account page's own close (popOverlay) goes back
// in the same turn. A push above the overlays used to be what that back
// removed, leaving a dead overlay entry and a user Back that closed nothing.
describe('a pane navigation under open modals waits for them', () => {
  const closed: string[] = [];
  const offs: Array<() => void> = [];

  beforeEach(async () => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
    freshInit('Notes');
    // Back from the cold-start list to Folders, where the gear lives.
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    closed.length = 0;
    for (const id of ['root', 'accounts', 'account']) {
      pushOverlay(id);
      offs.push(registerOverlayCloser(id, async () => { closed.push(id); return true; }));
    }
    expect(depthFromHistoryState(history.state)).toBe(3);
  });
  afterEach(async () => {
    for (const off of offs.splice(0)) off();
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  /** Asserts the stack after the Account page closed, then backs out of Settings. */
  async function expectSettingsStillOwnsBack(): Promise<void> {
    await waitFor(() => overlayFromHistoryState(history.state) === 'accounts');
    expect(depthFromHistoryState(history.state)).toBe(2);
    // The list shows under Settings at once (local-first)…
    expect(get(activePane)).toBe('list');
    // …and the user's Back closes the Accounts page, not a pane entry.
    history.back();
    await waitFor(() => closed.length === 1);
    expect(closed).toEqual(['accounts']);
    await waitFor(() => overlayFromHistoryState(history.state) === 'root');
    expect(get(activePane)).toBe('list');
    // Back closes Settings; only then is the list pushed, above Folders.
    history.back();
    await waitFor(() => closed.length === 2);
    expect(closed).toEqual(['accounts', 'root']);
    await waitFor(() => paneFromHistoryState(history.state) === 'list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
    // And Back from that list is a live press: it lands on Folders.
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    expect(depthFromHistoryState(history.state)).toBe(0);
  }

  it('watcher push, then the Account page closes (same turn)', async () => {
    navigateToPane('list', 'Notes');
    popOverlay('account');
    await expectSettingsStillOwnsBack();
  });

  it('the Account page closes, then the watcher push (same turn, queued)', async () => {
    popOverlay('account');
    navigateToPane('list', 'Notes');
    await expectSettingsStillOwnsBack();
  });

  it("a rename while the list waits names the waiting push, not the pane under the modals", async () => {
    navigateToPane('list', 'Notes');
    // App's $listTitle watcher renames the active list as the view resolves.
    navigateToPane('list', 'All Kaiwan');
    popOverlay('account');
    await waitFor(() => overlayFromHistoryState(history.state) === 'accounts');
    popOverlay('accounts');
    popOverlay('root');
    await waitFor(() => paneFromHistoryState(history.state) === 'list');
    // Folders kept its own title; the list got the renamed one.
    expect(get(backLabel)).toBe('Folders');
    navigateToPane('note');
    expect(get(backLabel)).toBe('All Kaiwan');
  });

  it('a modal that closes INTO a destination drops the waiting navigation', async () => {
    navigateToPane('list', 'Notes');
    closeOverlayInto('account', 'note');
    expect(history.state).toEqual({ pane: 'note', depth: 3 });
    expect(get(activePane)).toBe('note');
  });
});

// Live pass 2026-10-21 (Galaxy S23 FE, landscape ≥ 700 CSS px = tablet
// layout): Folders → a folder → a note, rotate to landscape, Back. The first
// two presses did nothing visible — they popped the phone stack's note and
// list entries, which tablet ignores — and the third backgrounded the app.
// Back in portrait the note pane showed with no Back button and Back
// backgrounded at once: the history was at the root while activePane still
// said 'note'. The model now: the tablet layout has no pane stack, so a flip
// to it collapses the phone entries to the root (tablet Back closes a modal
// or the drawer, else backgrounds — as on a tablet cold start); a flip back
// to phone rebuilds folders → [tags | smart] → list → note for the pane the
// user is on.
describe('rotation between phone and tablet', () => {
  const offs: Array<() => void> = [];
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
  });
  afterEach(async () => {
    for (const off of offs.splice(0)) off();
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  /** Folders → "Test" → a note: the device's path. */
  async function openNoteFromFolder(): Promise<void> {
    freshInit('Notes');
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders' && depthFromHistoryState(history.state) === 0);
    navigateToPane('list', '_test_');
    navigateToPane('note');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
  }

  it('phone → tablet collapses the pane entries, so tablet Back backgrounds at once', async () => {
    await openNoteFromFolder();
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(overlayFromHistoryState(history.state)).toBeNull();
    // The pane the user is on is remembered for the way back.
    expect(get(activePane)).toBe('note');
  });

  it('tablet → phone rebuilds the stack for the open note: Back reaches the list, then Folders', async () => {
    await openNoteFromFolder();
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    androidLayoutMode.set('phone');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
    expect(get(backLabel)).toBe('_test_');
    history.back();
    await waitFor(() => get(activePane) === 'list');
    expect(get(backLabel)).toBe('Folders');
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(get(backLabel)).toBeNull();
  });

  it('a note opened on the tablet shows on the phone with a Back to its list', async () => {
    freshInit('Notes');
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    // App's watchers still run on the tablet.
    navigateToPane('list', 'Projects');
    navigateToPane('note');
    expect(depthFromHistoryState(history.state)).toBe(0); // no stack on tablet
    androidLayoutMode.set('phone');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(backLabel)).toBe('Projects');
  });

  it('a note closed on the tablet (popPane) comes back as its list', async () => {
    await openNoteFromFolder();
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    popPane('note');
    androidLayoutMode.set('phone');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
  });

  it('a tablet cold start rotated to phone gets the cold-start stack', async () => {
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    freshInit('Inbox');
    expect(depthFromHistoryState(history.state)).toBe(0);
    androidLayoutMode.set('phone');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
  });

  it('a modal open across phone → tablet keeps its entry; the collapse waits for it to close', async () => {
    await openNoteFromFolder();
    const closer = vi.fn(async () => true);
    offs.push(registerOverlayCloser('rot-a', closer));
    pushOverlay('rot-a'); // depth 3
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    // Not unwound under the open modal: Back still closes it first.
    expect(overlayFromHistoryState(history.state)).toBe('rot-a');
    history.back();
    await waitFor(() => closer.mock.calls.length === 1);
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(get(activePane)).toBe('note');
  });

  it('tablet → phone under an open modal: the pane is kept, the stack is built once it closes', async () => {
    await openNoteFromFolder();
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    const closer = vi.fn(async () => true);
    offs.push(registerOverlayCloser('rot-b', closer));
    pushOverlay('rot-b'); // depth 1, above the root
    androidLayoutMode.set('phone');
    expect(get(activePane)).toBe('note');
    expect(overlayFromHistoryState(history.state)).toBe('rot-b');
    history.back();
    await waitFor(() => closer.mock.calls.length === 1);
    await waitFor(() => depthFromHistoryState(history.state) === 2);
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
    expect(get(backLabel)).toBe('_test_');
  });

  it('a modal open through a whole round trip leaves the phone stack beneath it untouched', async () => {
    await openNoteFromFolder();
    const closer = vi.fn(async () => true);
    offs.push(registerOverlayCloser('rot-c', closer));
    pushOverlay('rot-c'); // depth 3
    const length = history.length;
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    history.back(); // closes the modal
    await waitFor(() => closer.mock.calls.length === 1);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(history.length).toBe(length); // nothing unwound or re-pushed
    expect(get(backLabel)).toBe('_test_');
  });

  it('a round trip with no navigation in between leaves the phone stack as it was', async () => {
    await openNoteFromFolder();
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    androidLayoutMode.set('phone');
    // Navigation after the round trip pushes normally above the rebuilt note.
    history.back();
    await waitFor(() => get(activePane) === 'list');
    navigateToPane('note');
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(backLabel)).toBe('_test_');
  });

  // Review of the rotation model (2026-10-21): three sequences that left
  // history and activePane out of step.
  it('review 1: a second round trip under a modal does not mistake a never-built stack for an intact one', async () => {
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    freshInit('Inbox'); // tablet cold start: no phone stack yet
    offs.push(registerOverlayCloser('rev-1', async () => true));
    pushOverlay('rev-1'); // [root, modal]
    androidLayoutMode.set('phone');
    androidLayoutMode.set('tablet');
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    history.back(); // closes the modal
    await waitFor(() => depthFromHistoryState(history.state) === 1 && overlayFromHistoryState(history.state) === null);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
  });

  it('review 2: a pane push deferred under a modal survives a round trip', async () => {
    freshInit('Notes'); // [folders, list]
    offs.push(registerOverlayCloser('rev-2', async () => true));
    pushOverlay('rev-2'); // depth 2
    navigateToPane('note'); // deferred: the modal owns the top entry
    expect(get(activePane)).toBe('note');
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    history.back(); // closes the modal
    await waitFor(() => paneFromHistoryState(history.state) === 'note' && overlayFromHistoryState(history.state) === null);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
    expect(get(backLabel)).toBe('Notes');
  });

  it('review 2: a pane push queued behind a jump survives a flip to tablet', async () => {
    freshInit('Notes');
    history.back();
    await waitFor(() => get(activePane) === 'folders');
    pushOverlay('rev-2b'); // depth 1
    popOverlay('rev-2b'); // back in flight
    navigateToPane('list', 'Projects'); // queued behind it
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 50));
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');
  });

  it('review 2: a pop deferred under a modal lands its parent after a round trip', async () => {
    await openNoteFromFolder(); // [folders, list, note]
    offs.push(registerOverlayCloser('rev-2c', async () => true));
    pushOverlay('rev-2c'); // depth 3
    popPane('note'); // the note was deleted under the modal: waits for it
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    history.back(); // closes the modal
    await waitFor(() => get(activePane) === 'list' && overlayFromHistoryState(history.state) === null);
    await new Promise((r) => setTimeout(r, 50));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(backLabel)).toBe('Folders');
  });

  it('review 3: a modal left live under a pane entry is not unwound from under (no stuck history)', async () => {
    await openNoteFromFolder(); // depth 2
    offs.push(registerOverlayCloser('rev-3a', async () => true));
    pushOverlay('rev-3a'); // depth 3
    pushOverlay('rev-3b'); // depth 4
    closeOverlayInto('rev-3b', 'list', 'Elsewhere'); // B becomes a pane entry above live A
    // A back issued at the root never pops anything on a device (the WebView
    // has no entry below it), so its ignored popstate never arrives and
    // every later push and navigation queues forever. jsdom has earlier
    // tests' entries below this root, so assert the invariant directly.
    const backDepths: number[] = [];
    const realBack = history.back.bind(history);
    const spy = vi.spyOn(history, 'back').mockImplementation(() => {
      backDepths.push(depthFromHistoryState(history.state));
      realBack();
    });
    androidLayoutMode.set('tablet');
    await new Promise((r) => setTimeout(r, 30));
    popOverlay('rev-3a'); // A closes from its own UI
    await new Promise((r) => setTimeout(r, 80));
    spy.mockRestore();
    expect(backDepths).not.toContain(0);
    // History is not wedged: a new modal still gets its entry.
    pushOverlay('rev-3c');
    await waitFor(() => overlayFromHistoryState(history.state) === 'rev-3c');
    popOverlay('rev-3c');
    await waitFor(() => overlayFromHistoryState(history.state) === null);
  });
});

/**
 * Live pass 2026-10-21 (S23 FE, first launch after an update): What's New
 * opens from App's onMount BEFORE is_authenticated returns, so its Modal
 * pushes its entry before initPhoneNavHistory runs. Init then took that
 * overlay entry for an old phone stack: it forgot the modal (Back no longer
 * closed it) and unwound with go(-1), whose popstate arrived before App had
 * added its popstate listener — so the unwind never "landed", every later
 * navigation queued behind it, and "‹ Folders" did nothing until a restart.
 */
describe('a modal opened before init (What\'s New at startup)', () => {
  const offs: Array<() => void> = [];
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
  });
  afterEach(async () => {
    await new Promise((r) => setTimeout(r, 30));
    for (const off of offs.splice(0)) off();
    window.removeEventListener('popstate', handlePopState);
  });

  function openBeforeInit(id: string, onClose: () => void): void {
    history.replaceState(null, '', location.href); // a fresh page load
    offs.push(
      registerOverlayCloser(id, async () => {
        onClose();
        return true;
      }),
    );
    pushOverlay(id);
    initPhoneNavHistory('Notes');
  }

  it('Android Back closes it, then shows the cold-start list over Folders', async () => {
    let closed = 0;
    openBeforeInit('wn-startup-a', () => closed++);
    await new Promise((r) => setTimeout(r, 30));
    // The modal still owns the top entry; the list shows beneath it.
    expect(overlayFromHistoryState(history.state)).toBe('wn-startup-a');
    expect(get(activePane)).toBe('list');

    history.back(); // Android Back
    await waitFor(() => closed === 1);
    await waitFor(() => paneFromHistoryState(history.state) === 'list' && depthFromHistoryState(history.state) === 1);
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(activePane)).toBe('list');
    expect(get(backLabel)).toBe('Folders');

    history.back(); // Back from the list shows Folders, not the background
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
  });

  it('its own "‹ Back" closes it, and "‹ Folders" then works', async () => {
    openBeforeInit('wn-startup-b', () => {});
    await new Promise((r) => setTimeout(r, 30));
    expect(overlayFromHistoryState(history.state)).toBe('wn-startup-b');
    popOverlay('wn-startup-b'); // the header's ‹ Back
    await waitFor(() => paneFromHistoryState(history.state) === 'list' && overlayFromHistoryState(history.state) === null);
    await new Promise((r) => setTimeout(r, 30));
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(backLabel)).toBe('Folders');

    navigateToPane('folders'); // the list header's ‹ Folders
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
  });
});

/**
 * Live pass 2026-10-21 (S23 FE): in landscape (tablet layout) open a note,
 * Back closes the drawer, Back backgrounds; the portrait-only launcher flips
 * the layout while Jodd is away, and the rebuild pushes folders → list → note
 * with no tap since the last Back. Chromium's history-manipulation
 * intervention marks such entries skippable for the BROWSER's back — the
 * WebView's canGoBack()/goBack() — while a script's history.back() is not
 * affected. So canGoBack() said false, Tauri's Back handler fell through to
 * MainActivity's moveTaskToBack, and Jodd backgrounded with "‹ _test_" on
 * screen. MainActivity now asks the page first (window.__joddHardwareBack);
 * jsdom has no intervention, so these pin the page side of that contract.
 */
describe('hardwareBackFallback — Android Back the WebView refused', () => {
  beforeEach(() => {
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
  });
  afterEach(async () => {
    androidLayoutMode.set('phone');
    await new Promise((r) => setTimeout(r, 30));
    window.removeEventListener('popstate', handlePopState);
  });

  it('at the root it declines, so MainActivity backgrounds the app (gotcha #31)', async () => {
    freshInit('Notes');
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders' && depthFromHistoryState(history.state) === 0);
    const spy = vi.spyOn(history, 'back');
    expect(hardwareBackFallback()).toBe(false);
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });

  it('above the root it goes back one entry, as Back would', async () => {
    freshInit('Notes'); // the cold-start list over Folders
    expect(hardwareBackFallback()).toBe(true);
    await waitFor(() => get(activePane) === 'folders');
    expect(history.state).toEqual({ pane: 'folders', depth: 0 });
  });

  it('after a tablet → phone flip, the first Back pops the rebuilt note to its list', async () => {
    freshInit('Notes');
    navigateToPane('folders');
    await waitFor(() => get(activePane) === 'folders' && depthFromHistoryState(history.state) === 0);
    navigateToPane('list', '_test_');
    androidLayoutMode.set('tablet');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await new Promise((r) => setTimeout(r, 30));
    navigateToPane('note'); // a different note, opened on the tablet
    androidLayoutMode.set('phone'); // the launcher's portrait, while away
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(backLabel)).toBe('_test_');
    expect(hardwareBackFallback()).toBe(true);
    await waitFor(() => get(activePane) === 'list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
    expect(get(backLabel)).toBe('Folders');
  });

  it('closes a modal opened before init (What\'s New at startup)', async () => {
    history.replaceState(null, '', location.href);
    let closed = 0;
    const off = registerOverlayCloser('wn-hw', async () => {
      closed++;
      return true;
    });
    pushOverlay('wn-hw');
    initPhoneNavHistory('Notes');
    expect(hardwareBackFallback()).toBe(true);
    await waitFor(() => closed === 1);
    off();
  });
});
