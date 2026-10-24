import { writable, get } from 'svelte/store';
import type { Readable, Writable } from 'svelte/store';
import { isAndroid } from './platform';
import { androidLayoutMode } from './viewport';

/**
 * The phone stack is always folders → [tags | smart] → list → note
 * (spec §5.1). `tags` and `smart` are one account's Tags and Smart folders
 * panes, drilled into from Folders.
 */
export type Pane = 'folders' | 'tags' | 'smart' | 'list' | 'note';

const VALID_PANES: readonly Pane[] = ['folders', 'tags', 'smart', 'list', 'note'];

/** The title a pane gets when its caller names none. */
const DEFAULT_TITLES: Record<Pane, string> = {
  folders: 'Folders',
  tags: 'Tags',
  smart: 'Smart folders',
  list: '',
  note: '',
};

/**
 * Which single pane is visible on the phone layout. The tablet layout does
 * not show it, but on Android it keeps recording the pane the user is on —
 * the pane a rotation back to phone shows (see "Rotation" below).
 */
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

/* ------------------------------------------------------------------------ *
 * Pane titles — what each Back button says (spec §5.1). Back is always
 * history.back(), so for its label to be honest it must name the entry
 * BELOW: `paneTitles[depth]` holds the title of the history entry at that
 * depth, written on every push and replace and truncated on popstate to the
 * landed depth + 1.
 *
 * An overlay entry (an open Modal) occupies a depth too, but it is not a
 * pane: its slot holds `null`, and `backLabel` skips null slots both when
 * finding the pane the user sees under any modals and when finding the pane
 * below that. So opening a modal never changes the label of the header it
 * covers, and Back from a modal (which closes it) is not mislabelled as a
 * pane.
 * ------------------------------------------------------------------------ */

/** Title by history depth; `null` marks an overlay entry's depth. */
const paneTitles: Array<string | null> = [];
const backLabelStore = writable<string | null>(null);

/**
 * The label for the visible pane's Back button: the title of the nearest
 * pane entry below it, or `null` at the root (no Back button — root Back
 * backgrounds the app via MainActivity's moveTaskToBack, gotcha #31). An
 * untitled pane below yields `''`.
 */
export const backLabel: Readable<string | null> = { subscribe: backLabelStore.subscribe };

/** Nearest depth at or below `d` that is a pane, not an overlay. */
function paneDepthAtOrBelow(d: number): number {
  while (d > 0 && paneTitles[d] === null) d--;
  return d;
}

function refreshBackLabel(): void {
  const visible = paneDepthAtOrBelow(depthFromHistoryState(history.state));
  if (visible === 0) {
    backLabelStore.set(null);
    return;
  }
  backLabelStore.set(paneTitles[paneDepthAtOrBelow(visible - 1)] ?? '');
}

/**
 * Fills every missing slot below `depth` with `''`. A slot is missing when
 * a title is recorded deeper than any seen so far (e.g. an entry reached
 * before init, or setPaneTitle past the end); such an entry is an untitled
 * PANE, never an overlay, so it must read `''` and not be an
 * `undefined` hole that `paneDepthAtOrBelow` / `backLabel` would have to
 * special-case.
 */
function fillSlotsBelow(depth: number): void {
  while (paneTitles.length < depth) paneTitles.push('');
}

/** Records `title` for the entry at `depth`, discarding every slot above it. */
function recordEntry(depth: number, title: string | null): void {
  fillSlotsBelow(depth);
  paneTitles.length = depth;
  paneTitles[depth] = title;
  refreshBackLabel();
}

/** Forgets the titles of entries a popstate just removed. */
function truncateTitlesTo(landed: number): void {
  if (paneTitles.length > landed + 1) paneTitles.length = landed + 1;
}

/**
 * Renames the entry at `depth` without touching history — for a pane whose
 * name changes while it is on the stack (a renamed folder, a view whose name
 * resolves after the push).
 */
export function setPaneTitle(depth: number, title: string): void {
  fillSlotsBelow(depth);
  paneTitles[depth] = title;
  // The visible pane's own entry: remember its title for a rebuild.
  if (depth === paneDepthAtOrBelow(depthFromHistoryState(history.state))) {
    titleMemo[paneFromHistoryState(history.state)] = title;
  }
  refreshBackLabel();
}

/** True only when the phone single-pane stack applies — tablet and desktop both show multiple panes and never consult `activePane`. */
export function isPhoneNavActive(): boolean {
  return get(isAndroid) && get(androidLayoutMode) === 'phone';
}

/**
 * Seeds the cold-start stack with TWO entries: `folders` as the root
 * (depth 0), then `list` (depth 1, the last-viewed folder, matching
 * desktop's existing selectedFolder persistence), and shows the list. Back
 * from the cold-start list therefore shows Folders rather than sending the
 * app to the background (spec §5.1) — leaving takes one more press than it
 * did when `list` was the root. Call exactly once, on the false→true
 * transition of $isAuthenticated — see App.svelte, which passes the list's
 * view name as `listTitle`.
 */
