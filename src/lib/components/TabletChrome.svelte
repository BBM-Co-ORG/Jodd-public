<script lang="ts">
  // The Android tablet frame: a top bar holding the Folders button (the menu icon) and the
  // current view's name, the folders (a column or a drawer), and the panes
  // (note list + editor) below the bar.
  //
  // The bar is IN FLOW on purpose. It used to be a `position: absolute`
  // button over the note list; it cleared the status bar itself but took no
  // space, so the list's search box sat on the status bar's row with the
  // button over its left edge (Galaxy Tab S7, 2026-09-28). It spans the
  // panes' full width because the editor toolbar has no status-bar clearance
  // of its own either.
  //
  // Folders by width (spec 2026-10-20 §5.4):
  // - 'tabletWide' (≥ 1024, landscape): a persistent in-flow column, full
  //   height at the left. The Folders button collapses it; the choice is kept in localStorage.
  //   No scrim and no history entry — it is part of the layout, not an
  //   overlay, so Back never has to close it.
  // - 'tablet' (700–1023, mostly portrait): an overlay drawer that owns a
  //   history entry while open (pushOverlay, the mechanism a Modal uses), so
  //   Android Back closes it before reaching the root's moveTaskToBack
  //   (gotcha #31). The scrim, a pick (folder, tag, Smart Folder, note) and
  //   Folders button close it through popOverlay.
  //
  // While the folders are out of sight (drawer closed, column collapsed), the
  // Folders button carries a danger dot when an active account needs the user — the same
  // rule as the account header's alert mark (accountDot.ts): signed out,
  // blocked, or changes the backend refused for good (spec 2026-11-12 §7).
  // Its name says which, so the dot is never colour alone.
  //
  // Refused changes are not on the account: they are read with
  // `count_pending_pushes`, SQLite only (gotcha #15), on the Sidebar's 5 s
  // cadence (gotcha #6: the worker refuses on its own and emits nothing). The
  // Sidebar polls the same count while it is mounted, so this polls only
  // while the folders are hidden, and drops what it read when they show.
  import type { Snippet } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, untrack } from 'svelte';
  import Sidebar from './Sidebar.svelte';
  import Icon from './Icon.svelte';
  import { androidLayoutMode } from '../stores/viewport';
  import { pushOverlay, popOverlay, registerOverlayCloser } from '../stores/phoneNav';
  import { listTitle } from '../stores/listView';
  import { get } from 'svelte/store';
  import { activeAccounts, selectedNote } from '../stores/notes';
  import { accountNameWithBackend } from '../stores/listView';
  import { isSignedOut } from '../controllers/authLoss';
  import { accountDotState, accountHeaderMark } from '../accountDot';
  import type { PendingPushes } from '../controllers/accountActions';
  import { sameNote } from '../noteIdentity';
  import type { Note } from '../types';

  let { children }: { children: Snippet } = $props();

  const DRAWER_ID = 'drawer';
  const COLUMN_OPEN_KEY = 'jodd.tablet.sidebarOpen';
  const COLUMN_WIDTH = 260;
  const DRAWER_WIDTH = 280;

  // Storage can throw (blocked site data) or come back empty; either way the
  // column defaults to open.
  function readColumnOpen(): boolean {
    try {
      return localStorage.getItem(COLUMN_OPEN_KEY) !== 'false';
    } catch {
      return true;
    }
  }
  function writeColumnOpen(open: boolean): void {
    try {
      localStorage.setItem(COLUMN_OPEN_KEY, String(open));
    } catch {
      /* not persisted; the toggle still works for this session */
    }
  }

  const wide = $derived($androidLayoutMode === 'tabletWide');
  let columnOpen = $state(readColumnOpen());
  // The portrait drawer is never persisted: it starts closed so the note
  // list gets the width.
  let drawerOpen = $state(false);

  const foldersShown = $derived(wide ? columnOpen : drawerOpen);

  // Per account: its last count, while the folders are hidden. A read that
  // fails leaves no count, and no count is no dot (accountDot: unknown shows
  // nothing). Keyed by account, so a slow answer only ever lands on its own.
  const COUNTS_POLL_MS = 5_000;
  let counts = $state<Record<string, PendingPushes>>({});
  const activeIds = $derived($activeAccounts.map((a) => a.id).join('\n'));
  $effect(() => {
    if (foldersShown || !activeIds) return;
    const ids = activeIds.split('\n');
    let live = true;
    const read = () => {
      for (const id of ids) {
        invoke<PendingPushes>('count_pending_pushes', { accountId: id })
          .then((c) => { if (live) counts = { ...counts, [id]: c }; })
          .catch((e) => console.error(`count_pending_pushes (Folders button) failed for ${id}:`, e));
      }
    };
    read();
    const handle = setInterval(read, COUNTS_POLL_MS);
    return () => {
      live = false;
      clearInterval(handle);
      // Stale once the Sidebar takes over: never shown again without a read.
      counts = {};
    };
  });

  const needUser = $derived(
    $activeAccounts.filter((a) => accountHeaderMark(accountDotState(a, counts[a.id], false)) === 'alert'),
  );
  // The Folders button's name: plain "Folders", or which account needs the user and why.
  const navLabel = $derived.by(() => {
    if (foldersShown || needUser.length === 0) return 'Folders';
    if (needUser.length > 1) return `Folders — ${needUser.length} accounts need you`;
    const a = needUser[0];
    const name = accountNameWithBackend(a, $activeAccounts);
    if (isSignedOut(a)) return `Folders — ${name} is signed out`;
    if (a.blocked_reason) return `Folders — ${name} is not syncing`;
    const n = counts[a.id]?.blocked ?? 0;
    return `Folders — ${name} has ${n} ${n === 1 ? 'change' : 'changes'} it can't send`;
  });

  function setColumnOpen(open: boolean): void {
    columnOpen = open;
    writeColumnOpen(open);
  }

  function openDrawer(): void {
    if (drawerOpen) return;
    drawerOpen = true;
    pushOverlay(DRAWER_ID);
  }

  /** Closes the drawer from Jodd's own UI: its entry goes with one back. */
  function closeDrawer(): void {
    if (!drawerOpen) return;
    drawerOpen = false;
    popOverlay(DRAWER_ID);
  }

  function toggle(): void {
    if (wide) setColumnOpen(!columnOpen);
    else if (drawerOpen) closeDrawer();
    else openDrawer();
  }

  function collapse(): void {
    if (wide) setColumnOpen(false);
    else closeDrawer();
  }

  // A pick closes the portrait drawer; the wide column stays.
  function onPick(): void {
    if (!wide) closeDrawer();
  }

  // Android Back popped the drawer's entry: phoneNav already forgot it, so
  // only the state changes here (no popOverlay — that would back once more).
  const unregisterCloser = registerOverlayCloser(DRAWER_ID, async () => {
    drawerOpen = false;
    return true;
  });

  // A rotation to landscape turns the drawer into the column: drop the
  // drawer and its entry, or Back would be a dead press and the scrim would
  // come back on the next rotation.
  $effect(() => {
    if (wide) untrack(closeDrawer);
  });

  // Opening a note — a note pick, wherever it came from (New note here, an
  // Ask citation) — closes the drawer. Keyed on identity, not on object
  // changes: a refresh of the open note, or its identity being forwarded
  // after a create (gotcha #16), is the same note and must not.
  let lastNote: Note | null = get(selectedNote);
  const unsubscribeNote = selectedNote.subscribe((n) => {
    if (n && !sameNote(n, lastNote)) onPick();
    lastNote = n;
  });

  onDestroy(() => {
    unsubscribeNote();
    unregisterCloser();
    // Unmounted open (a rotation to phone): its entry must not outlive it.
    if (drawerOpen) popOverlay(DRAWER_ID);
  });
