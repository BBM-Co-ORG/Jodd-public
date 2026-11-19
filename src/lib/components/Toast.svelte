<script lang="ts">
  // Rendered once, by App.svelte, outside every pane (like ErrorBar), so a
  // toast raised from a menu that has since closed still has somewhere to
  // show. The live region stays mounted so screen readers announce each
  // message; only its content comes and goes.
  import { toast } from '../stores/toast';
</script>

<div class="toast-region" role="status" aria-live="polite">
  {#if $toast}
    {#key $toast.id}
      {#if $toast.lines}
        <div class="toast toast-lines">
          {#each $toast.lines as line, i (i)}
            <div class="toast-line">{line}</div>
          {/each}
        </div>
      {:else}
        <div class="toast">{$toast.text}</div>
      {/if}
    {/key}
  {/if}
</div>

<style>
  .toast-region {
    position: fixed;
    left: 50%;
    bottom: calc(var(--space-8) + env(safe-area-inset-bottom));
    transform: translateX(-50%);
    /* Cannot paint above an open modal <dialog>: it sits in the top layer, beyond any z-index. */
    z-index: var(--z-toast);
    max-width: calc(100vw - 2 * var(--space-4));
    pointer-events: none;
  }

  .toast {
    box-sizing: border-box;
    padding: var(--space-2) var(--space-4);
    background: var(--surface-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
    font-size: var(--size-sm);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* A list of results: every line is read in full, so it wraps. */
  .toast-lines {
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
  }

  .toast-line {
    overflow-wrap: anywhere;
  }
</style>
