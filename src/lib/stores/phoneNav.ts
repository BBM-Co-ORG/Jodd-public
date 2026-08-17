import { writable, get } from 'svelte/store';
import type { Writable } from 'svelte/store';
import { isAndroid } from './platform';
import { androidLayoutMode } from './viewport';

export type Pane = 'folders' | 'list' | 'note';

const VALID_PANES: readonly Pane[] = ['folders', 'list', 'note'];

/** Which single pane is visible on the phone layout. Ignored on tablet/desktop. */
export const activePane: Writable<Pane> = writable('list');

/**
 * Pure so the history.state → Pane mapping is testable without a real
 * PopStateEvent. `history.state` is null on the page's very first load
 * (before any pushState/replaceState), so this must default sanely rather
 * than throw.
 */
export function paneFromHistoryState(state: unknown): Pane {
  if (state && typeof state === 'object' && 'pane' in state) {
    const p = (state as { pane: unknown }).pane;
    if (typeof p === 'string' && (VALID_PANES as string[]).includes(p)) {
      return p as Pane;
    }
  }
  return 'list';
}

/**
 * How many pushState calls deep the current entry is from the stack root
 * (root = 0). Stored alongside `pane` in every history entry so it survives
 * back/forward navigation — a module-level counter would desync the moment
 * the user pressed the physical back button, since that changes the
 * browser's position in the stack without running any of our push/replace
 * code. Same defensive default as `paneFromHistoryState`.
 */
export function depthFromHistoryState(state: unknown): number {
  if (state && typeof state === 'object' && 'depth' in state) {
    const d = (state as { depth: unknown }).depth;
    if (typeof d === 'number' && Number.isInteger(d) && d >= 0) return d;
  }
  return 0;
}

/** True only when the phone single-pane stack applies — tablet and desktop both show multiple panes and never consult `activePane`. */
export function isPhoneNavActive(): boolean {
  return get(isAndroid) && get(androidLayoutMode) === 'phone';
}

/**
 * Establishes the history root at `list` (the last-viewed folder, matching
 * desktop's existing selectedFolder persistence) without creating a
 * poppable-forward entry. Call exactly once, on the false→true transition
 * of $isAuthenticated — see App.svelte.
 */
export function initPhoneNavHistory(): void {
  if (!isPhoneNavActive()) return;
  history.replaceState({ pane: 'list', depth: 0 }, '', location.href);
  activePane.set('list');
  foldersResetInFlight = false;
}

/**
 * Set while a `navigateToPane('folders')` call from depth > 0 is waiting for
 * the `history.go()` it triggered to land — see `navigateToPane` below. Guards
 * against a second `navigateToPane('folders')` firing `go()` again before the
 * first one's popstate has arrived (e.g. a fast double-tap of the Folders
 * button), which would otherwise race two in-flight history jumps.
 */
let foldersResetInFlight = false;

/**
 * Pushes a new history entry for `pane`, unless we're already showing it —
 * re-entrant navigation (e.g. re-selecting the same folder) must not pile up
 * dead history entries the user then has to back through.
 *
 * `folders` is the exception: every visit must leave `folders` as the SOLE
 * entry at the root of the stack (index 0, depth 0) — it is the top of the
 * phone nav hierarchy, and `list`/`note` are the only two real "depths" a
 * user drills into. Two cases:
 *
 * - Already at the root (depth 0, e.g. straight from `initPhoneNavHistory`
 *   or a previous folders reset): a plain `replaceState` on the current
 *   entry is correct and synchronous.
 * - Deeper than the root (the user drilled into a folder, or a folder then a
 *   note): the History API can only replace the CURRENT top-of-stack entry,
 *   not an arbitrary older one, so a plain `replaceState` here would leave
 *   the true root entry (still `{pane:'list'}` or an earlier `{pane:
 *   'folders'}`) untouched underneath — reproducing the exact "extra dead
 *   entry" bug this function exists to prevent, just requiring a second
 *   Folders visit to trigger (confirmed by code review, 2026-08-03: browse
 *   folder A → back to folder list → browse folder B produces a second,
 *   indistinguishable `folders` entry, costing an extra silent back-press
 *   before exit). The fix: `history.go(-depth)` physically unwinds the
 *   browser back to entry 0, and once `handlePopState` observes that landing
 *   (depth 0), it relabels that now-current root entry as `folders` via
 *   `replaceState`. `go()` is asynchronous — a single popstate fires with
 *   the destination already reached (browsers jump directly, not one entry
 *   at a time) — so `activePane` updates one tick later in this branch
 *   only; no caller depends on the synchronous update (the one call site,
 *   the phone Folders button in App.svelte, is a plain click handler with no
 *   follow-on logic).
 *
 * Confirmed on-device (Galaxy S23 FE, Android 16, 2026-08-03): without any
 * of this, back from note took 4 presses to exit (note → list → folders → a
 * redundant list → exit) instead of the intended 3 (note → list → folders →
 * exit) on the FIRST folders visit — and, before this depth-aware fix, a
 * second folders visit in the same session reintroduced the identical bug
 * one level deeper.
 */
export function navigateToPane(pane: Pane): void {
  if (!isPhoneNavActive()) return;
  if (get(activePane) === pane) return;
  const currentDepth = depthFromHistoryState(history.state);
  if (pane === 'folders') {
    if (currentDepth === 0) {
      history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
      activePane.set('folders');
    } else if (!foldersResetInFlight) {
      foldersResetInFlight = true;
      history.go(-currentDepth);
      // activePane + the root-entry relabel happen in handlePopState once
      // the jump lands at depth 0 — see the branch there.
    }
  } else {
    history.pushState({ pane, depth: currentDepth + 1 }, '', location.href);
    activePane.set(pane);
  }
}

/** Wire to `window.addEventListener('popstate', handlePopState)` in App.svelte. */
export function handlePopState(e: PopStateEvent): void {
  if (!isPhoneNavActive()) {
    // A history.go(-depth) triggered by navigateToPane('folders') may still
    // be in flight when the device crosses the phone/tablet breakpoint
    // (rotation, window resize) — androidLayoutMode flips, isPhoneNavActive()
    // goes false, and the early return above used to swallow the landing
    // popstate without ever clearing the flag. Every navigateToPane('folders')
    // call after switching back to phone mode would then find
    // foldersResetInFlight still true and silently no-op forever. The flag's
    // only job is to guard against a SECOND go() firing while one is already
    // in flight — once we're not even tracking phone nav, there is nothing
    // left to guard, so it must reset here too.
    foldersResetInFlight = false;
    return;
  }
  if (foldersResetInFlight) {
    const depth = depthFromHistoryState(e.state);
    if (depth === 0) {
      foldersResetInFlight = false;
      history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
      activePane.set('folders');
      return;
    }
    // go(-currentDepth) is specced to land in one jump, so this branch
    // (still mid-unwind) is not expected in practice; fall through and
    // reflect whatever pane actually landed rather than getting stuck.
    foldersResetInFlight = false;
  }
  activePane.set(paneFromHistoryState(e.state));
}
