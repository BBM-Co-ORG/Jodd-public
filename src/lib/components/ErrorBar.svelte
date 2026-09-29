<script lang="ts">
  import { error } from '../stores/notes';

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
</script>

{#if $error}
  <div class="error-bar" role="alert">
    <span class="error-text">{$error}</span>
    <button type="button" class="error-dismiss" aria-label="Dismiss error" onclick={dismiss}>×</button>
  </div>
{/if}

<style>
  /* Pinned to the viewport rather than placed in the flow: `.app-layout` is
     `height: 100vh; overflow: hidden`, so a sibling in normal flow would be
     pushed off-screen — which is the placement bug this component exists to
     fix, in a new costume. z-index sits above the panes (which top out
     around 100) and below the modals (1000+), so a dialog is never covered
     by an error the user cannot reach past it. */
  .error-bar {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    z-index: 900;
    display: flex;
    align-items: center;
    gap: 12px;
    background: var(--danger-wash);
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

  .error-text {
    flex: 1;
    min-width: 0;
  }

  .error-dismiss {
    flex: none;
    background: none;
    border: none;
    color: inherit;
    cursor: pointer;
    font-size: 1.1rem;
    padding: 0 4px;
  }

  .error-dismiss:hover {
    opacity: 0.7;
  }
</style>