export function initPhoneNavHistory(listTitle?: string): void {
  if (!get(isAndroid)) return;
  armLayoutWatch();
  navArmed = true;
  const title = listTitle ?? DEFAULT_TITLES.list;
  for (const p of VALID_PANES) delete titleMemo[p];
  titleMemo.list = title;
  if (!isPhoneNavActive()) {
    // Started on the tablet layout, which keeps no pane stack. The pane is
    // the list, as on a phone; a rotation to phone builds its stack.
    activePane.set('list');
    phoneStackStale = true;
    tabletSpellChanged = true; // there never was a phone stack
    return;
  }
  if (onlyModalsAboveRoot()) {
    // Modals opened before init own every entry above the root — What's New,
    // which App opens on the first launch after an update before
    // is_authenticated returns. Seeding now would push the list ABOVE their
    // entries, or unwind them from under the open modals (live pass
    // 2026-10-21: Back no longer closed What's New, and the unwind left
    // "‹ Folders" dead). Show the list beneath them and build the stack once
    // no modal owns an entry: reconcileLayout runs on every landing, including
    // the one that closes the last of them. (A re-init over a phone stack
    // still unwinds it, modal entries included — below.)
    activePane.set('list');
    phoneStackStale = true;
    tabletSpellChanged = true;
    deferredPaneNav = null;
    forgetDeferredPanePop();
    refreshBackLabel();
    return;
  }
  phoneStackStale = false;
  // A fresh root has no overlay entries above it.
  liveOverlays.clear();
  pendingOps.length = 0;
  ignoredPops = 0;
  forgetDeferredPanePop();
  deferredPaneNav = null;
  paneTitles.length = 0;
  paneTitles.push(DEFAULT_TITLES.folders, title);
  activePane.set('list');
  foldersResetTitle = DEFAULT_TITLES.folders;

  // Called again over a stack we already built (sign-out → sign-in without
  // a reload re-arms App.svelte's lastAuthed): seeding on top would leave
  // the old entries underneath as a dead root — [folders, folders, list].
  // Unwind to the real root first with the Folders reset's go(-depth); its
  // landing in handlePopState seeds the list (seedListTitle). The list
  // shows at once (local-first); pane ops issued meanwhile are queued.
  const depth = depthFromHistoryState(history.state);
  if (depth > 0 && paneFromStateOrNull(history.state) !== null) {
    foldersResetInFlight = true;
    seedListTitle = title;
    history.go(-depth);
    // history.state is still the old deep entry: label the shown list
    // directly rather than derive it from that.
    backLabelStore.set(DEFAULT_TITLES.folders);
    return;
  }
  foldersResetInFlight = false;
  seedListTitle = null;
  seedRoot(title);
}

/**
 * Every entry above the root belongs to an open modal: no phone stack was
 * ever pushed beneath them (a modal opened before the first init).
 */
function onlyModalsAboveRoot(): boolean {
  const depth = depthFromHistoryState(history.state);
  if (depth === 0) return false;
  let owned = 0;
  for (const d of liveOverlays.values()) if (d > 0 && d <= depth) owned++;
  return owned === depth;
}

/** Relabels the current entry as the folders root and pushes the list above it. */
function seedRoot(listTitle: string): void {
  history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
  history.pushState({ pane: 'list', depth: 1 }, '', location.href);
  activePane.set('list');
  paneTitles.length = 0;
  paneTitles.push(DEFAULT_TITLES.folders, listTitle);
  refreshBackLabel();
}

/** The `pane` of a phoneNav history entry, or null for any other state. */
function paneFromStateOrNull(state: unknown): Pane | null {
  if (state && typeof state === 'object' && 'pane' in state) {
    const p = (state as { pane: unknown }).pane;
    if (typeof p === 'string' && (VALID_PANES as string[]).includes(p)) return p as Pane;
  }
  return null;
}

/**
 * Set while a `navigateToPane('folders')` call from depth > 0 is waiting for
 * the `history.go()` it triggered to land — see `navigateToPane` below. Guards
 * against a second `navigateToPane('folders')` firing `go()` again before the
 * first one's popstate has arrived (e.g. a fast double-tap of the Folders
 * button), which would otherwise race two in-flight history jumps.
 */
let foldersResetInFlight = false;
/** The root title to record when that unwind lands. */
let foldersResetTitle = DEFAULT_TITLES.folders;
/**
 * Set when the unwind in flight is a re-run `initPhoneNavHistory`: its
 * landing seeds the cold-start list above the root (with this title)
 * instead of showing Folders.
 */
let seedListTitle: string | null = null;

