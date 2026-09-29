<script lang="ts">
  // The Android tablet frame: a top bar holding the ☰ Folders toggle, the
  // folders drawer, and the panes (note list + editor) below the bar.
  //
  // The bar is IN FLOW on purpose. It used to be a `position: absolute`
  // button over the note list; it cleared the status bar itself but took no
  // space, so the list's search box sat on the status bar's row with the
  // button over its left edge (Galaxy Tab S7, 2026-09-28). It spans the full
  // width because the editor toolbar has no status-bar clearance of its own
  // either.
  import type { Snippet } from 'svelte';
  import Sidebar from './Sidebar.svelte';

  let { children }: { children: Snippet } = $props();

  // Independent of desktop's sidebarCollapsed — that one defaults to
  // expanded and is user-toggled per session; the tablet drawer starts
  // closed so the note list gets the width.
  let drawerOpen = $state(false);
</script>

<div class="app-layout tablet-layout">
  {#if drawerOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="tablet-drawer-backdrop"
      role="button"
      tabindex="0"
      aria-label="Close folders"
      onclick={() => (drawerOpen = false)}
      onkeydown={(e) => { if (e.key === 'Escape') drawerOpen = false; }}
    ></div>
    <div class="tablet-drawer">
      <Sidebar width={280} on:collapse={() => (drawerOpen = false)} />
    </div>
  {/if}
  <div class="tablet-pane-header">
    <button
      class="tablet-nav-btn"
      onclick={() => (drawerOpen = !drawerOpen)}
      aria-label="Folders"
    >☰ Folders</button>
  </div>
  <div class="tablet-panes">
    {@render children()}
  </div>
</div>

<style>
  .app-layout {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .tablet-pane-header {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    /* Same status-bar clearance as App.svelte's .phone-pane-header and
       Sidebar's .sidebar-header. env(safe-area-inset-top) falls back to 0 in
       WebViews that don't populate it, so max(8px, ...) always keeps at
       least 8px. */
    padding: max(8px, env(safe-area-inset-top)) 12px 8px;
    border-bottom: 1px solid var(--border);
    background: var(--surface-sidebar);
  }

  .tablet-nav-btn {
    background: none;
    border: none;
    color: var(--text);
    font-size: var(--size-md);
    padding: 4px 8px;
    cursor: pointer;
  }

  .tablet-panes {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }

  /* NoteList, TrashList, NoteEditor and TrashPreview each set
     `height: 100vh` for the desktop row, where they are the full window
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
    z-index: 10;
    border: none;
  }

  .tablet-drawer {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    z-index: 11;
    box-shadow: var(--shadow-menu);
  }
</style>