</script>

<div class="app-layout tablet-layout" class:wide>
  {#if wide && columnOpen}
    <div class="tablet-column">
      <!-- The Folders button is this column's one toggle: the Sidebar drops its ‹ here. -->
      <Sidebar width={COLUMN_WIDTH} tabletFrame="column" on:collapse={collapse} on:pick={onPick} />
    </div>
  {/if}
  {#if !wide && drawerOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="tablet-drawer-backdrop"
      role="button"
      tabindex="0"
      aria-label="Close folders"
      onclick={closeDrawer}
      onkeydown={(e) => { if (e.key === 'Escape') closeDrawer(); }}
    ></div>
    <div class="tablet-drawer">
      <Sidebar width={DRAWER_WIDTH} tabletFrame="drawer" on:collapse={collapse} on:pick={onPick} />
    </div>
  {/if}
  <div class="tablet-pane-header">
    <button
      class="tablet-nav-btn"
      onclick={toggle}
      aria-label={navLabel}
      title={navLabel}
      aria-expanded={foldersShown}
    ><Icon name="menu" size={20} />{#if !foldersShown && needUser.length > 0}<span class="nav-dot" aria-hidden="true"></span>{/if}</button>
    <span class="tablet-title">{$listTitle}</span>
  </div>
  <div class="tablet-panes">
    {@render children()}
  </div>
</div>

<style>
  /* Two columns: the folders column (absent, so zero-width, when collapsed
     or in portrait) spans both rows; the bar and the panes share the rest. */
  .app-layout {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    height: 100dvh;
    overflow: hidden;
    /* A phone in landscape lands here (≥ 700 CSS px) with the navigation bar
       at a side: the editor toolbar's ⋯ and #N sat under it on the right
       (live pass 2026-10-21). Clear both sides for every column; the top
       is .tablet-pane-header's, the editor's bottom is its own. */
    box-sizing: border-box;
    padding-left: env(safe-area-inset-left);
    padding-right: env(safe-area-inset-right);
  }

  .tablet-column {
    grid-column: 1;
    grid-row: 1 / span 2;
    display: flex;
    min-height: 0;
  }

  .tablet-pane-header {
    grid-column: 2;
    grid-row: 1;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    /* Same status-bar clearance as App.svelte's .phone-pane-header and
       Sidebar's .sidebar-header. env(safe-area-inset-top) falls back to 0 in
       WebViews that don't populate it, so max(8px, ...) always keeps at
       least 8px. */
    padding: max(var(--space-2), env(safe-area-inset-top)) var(--space-3) var(--space-2);
    /* The wide column's Sidebar header (.sidebar-header.tablet-bar) copies
       these three, so the two headers share one row: same top, same height,
       the Folders button and "JODD" on one centre line (Galaxy Tab S7, 2026-10-21). */
    min-height: var(--touch-target);
    box-sizing: content-box;
    border-bottom: 1px solid var(--border);
    background: var(--surface-sidebar);
  }

  .tablet-nav-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    min-width: var(--touch-target);
    min-height: var(--touch-target);
    background: none;
    border: none;
    border-radius: var(--radius);
    color: var(--text);
    font-size: var(--size-md);
    padding: var(--space-1) var(--space-2);
    cursor: pointer;
  }

  /* The Settings gear's dot (App.svelte .pane-dot), at this button's size:
     danger, because an account that needs the user is a failure to sync. */
  .tablet-nav-btn {
    position: relative;
  }
  .nav-dot {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
    width: var(--space-2);
    height: var(--space-2);
    border-radius: 50%;
    background: var(--danger);
  }

  .tablet-title {
    flex: 1 1 auto;
    min-width: 0;
    font-size: var(--size-md);
    font-weight: var(--weight-semibold);
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .tablet-panes {
    grid-column: 2;
    grid-row: 2;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }

  /* NoteList, TrashList, NoteEditor and TrashPreview each set
     `height: 100dvh` for the desktop row, where they are the full window
     height. Under the bar that is a bar's height too tall, so let the row
     stretch them to the space that is actually left. Both classes, not
     one: `.tablet-panes > *` alone ties `.note-list` on specificity (each
     scoped class carries a svelte-hash class), and a tie goes to whichever
     component stylesheet happens to be emitted last. */
  .tablet-layout .tablet-panes > :global(*) {
    height: auto;
    align-self: stretch;
  }

  .tablet-drawer-backdrop {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    z-index: var(--z-drawer);
    border: none;
  }

  .tablet-drawer {
    position: fixed;
    top: 0;
    /* Fixed, so .app-layout's side inset does not reach it: a landscape
       phone with the navigation bar on the left would hide its edge. */
    left: env(safe-area-inset-left);
    bottom: 0;
    z-index: calc(var(--z-drawer) + 1);
    box-shadow: var(--shadow-menu);
  }
</style>