/**
 * Pushes a new history entry for `pane`, unless we're already showing it —
 * re-entrant navigation (e.g. re-selecting the same folder) must not pile up
 * dead history entries the user then has to back through.
 *
 * `title` is recorded at the depth the pane lands on — when the push is
 * queued behind an in-flight history jump, at the moment it actually runs —
 * and is what the Back button of the pane above it will say. Omitted, the
 * pane gets its default (`DEFAULT_TITLES`). On the already-active pane a
 * title renames it in place.
 *
 * `folders` is the exception: every visit must leave `folders` as the SOLE
 * entry at the root of the stack (index 0, depth 0) — it is the top of the
 * phone nav hierarchy, and `tags`/`smart`, `list` and `note` are the depths
 * a user drills into. Two cases:
 *
 * - Already at the root (depth 0, e.g. after a Back from the cold-start
 *   list, or a previous folders reset): a plain `replaceState` on the
 *   current entry is correct and synchronous.
 * - Deeper than the root (the cold-start list itself, or the user drilled
 *   into a folder, or a folder then a note): the History API can only
 *   replace the CURRENT top-of-stack entry, not an arbitrary older one, so a
 *   plain `replaceState` here would leave the true root entry untouched
 *   underneath — reproducing the exact "extra dead
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
 * "Exit" in the stack below means Jodd goes to the background, NOT that the
 * activity finishes: MainActivity.kt catches the Back that wry re-dispatches
 * once the webview has no history left and calls `moveTaskToBack`. A finished
 * activity makes tao `exit()` the process under running background work
 * (gotcha #31), so the stack's last step must never reach `finish()`.
 *
 * Confirmed on-device (Galaxy S23 FE, Android 16, 2026-08-03): without any
 * of this, back from note took 4 presses to exit (note → list → folders → a
 * redundant list → exit) instead of the intended 3 (note → list → folders →
 * exit) on the FIRST folders visit — and, before this depth-aware fix, a
 * second folders visit in the same session reintroduced the identical bug
 * one level deeper. Those counts are for a note reached FROM Folders. Since
 * the 2026-10-20 two-entry cold start (`initPhoneNavHistory`), a note opened
 * straight from the cold-start list takes the same 3 (note → list → folders
 * → background) — before it, that path took 2, because `list` was the root
 * and Back from it backgrounded the app. Through a Tags or Smart folders
 * pane it is one more (note → list → tags → folders → background).
 */
export function navigateToPane(pane: Pane, title?: string): void {
  if (!isPhoneNavActive() || phoneStackStale) {
    // No phone stack to push onto — the tablet layout, or a phone whose
    // stack waits to be rebuilt: remember the pane for that rebuild.
    rememberPane(pane, title);
    return;
  }
  if (title !== undefined) titleMemo[pane] = title;
  if (get(activePane) === pane) {
    if (title !== undefined) renameActivePane(pane, title);
    return;
  }
  if (historyBusy()) {
    // A history jump we issued is still in flight (a modal's popOverlay()
    // back, or a Folders unwind). A push now would land UNDER it and be
    // popped by it, and a go(-depth) would read a stale depth and overshoot.
    // Queue it and replay once the jump lands; the pane shows now
    // (local-first: navigation never waits). Its title is recorded when it
    // runs, at the depth it actually lands on.
    pendingOps.push({ kind: 'pane', pane, title });
    activePane.set(pane);
    return;
  }
  doNavigate(pane, title);
}

/**
 * A title for the pane already showing. While a jump is in flight the
 * active pane may be a queued op that has not pushed yet — the title goes
 * on that op; otherwise on the visible pane's own entry (beneath any
 * modals).
 */
function renameActivePane(pane: Pane, title: string): void {
  titleMemo[pane] = title;
  if (deferredPaneNav !== null && deferredPaneNav.pane === pane) {
    // The pane showing is one waiting for the modals above it to close: its
    // entry does not exist yet, and the entry under the modals is another
    // pane's — name the waiting push instead.
    deferredPaneNav.title = title;
    return;
  }
  if (historyBusy()) {
    for (let i = pendingOps.length - 1; i >= 0; i--) {
      const op = pendingOps[i];
      if ((op.kind === 'pane' || op.kind === 'rename') && op.pane === pane) {
        op.title = title;
        return;
      }
    }
    // The pane is showing but its entry is under a jump still in flight
    // (e.g. a modal's back): rename it once the jump lands. A rename op, not
    // a pane op — if the jump lands elsewhere (a Folders unwind), nothing
    // must be pushed; the title is simply dropped.
    pendingOps.push({ kind: 'rename', pane, title });
    return;
  }
  setPaneTitle(paneDepthAtOrBelow(depthFromHistoryState(history.state)), title);
}

