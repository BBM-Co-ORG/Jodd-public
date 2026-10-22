<script lang="ts">
  // The phone pane header (spec 2026-10-20 §5.1): a bar with Back on the left
  // and up to two actions on the right, then a large title and a subtitle.
  // App.svelte's phone branch renders one per pane; desktop and tablet render
  // none.
  //
  // Back is `history.back()` everywhere — phoneNav keeps history as the
  // navigation stack, so Back must be the same step Android's Back takes. Its
  // label is the caller's: the title of the entry beneath (phoneNav's
  // `backLabel`), so it names where Back actually goes.
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  let {
    title,
    subtitle,
    back = null,
    actions,
    largeTitle = true,
  }: {
    title?: string;
    subtitle?: string;
    /** The parent's name; null = no Back button. An untitled parent reads "Back". */
    back?: string | null;
    actions?: Snippet;
    largeTitle?: boolean;
  } = $props();

  const backText = $derived(back || 'Back');
</script>

<header class="pane-header">
  <div class="pane-bar">
    {#if back !== null}
      <button
        type="button"
        class="pane-back"
        aria-label={back ? `Back to ${back}` : 'Back'}
        onclick={() => history.back()}
      >
        <Icon name="chevron-left" size={20} />
        <span class="pane-back-label">{backText}</span>
      </button>
    {/if}
    {#if title && !largeTitle}
      <h1 class="pane-title in-bar">{title}</h1>
    {/if}
    <div class="pane-actions">
      {@render actions?.()}
    </div>
  </div>
  {#if title && largeTitle}
    <div class="pane-large">
      <h1 class="pane-title">{title}</h1>
      {#if subtitle}
        <p class="pane-subtitle">{subtitle}</p>
      {/if}
    </div>
  {/if}
</header>

<style>
  .pane-header {
    flex: 0 0 auto;
    background: var(--surface-sidebar);
    border-bottom: 1px solid var(--border);
  }

  /* The status bar sits over the top inset; the bar's touch row starts below
     it. env() is 0 in WebViews that do not populate it. */
  .pane-bar {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-height: calc(var(--touch-target) + env(safe-area-inset-top));
    padding-top: env(safe-area-inset-top);
    padding-left: var(--space-1);
    padding-right: var(--space-1);
  }

  .pane-back {
    display: inline-flex;
    align-items: center;
    gap: var(--space-0-5);
    min-height: var(--touch-target);
    min-width: var(--touch-target);
    max-width: 40%;
    padding: 0 var(--space-2) 0 0;
    background: none;
    border: none;
    color: var(--accent-action);
    font-size: var(--size-md);
    cursor: pointer;
  }
  .pane-back-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .pane-title {
    margin: 0;
    font-size: var(--size-xl);
    font-weight: 700;
    color: var(--text);
    line-height: var(--leading);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pane-title.in-bar {
    flex: 1 1 auto;
    min-width: 0;
    font-size: var(--size-md);
    font-weight: 600;
  }

  .pane-actions {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
  }
  /* Every action is a 48px target, whatever the caller renders. */
  .pane-actions :global(button) {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--accent-action);
    cursor: pointer;
  }

  .pane-large {
    padding: 0 var(--space-4) var(--space-2);
  }
  .pane-subtitle {
    margin: 0;
    font-size: var(--size-sm);
    color: var(--text-muted);
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
