<script lang="ts">
  import { error } from '../stores/notes';
  import Icon from './Icon.svelte';
  import { isCalm, DETAIL_PREFIX } from '../errorText';

  // Rendered by App.svelte at the app level, NOT inside a pane.
  //
  // It used to live in NoteEditor.svelte, which App.svelte replaces with
  // TrashPreview whenever '__TRASH__' is selected — so an error raised while
  // the user sat in Recently Deleted had nowhere to render, and surfaced only
  // once they opened a note in some other folder, reading as if THAT folder
  // had failed. `error` is a global store; its surface has to outlive the
  // pane that happened to be mounted when the failure occurred.
  //
  // Dismissal is manual on purpose. This store also carries account state the
  // user must act on — `handleAuthLoss`'s "Signed out — Keychain credentials
  // were removed. Please sign in again." — so a timeout, or a clear on
  // navigation, would quietly retire a message that is still true.
  function dismiss() {
    error.set(null);
  }

  // Lines are separate sentences (one per account for a listing). A details
  // line (errorText.ts) is the backend's own text: kept for a bug report,
  // shown smaller than what the user needs to read.
  const linesOf = (text: string) => text.split('\n').filter((l) => l.trim() !== '');
</script>

{#if $error}
  <!-- Calm: offline or rate-limited, so nothing failed that the user must act
       on. Neutral colours, and announced politely instead of as an alert. -->
  {@const calm = isCalm($error)}
  <div class="error-bar" class:calm role={calm ? 'status' : 'alert'}>
    <div class="error-text">
      {#each linesOf($error) as line, i (i)}
        {#if line.startsWith(DETAIL_PREFIX)}
          <p class="error-detail" title={line.slice(DETAIL_PREFIX.length)}>{line.slice(DETAIL_PREFIX.length)}</p>
        {:else}
          <p>{line}</p>
        {/if}
      {/each}
    </div>
    <button type="button" class="error-dismiss" aria-label={calm ? 'Dismiss' : 'Dismiss error'} title="Dismiss" onclick={dismiss}>
      <Icon name="close" size={14} />
    </button>
  </div>
{/if}

<style>
  /* Pinned to the viewport rather than placed in the flow: `.app-layout` is
     `height: 100dvh; overflow: hidden`, so a sibling in normal flow would be
     pushed off-screen — which is the placement bug this component exists to
     fix, in a new costume. --z-banner sits above the panes and drawers and
     below menus, so a menu opened near the bottom is never covered; dialogs
     are on the browser's top layer (showModal), above every z-index. */
  .error-bar {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    z-index: var(--z-banner);
    display: flex;
    align-items: center;
    gap: 12px;
    /* The wash over an opaque surface. The bar floats over the panes, and
       the wash alone let note text show through it: 2.18:1 where the two
       crossed (workspace review 2026-11-09). */
    background: linear-gradient(var(--danger-wash), var(--danger-wash)), var(--surface-panel);
    color: var(--danger);
    padding: 8px 20px;
    /* Clear Android's nav bar / tablet taskbar: bottom: 0 on a fixed element
       is the physical screen edge, so on the Tab S7 the whole bar — remedy
       text included — sat behind the taskbar (device pass, 2026-09-29).
       Same inset Sidebar's .footer-row uses; env() is 0 on desktop, so the
       bar there is unchanged. */
    padding-bottom: calc(8px + env(safe-area-inset-bottom));
    font-size: var(--size-sm);
    border-top: 1px solid var(--danger);
  }

  /* Offline or rate-limited: not a failure, so not the alarm colour. */
  .error-bar.calm {
    background: var(--surface-panel);
    color: var(--text);
    border-top-color: var(--border);
  }

  .error-text {
    flex: 1;
    min-width: 0;
  }

  .error-text p {
    margin: 0;
    line-height: var(--leading);
  }

  /* The backend's own words: a URL or a stack of them can be one long
     token, so it may break anywhere, and three lines is enough to quote in
     a report (the full text is in the title). */
  .error-text .error-detail {
    font-size: var(--size-xs);
    color: var(--text-secondary);
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    overflow: hidden;
  }

  .error-dismiss {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--hit-target-desktop);
    min-height: var(--hit-target-desktop);
    margin: calc(-1 * var(--space-1)) calc(-1 * var(--space-2)) calc(-1 * var(--space-1)) 0;
    padding: 0;
    background: none;
    border: none;
    border-radius: var(--radius);
    color: inherit;
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }

  .error-dismiss:hover {
    background: var(--danger-wash);
  }

  .calm .error-dismiss:hover {
    background: var(--surface-hover);
  }

  /* A finger needs the full touch target. Asked of the pointer, not the
     platform or the width: a touch laptop answers coarse too. */
  @media (pointer: coarse) {
    .error-dismiss {
      min-width: var(--touch-target);
      min-height: var(--touch-target);
    }
  }
</style>