function doNavigate(pane: Pane, title?: string): void {
  if (!isPhoneNavActive() || phoneStackStale) {
    // A queued op replayed after a flip to the tablet (or onto a stack that
    // waits for its rebuild): record it rather than drop it. It showed its
    // pane when it was queued, so activePane already says so — but history
    // never got its entry, so the phone stack is not intact.
    tabletSpellChanged = true;
    rememberPane(pane, title);
    return;
  }
  const state = history.state;
  const currentDepth = depthFromHistoryState(state);
  if (pane !== 'folders' && topIsLiveOverlay()) {
    // An open modal owns the top entry. A push now would land ABOVE it, and
    // the modal's own close (one history.back()) or the user's next Back
    // would pop the pane entry instead of the modal's — leaving a dead
    // overlay entry and a Back that closes nothing (final review 2026-10-20:
    // phone Settings › Accounts › Remove fires App's folder watcher under
    // three Settings pages). Show the pane now (local-first) and push it
    // once the modal entries above the pane stack are gone. The latest such
    // navigation wins. 'folders' is not deferred: it pushes nothing — its
    // unwind replaces the stack, overlays included (see navigateToPane).
    deferredPaneNav = { pane, title };
    activePane.set(pane);
    return;
  }
  // Any navigation that runs supersedes one still waiting.
  deferredPaneNav = null;
  if (pane === 'folders') {
    const rootTitle = title ?? DEFAULT_TITLES.folders;
    if (currentDepth === 0) {
      history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
      activePane.set('folders');
      recordEntry(0, rootTitle);
    } else {
      foldersResetInFlight = true;
      foldersResetTitle = rootTitle;
      history.go(-currentDepth);
      // activePane, the root-entry relabel and its title happen in
      // handlePopState once the jump lands at depth 0 — see the branch there.
    }
  } else if (paneFromHistoryState(state) === pane && overlayFromHistoryState(state) === null) {
    // A replayed op whose jump landed on that pane already: no dead entry.
    activePane.set(pane);
    if (title !== undefined) setPaneTitle(currentDepth, title);
  } else {
    history.pushState({ pane, depth: currentDepth + 1 }, '', location.href);
    activePane.set(pane);
    recordEntry(currentDepth + 1, title ?? DEFAULT_TITLES[pane]);
  }
}

/**
 * Android Back that the WebView itself refused — MainActivity calls this
 * (as `window.__joddHardwareBack`) before it backgrounds Jodd, and
 * backgrounds only when it returns false.
 *
 * The WebView refuses at the root, which is right, but also over entries
 * Chromium's history-manipulation intervention marked skippable: entries a
 * page pushes with no user activation since its last back/forward are
 * skipped by the BROWSER's back (canGoBack()/goBack()), while a script's
 * history.back() is not affected. Every stack this module builds without a
 * tap — a rebuild on a layout flip that happens while Jodd is in the
 * background, a cold-start seed, a stack built under What's New — is such a
 * stack, and the live pass of 2026-10-21 saw its first Back background the
 * app with "‹ _test_" on screen. Above the root, one script back is exactly
 * what the Back should have done; its popstate is handled as usual.
 */
export function hardwareBackFallback(): boolean {
  if (depthFromHistoryState(history.state) === 0) return false;
  history.back();
  return true;
}

/** Wire to `window.addEventListener('popstate', handlePopState)` in App.svelte. */
export function handlePopState(e: PopStateEvent): void {
  // Our own popOverlay() back: the modal already closed itself. Checked
  // before the layout gate because the back is issued regardless of layout
  // (a modal opened on the phone may close after a rotation to tablet).
  const landed = depthFromHistoryState(e.state);
  // Every entry above the landing is gone, and so are its titles. Done
  // before any queued op replays, so a replayed push records at the right
  // depth.
  truncateTitlesTo(landed);

  if (ignoredPops > 0) {
    ignoredPops--;
    // A queued pane op already showed its pane; replaying it below sets the
    // final value, so do not flash the landing pane in between.
    const paneQueued = pendingOps.some((op) => op.kind === 'pane') || deferredPaneNav !== null;
    // While the stack waits for a rebuild, history does not say which pane
    // the user is on; activePane does.
    if (isPhoneNavActive() && !paneQueued && !phoneStackStale) activePane.set(paneFromHistoryState(e.state));
    refreshBackLabel();
    drainPendingOps();
    return;
  }

  // Not one of our own backs: the user moved, so a waiting popPane is stale.
  forgetDeferredPanePop();

  // Every live overlay entry deeper than where we landed was just popped.
  const popped = [...liveOverlays]
    .filter(([, depth]) => depth > landed)
    .sort((a, b) => b[1] - a[1])
    .map(([id]) => id);
  for (const id of popped) liveOverlays.delete(id);

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
    // left to guard, so it must reset here too. A re-init's seed is dropped
    // with it: off the phone there is no stack to seed — but the pane the
    // user asked for is remembered, for the stack a rotation back rebuilds.
    if (foldersResetInFlight && landed === 0) {
      if (seedListTitle !== null) rememberPane('list', seedListTitle);
      else rememberPane('folders', foldersResetTitle);
    }
    foldersResetInFlight = false;
    seedListTitle = null;
    // Off the phone there is no pane stack to push onto.
    deferredPaneNav = null;
    refreshBackLabel();
    drainPendingOps();
    // Every Android modal owns an entry in any layout (and keeps it across a
    // rotation), so Back must still close it here.
    void routeOverlayPops(popped);
    return;
  }
  if (foldersResetInFlight) {
    // A programmatic unwind to the root. Overlay entries it passed through
    // are forgotten, not routed to their closers: the caller navigated on
    // purpose and owns closing whatever it navigated away from.
    if (landed === 0) {
      foldersResetInFlight = false;
      if (seedListTitle !== null) {
        // A re-run initPhoneNavHistory: seed the cold-start list.
        const title = seedListTitle;
        seedListTitle = null;
        seedRoot(title);
      } else {
        history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
        activePane.set('folders');
        recordEntry(0, foldersResetTitle);
      }
      drainPendingOps();
      return;
    }
    // go(-currentDepth) is specced to land in one jump, so this branch
    // (still mid-unwind) is not expected in practice; fall through and
    // reflect whatever pane actually landed rather than getting stuck.
    foldersResetInFlight = false;
    seedListTitle = null;
    activePane.set(paneFromHistoryState(e.state));
    refreshBackLabel();
    drainPendingOps();
    return;
  }
  // A pane waiting for the modals above the stack keeps showing: the user's
  // Back closed one of those modals, not the pane. So does one waiting for
  // its stack to be rebuilt (reconcileLayout).
  if (!phoneStackStale) activePane.set(deferredPaneNav?.pane ?? paneFromHistoryState(e.state));
  refreshBackLabel();
  runDeferredPaneNav();
  reconcileLayout();
  void routeOverlayPops(popped);
}

