<script lang="ts">
  // A phone pane's bottom bar (spec 2026-10-20 §5.1): Folders has New folder ·
  // AI ✦ · New note, the list has New note on the right. It sits above the
  // safe-area inset, and every button in it is a 48px target. Tokens only.
  import type { Snippet } from 'svelte';

  let { label, children }: { label: string; children: Snippet } = $props();
</script>

<div class="phone-bar" role="toolbar" aria-label={label}>
  {@render children()}
</div>

<style>
  .phone-bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex: 0 0 auto;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    padding-bottom: calc(var(--space-1) + env(safe-area-inset-bottom));
    border-top: 1px solid var(--border);
    background: var(--surface-sidebar);
  }
  /* A lone button (the list's New note) sits on the right. */
  .phone-bar :global(button:only-child) {
    margin-left: auto;
  }
  .phone-bar :global(button) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: var(--touch-target);
    min-height: var(--touch-target);
    padding: 0;
    background: none;
    border: none;
    border-radius: var(--radius);
    color: var(--accent-action);
    font: inherit;
    font-size: var(--size-xl);
    line-height: var(--leading-tight);
    cursor: pointer;
  }
  .phone-bar :global(button:disabled) {
    color: var(--text-disabled);
    cursor: default;
  }
</style>
