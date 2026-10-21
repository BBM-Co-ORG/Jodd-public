<script lang="ts">
  // The #-autocomplete list (spec 2026-10-20 §6.4): the account's existing
  // tags for the `#prefix` being typed in the body. It only presents — what
  // to list, where, and which row is active are NoteEditor's (the `[[`
  // picker's keys and caret placement, ../editor/hashtagTrigger.ts and
  // ../editor/anchorPosition.ts) — and it renders nothing for an empty list,
  // so a new tag is simply the text the user typed.
  //
  // A tap or click must not take focus from the body, or the insert runs
  // against nothing and a phone keyboard drops: pointerdown and mousedown are
  // cancelled (FormatBar's pattern), and the pick runs on click.

  let {
    items,
    activeIndex,
    position,
    onPick,
    touch = false,
    id = 'hashtag-picker',
  }: {
    items: string[];
    activeIndex: number;
    /** Viewport coordinates. Below the caret: `top`. On a phone the list may
     *  sit above the caret instead, anchored by its `bottom` edge, with a
     *  `maxHeight` that keeps it clear of the format bar and the keyboard
     *  (caretPanelPosition). */
    position: { left: number; top?: number; bottom?: number; maxHeight?: number };
    onPick: (tag: string) => void;
    /** Phone/tablet: 48px rows. */
    touch?: boolean;
    /** The listbox id; row i is `${id}-${i}`. The editor body points
     *  aria-activedescendant at the active row, since focus stays there. */
    id?: string;
  } = $props();

  const style = $derived(
    [
      `left:${position.left}px`,
      position.top !== undefined ? `top:${position.top}px` : '',
      position.bottom !== undefined ? `bottom:${position.bottom}px` : '',
      position.maxHeight !== undefined ? `max-height:${position.maxHeight}px` : '',
    ]
      .filter(Boolean)
      .join('; '),
  );

  function keepFocus(e: Event): void {
    e.preventDefault();
  }
</script>

{#if items.length}
  <div class="hashtag-picker" class:touch role="listbox" aria-label="Tags" data-testid="hashtag-picker" {id} {style}>
    {#each items as tag, i (tag)}
      <button
        class="opt"
        class:active={i === activeIndex}
        type="button"
        role="option"
        id={`${id}-${i}`}
        aria-selected={i === activeIndex}
        tabindex="-1"
        data-tag={tag}
        onpointerdown={keepFocus}
        onmousedown={keepFocus}
        onclick={() => onPick(tag)}
      ><span class="hash">#</span><span class="name">{tag}</span></button>
    {/each}
  </div>
{/if}

<style>
  .hashtag-picker {
    position: fixed;
    z-index: var(--z-popover);
    min-width: 160px;
    max-width: min(340px, calc(100vw - 2 * var(--space-2)));
    max-height: 240px;
    overflow-y: auto;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-menu);
    padding: var(--space-1);
  }
  .opt {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: var(--hit-target-desktop);
    border: none;
    background: none;
    text-align: left;
    padding: var(--space-1-5) var(--space-2);
    border-radius: var(--radius);
    cursor: pointer;
    font-family: inherit;
    font-size: var(--size);
    color: var(--text);
    line-height: var(--leading);
  }
  .name {
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .touch .opt {
    min-height: var(--touch-target);
  }
  .opt:hover,
  .opt.active {
    background: var(--accent-tint);
  }
  .hash {
    color: var(--text-muted);
  }
</style>