/* ------------------------------------------------------------------------ *
 * Overlay entries — every open Modal on Android owns one history entry, in
 * any layout (controller ruling R7; spec §5.4: tablet Back closes an open
 * modal first). Desktop pushes nothing.
 *
 * Shape: `{ pane: <pane beneath>, depth: <depth beneath> + 1, overlay: id }`.
 * - They COUNT toward depth, so navigateToPane('folders')'s go(-depth)
 *   unwinds through them to the true root.
 * - `pane` repeats the pane beneath, so paneFromHistoryState() of an overlay
 *   entry is still the pane the user sees under the modal, and activePane
 *   never changes on push or pop.
 * - Android Back (popstate) that removes one routes to the closer the modal
 *   registered; a closer that returns false (the dirty guard kept the modal
 *   open) gets its entry pushed back.
 * - A modal closing from its own UI calls popOverlay(): one history.back(),
 *   whose popstate is then ignored. history.back() is asynchronous, so while
 *   one of ours is in flight (or a Folders go(-depth) unwind) any further
 *   push, back or navigateToPane is queued and replayed after it lands —
 *   otherwise a push could land UNDER the in-flight back and be popped by
 *   it, two backs in one turn would race, and go(-depth) would overshoot.
 * The root-Back path (MainActivity.kt's moveTaskToBack, gotcha #31) is
 * untouched: an overlay entry is always above the root.
 * ------------------------------------------------------------------------ */

/** Overlay id → depth of its live history entry. */
const liveOverlays = new Map<string, number>();
const overlayClosers = new Map<string, () => Promise<boolean>>();
/** Popstates still to arrive from backs we issued ourselves. */
let ignoredPops = 0;
type PendingOp =
  | { kind: 'push'; id: string }
  | { kind: 'back' }
  | { kind: 'pane'; pane: Pane; title?: string }
  | { kind: 'rename'; pane: Pane; title: string };
const pendingOps: PendingOp[] = [];

/** `history.state.overlay` when it is an overlay entry, else null. */
export function overlayFromHistoryState(state: unknown): string | null {
  if (state && typeof state === 'object' && 'overlay' in state) {
    const o = (state as { overlay: unknown }).overlay;
    if (typeof o === 'string') return o;
  }
  return null;
}

function paneBeneath(): Pane {
  const state = history.state;
  if (state && typeof state === 'object' && 'pane' in state) return paneFromHistoryState(state);
  return get(activePane);
}

function doPush(id: string): void {
  const depth = depthFromHistoryState(history.state) + 1;
  history.pushState({ pane: paneBeneath(), depth, overlay: id }, '', location.href);
  liveOverlays.set(id, depth);
  recordEntry(depth, null);
}

function doBack(): void {
  ignoredPops++;
  history.back();
}

/** True while a history jump we issued has not landed yet. */
function historyBusy(): boolean {
  return ignoredPops > 0 || foldersResetInFlight;
}

/** The current entry belongs to an open modal (one with a live entry). */
function topIsLiveOverlay(): boolean {
  const id = overlayFromHistoryState(history.state);
  return id !== null && liveOverlays.has(id);
}

/**
 * A pane navigation that arrived while a modal owned the top entry — see
 * doNavigate. One at most (the latest wins); it runs once the current entry
 * is no longer a live overlay and no jump is in flight.
 */
