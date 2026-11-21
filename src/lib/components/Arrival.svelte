<script lang="ts">
  // The arrival line (stores/arrival.ts), rendered once by App.svelte outside
  // every pane, like Toast and ErrorBar.
  //
  // The live region stays mounted and holds only the words, so a screen
  // reader announces the sentence and not the Dismiss button beside it.
  import { arrival, dismissArrival } from '../stores/arrival';
  import Icon from './Icon.svelte';
</script>

<div class="arrival" class:open={!!$arrival} data-testid="arrival">
  <div class="arrival-text" role="status" aria-live="polite">
    {#if $arrival}
      {#each $arrival.lines as line, i (i)}
        <p class:lead={i === 0}>{line}</p>
      {/each}
    {/if}
  </div>
  {#if $arrival}
    <button class="arrival-close" aria-label="Dismiss" onclick={dismissArrival}>
      <Icon name="close" size={14} />
    </button>
  {/if}
</div>

<style>
  /* Closed, it is an empty live region with no box. */
  .arrival {
    position: fixed;
    left: 50%;
    bottom: calc(var(--space-8) + env(safe-area-inset-bottom));
    transform: translateX(-50%);
    z-index: var(--z-toast);
    box-sizing: border-box;
    width: max-content;
    max-width: min(400px, calc(100vw - 2 * var(--space-4)));
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
  }

  /* Open: a card that floats, so it has the menu shadow (The
     Floating-Only Shadow Rule). It arrives lit, in the amber of the lamp,
     and settles to plain paper: warmth is colour, so it stays under reduced
     motion; the short rise is movement, so --dur-move takes it to 0. */
  .arrival.open {
    padding: var(--space-3) var(--space-2) var(--space-3) var(--space-4);
    background: var(--surface-panel);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
    animation:
      arrival-rise var(--dur-move) var(--ease-out) both,
      arrival-settle calc(var(--dur-focal) * 3) var(--ease-out) both;
  }

  .arrival-text p {
    margin: 0;
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text-secondary);
  }
  .arrival-text p.lead {
    font-size: var(--size);
    font-weight: var(--weight-semibold);
    color: var(--text);
  }

  .arrival-close {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--hit-target-desktop);
    min-height: var(--hit-target-desktop);
    margin: calc(var(--space-1) * -1) 0;
    padding: 0;
    border: none;
    border-radius: var(--radius);
    background: transparent;
    color: var(--text-muted);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .arrival-close:hover {
    background: var(--surface-hover);
  }
  @media (pointer: coarse) {
    .arrival-close {
      min-width: var(--touch-target);
      min-height: var(--touch-target);
      margin: calc(var(--space-3) * -1) 0;
    }
  }

  /* translateX(-50%) is the card's position; the rise rides on top of it. */
  @keyframes arrival-rise {
    from {
      transform: translateX(-50%) translateY(var(--space-2));
    }
    to {
      transform: translateX(-50%);
    }
  }

  @keyframes arrival-settle {
    from {
      background: var(--accent-tint);
      border-color: var(--accent-tint);
    }
    to {
      background: var(--surface-panel);
      border-color: var(--border);
    }
  }
</style>