let deferredPaneNav: { pane: Pane; title?: string } | null = null;

function runDeferredPaneNav(): void {
  const wanted = deferredPaneNav;
  if (wanted === null || historyBusy() || topIsLiveOverlay()) return;
  deferredPaneNav = null;
  doNavigate(wanted.pane, wanted.title);
}

function drainPendingOps(): void {
  while (!historyBusy() && pendingOps.length > 0) {
    const op = pendingOps.shift()!;
    if (op.kind === 'push') doPush(op.id);
    else if (op.kind === 'back') doBack();
    else if (op.kind === 'rename') doRename(op.pane, op.title);
    else doNavigate(op.pane, op.title);
  }
  if (!historyBusy()) runDeferredPanePop();
  runDeferredPaneNav();
  reconcileLayout();
}

/** A replayed rename: applies only if the jump landed on that pane. */
function doRename(pane: Pane, title: string): void {
  if (!isPhoneNavActive() || phoneStackStale) {
    titleMemo[pane] = title;
    return;
  }
  const depth = paneDepthAtOrBelow(depthFromHistoryState(history.state));
  if (paneFromHistoryState(history.state) === pane) setPaneTitle(depth, title);
}

function cancelPendingPush(id: string): boolean {
  const i = pendingOps.findIndex((op) => op.kind === 'push' && op.id === id);
  if (i < 0) return false;
  pendingOps.splice(i, 1);
  return true;
}

/**
 * Pushes an overlay entry for `id` above the current entry. Not gated on the
 * layout: the Modal decides at open time (Android only), and the re-push
 * after a refused Back must work after a rotation too.
 */
export function pushOverlay(id: string): void {
  if (historyBusy()) pendingOps.push({ kind: 'push', id });
  else doPush(id);
}

/**
 * Leaves `pane` the way Back would — one history.back(), whose popstate is
 * ignored and shows the entry beneath — when the phone stack is showing it.
 * For a pane whose subject went away under the user (the open note was
 * deleted, spec §5.3: "pop to list"). A no-op on any
 * other pane, and at the root (gotcha #31: the root is never left from here).
 * Off the phone layout it only records the pane beneath as the one a
 * rotation back to phone shows.
 */
export function popPane(pane: Pane): void {
  if (!isPhoneNavActive() || phoneStackStale) {
    // No stack to back out of: the pane beneath it is where a rebuilt stack
    // must land.
    if (pane !== 'folders' && get(activePane) === pane) rememberPane(PARENT_PANE[pane]);
    return;
  }
  if (get(activePane) !== pane) return;
  // Behind a jump in flight, or under an open modal (whose own close will
  // pop its entry first): wait for that, so this back removes the pane's
  // entry and not the modal's.
  // The entry to leave is the pane's own — beneath any modal entries.
  deferredPanePop = { pane, depth: paneDepthAtOrBelow(depthFromHistoryState(history.state)) };
  if (!historyBusy()) runDeferredPanePop();
}

/**
 * A popPane waiting for a jump in flight or a modal above it; one at most.
 * It names the pane's entry by depth and runs only on that same entry; a
 * history move of the user's own (a plain Back) or a modal turned into a
 * destination forgets it — so a later, unrelated modal close can never back
 * out of a note the user opened afterwards.
 */
let deferredPanePop: { pane: Pane; depth: number } | null = null;

function forgetDeferredPanePop(): void {
  deferredPanePop = null;
}

function runDeferredPanePop(): void {
  const wanted = deferredPanePop;
  if (wanted === null || overlayFromHistoryState(history.state) !== null) return;
  deferredPanePop = null;
  if (!isPhoneNavActive() || get(activePane) !== wanted.pane) return;
  const depth = depthFromHistoryState(history.state);
  if (depth === 0 || depth !== wanted.depth || paneFromHistoryState(history.state) !== wanted.pane) return;
  doBack();
}

/**
 * The modal closed from its own UI: remove its entry with exactly one
 * history.back(), whose popstate is ignored. A no-op when `id` has no live
 * entry (never pushed, already popped by Back, or turned into a destination
 * by closeOverlayInto), so a Modal may call it on every close path.
 */
export function popOverlay(id: string): void {
  if (cancelPendingPush(id)) return;
  if (!liveOverlays.has(id)) return;
  liveOverlays.delete(id);
  if (historyBusy()) pendingOps.push({ kind: 'back' });
  else doBack();
}

/**
 * A modal action that navigates closes the modal INTO its destination:
 * the overlay entry becomes `{ pane, depth: same }` via replaceState. Never
 * history.back() then push — back() is asynchronous and the two would race.
 * Back from the destination then lands on the pane beneath the modal.
 *
 * Two cases need no new entry: a destination equal to the pane beneath (one
 * back, or Back would be a dead press), and 'folders', which must stay the
 * sole root entry (navigateToPane's unwind). Off the phone stack (desktop
 * has no entry; tablet keeps one) there is no pane to land on, so the
 * entry is simply removed with one back — never left behind as a dead press
 * — and the destination is recorded for a rotation back to phone.
 *
 * `title` names the destination, as for navigateToPane; the overlay's depth
 * becomes a pane slot with that title.
 */
export function closeOverlayInto(id: string, pane: Pane, title?: string): void {
  // The user's own pick in the modal supersedes a navigation still waiting
  // for it to close.
  deferredPaneNav = null;
  if (cancelPendingPush(id)) {
    navigateToPane(pane, title);
    return;
  }
  if (!isPhoneNavActive() || phoneStackStale) {
    // No stack to turn the entry into: remove it, and remember the pane for
    // the stack a rotation to phone (or the close itself) rebuilds.
    rememberPane(pane, title);
    popOverlay(id);
    return;
  }
  const state = history.state;
  if (!liveOverlays.has(id) || overlayFromHistoryState(state) !== id) {
    navigateToPane(pane, title);
    return;
  }
  if (title !== undefined) titleMemo[pane] = title;
  if (pane === paneFromHistoryState(state)) {
    // The entry beneath the modal IS the destination: one back, and the
    // title (if any) renames that entry.
    if (title !== undefined) {
      setPaneTitle(paneDepthAtOrBelow(depthFromHistoryState(state) - 1), title);
    }
    popOverlay(id);
    activePane.set(pane);
    return;
  }
  if (pane === 'folders') {
    liveOverlays.delete(id);
    navigateToPane('folders', title);
    return;
  }
  liveOverlays.delete(id);
  // The modal became a destination: the stack moved on, and a waiting
  // popPane must not back out of it.
  forgetDeferredPanePop();
  const depth = depthFromHistoryState(state);
  history.replaceState({ pane, depth }, '', location.href);
  activePane.set(pane);
  recordEntry(depth, title ?? DEFAULT_TITLES[pane]);
}

/**
 * The function a popped overlay entry routes to — Modal registers its
 * `requestClose`. Resolves true if the modal closed. Returns the unregister.
 */
export function registerOverlayCloser(id: string, fn: () => Promise<boolean>): () => void {
  overlayClosers.set(id, fn);
  return () => {
    if (overlayClosers.get(id) === fn) overlayClosers.delete(id);
  };
}

/**
 * Assumes at most ONE refusal per popstate. A single Back pops exactly one
 * entry, so `ids` normally holds one overlay; if a larger jump popped
 * several and an inner one refuses, only that one is re-pushed — the
 * outer ones' entries stay gone (their modals stay open, without Back).
 */
async function routeOverlayPops(ids: string[]): Promise<void> {
  for (const id of ids) {
    const closer = overlayClosers.get(id);
    if (!closer) continue;
    let closed: boolean;
    try {
      closed = await closer();
    } catch (err) {
      console.error('overlay closer failed', err);
      closed = false;
    }
    // Still mounted and still open: give it its entry back.
    if (!closed && overlayClosers.get(id) === closer) {
      pushOverlay(id);
      return;
    }
  }
}

/* ------------------------------------------------------------------------ *
 * Rotation — the phone stack across a phone ⇄ tablet layout change.
 *
 * Live pass 2026-10-21 (Galaxy S23 FE, landscape ≥ 700 CSS px = tablet):
 * Folders → a folder → a note, rotate, Back: two presses popped the note and
 * list entries with no visible effect, the third backgrounded the app; back
 * in portrait the note showed with no Back and Back backgrounded at once —
 * history sat at the root while activePane still said 'note'.
 *
 * The model:
 * - The tablet layout keeps NO pane entries (as on a tablet cold start): a
 *   flip to it unwinds them to the root, so tablet Back closes an open modal
 *   or the drawer (they own entries, R7) and otherwise backgrounds the app.
 *   A live modal's entry is never unwound from under it — the unwind waits
 *   until no open modal owns an entry in history.
 * - While on the tablet, navigateToPane / popPane / closeOverlayInto still
 *   record the pane the user is on (activePane) and its title; nothing is
 *   pushed.
 * - A flip back to phone rebuilds folders → [tags | smart] → list → note for
 *   that pane, titled from what was recorded — again only once no modal owns
 *   the top entry; until then the pane shows (local-first) under the modal.
 * The root stays the bottom entry throughout, so root Back is still
 * MainActivity's moveTaskToBack (gotcha #31).
 * ------------------------------------------------------------------------ */

/** initPhoneNavHistory has run on Android: the pane stack is tracked. */
let navArmed = false;
/**
 * History does not hold the phone stack for activePane — the layout is (or
 * was) the tablet's. A phone layout rebuilds it (reconcileLayout).
 */
let phoneStackStale = false;
/**
 * Since the last flip to the tablet, history or the pane changed — an unwind
 * ran, or a pane was recorded. If not (a modal stayed open through a quick
 * rotation and back), the phone stack beneath it is intact and is kept.
 */
let tabletSpellChanged = false;
/** The latest title each pane was given, for a rebuild. */
const titleMemo: Partial<Record<Pane, string>> = {};
/** Where a pane sits in the stack: the pane whose entry is beneath it. */
const PARENT_PANE: Record<Exclude<Pane, 'folders'>, Pane> = {
  tags: 'folders',
  smart: 'folders',
  list: 'folders',
  note: 'list',
};

let layoutWatch: (() => void) | null = null;
let layoutWasPhone: boolean | null = null;

/** Starts following androidLayoutMode, once (from the first init). */
function armLayoutWatch(): void {
  if (layoutWatch) return;
  layoutWasPhone = isPhoneNavActive();
  layoutWatch = androidLayoutMode.subscribe(onLayoutChange);
}

function onLayoutChange(): void {
  const phone = isPhoneNavActive();
  if (phone === layoutWasPhone) return;
  layoutWasPhone = phone;
  if (!navArmed) return;
  if (phone) {
    if (!tabletSpellChanged) phoneStackStale = false;
  } else {
    // A stack already stale (a tablet cold start, an earlier spell not yet
    // rebuilt) stays known-changed: only an intact phone stack starts a
    // spell that may end with it kept.
    if (!phoneStackStale) tabletSpellChanged = false;
    phoneStackStale = true;
    // Both belonged to the phone stack, and neither has run: what they
    // wanted is recorded instead, so the rebuild lands there — the pane a
    // deferred push wanted (already activePane), the parent of a pane a
    // deferred pop was to leave.
    const nav = deferredPaneNav;
    deferredPaneNav = null;
    if (nav) {
      tabletSpellChanged = true;
      if (nav.title !== undefined) titleMemo[nav.pane] = nav.title;
    }
    const pop = deferredPanePop;
    forgetDeferredPanePop();
    if (pop && pop.pane !== 'folders' && get(activePane) === pop.pane) rememberPane(PARENT_PANE[pop.pane]);
  }
  reconcileLayout();
}

/** Off the phone, or while the stack waits for a rebuild: record the pane. */
function rememberPane(pane: Pane, title?: string): void {
  if (!navArmed || !get(isAndroid)) return;
  if (pane !== get(activePane)) tabletSpellChanged = true;
  activePane.set(pane);
  if (title !== undefined) titleMemo[pane] = title;
  reconcileLayout();
}

/**
 * Brings history in line with the layout: no pane entries on the tablet, the
 * full stack for activePane on the phone. Waits while one of our jumps is in
 * flight or an open modal owns an entry; every popstate landing and every
 * drain calls it again.
 */
function reconcileLayout(): void {
  if (!navArmed || !get(isAndroid) || historyBusy() || liveOverlayInHistory()) return;
  const depth = depthFromHistoryState(history.state);
  if (!isPhoneNavActive()) {
    if (depth > 0) unwindToRoot(depth);
    return;
  }
  if (!phoneStackStale) return;
  // Leftover entries (an unwind that could not run yet): to the root first;
  // the landing calls back here.
  if (depth > 0) {
    unwindToRoot(depth);
    return;
  }
  phoneStackStale = false;
  rebuildStack(get(activePane));
}

/**
 * An open modal still owns an entry in history — on top, or beneath a pane
 * entry another modal closed INTO (closeOverlayInto). Unwinding through it
 * would leave the modal open with no entry, and its own close would then
 * back() at the root, where nothing pops: the ignored popstate never comes
 * and every later push and navigation queues forever.
 */
function liveOverlayInHistory(): boolean {
  const depth = depthFromHistoryState(history.state);
  for (const d of liveOverlays.values()) if (d > 0 && d <= depth) return true;
  return false;
}

/** One jump to the root, its popstate ignored (it changes no pane). */
function unwindToRoot(depth: number): void {
  tabletSpellChanged = true;
  ignoredPops++;
  history.go(-depth);
}

/** The panes from the root up to `pane`. */
function stackFor(pane: Pane): Pane[] {
  const panes: Pane[] = [pane];
  let p = pane;
  while (p !== 'folders') {
    p = PARENT_PANE[p];
    panes.unshift(p);
  }
  return panes;
}

/** Relabels the root entry and pushes the stack for `pane` above it. */
function rebuildStack(pane: Pane): void {
  const panes = stackFor(pane);
  history.replaceState({ pane: 'folders', depth: 0 }, '', location.href);
  paneTitles.length = 0;
  paneTitles.push(titleMemo.folders ?? DEFAULT_TITLES.folders);
  for (let d = 1; d < panes.length; d++) {
    history.pushState({ pane: panes[d], depth: d }, '', location.href);
    paneTitles.push(titleMemo[panes[d]] ?? DEFAULT_TITLES[panes[d]]);
  }
  activePane.set(pane);
  refreshBackLabel();
}
