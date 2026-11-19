<script lang="ts">
  // The sidebar's Tags: the fold's header, Match all/any and Clear, the
  // filter field and the cloud of pills. Sidebar.svelte keeps which tags the
  // scope shows (scopeTags, most-used first), whether the filter on screen is
  // this cloud's, the stored fold, and what a pick or a menu does. Split out
  // of Sidebar.svelte 2026-11-16. Desktop and tablet only: the phone's tags
  // are PhoneTagsPane.svelte.
  //
  // Jodd-local, parallel to folders. Picking one filters the list to notes
  // carrying it. A long cloud gets a filter field and a height of its own,
  // so opening it never pushes the accounts out of the column
  // (spec 2026-11-12 §4).
  import Icon from '../Icon.svelte';
  import { selectedTags, tagMatchMode, clearSelectedTags } from '../../stores/notes';
  import { longpress } from '../../longpress';

  interface Props {
    /** The scope's tags, in the order to show them. Empty renders nothing. */
    tags: { tag: string; count: number }[];
    /** Which scope the cloud shows (an account id, or ALL_ACCOUNTS): a
     *  change clears the filter text. */
    scopeKey: string;
    /** Whether the tag filter on screen is this cloud's. A pill reads as
     *  picked, and the filter's controls show, only then. */
    ownsFilter: boolean;
    /** The fold, stored per device by Sidebar (sidebarFolds.ts). */
    open: boolean;
    ontoggle: () => void;
    onselect: (tag: string) => void;
    /** Right-click or long-press on a pill. */
    onmenu: (e: MouseEvent, tag: string) => void;
  }

  let { tags, scopeKey, ownsFilter, open, ontoggle, onselect, onmenu }: Props = $props();

  // The filter field shows once a cloud is long enough to hunt in.
  const FILTER_FROM = 20;
  // A case-insensitive substring, so Thai tags match on any part, as search
  // does. Cleared on a scope change.
  let filter = $state('');
  let filterFor: string | undefined;
  $effect.pre(() => {
    if (scopeKey !== filterFor) {
      filterFor = scopeKey;
      filter = '';
    }
  });
  const shown = $derived(
    filter.trim() ? tags.filter((t) => t.tag.toLowerCase().includes(filter.trim().toLowerCase())) : tags,
  );
</script>

{#if tags.length > 0}
  <div class="tags-header">
    <button
      type="button"
      class="tags-toggle"
      aria-expanded={open}
      onclick={ontoggle}
    >
      <span class="tags-caret"><Icon name={open ? 'chevron-down' : 'chevron-right'} size={12} /></span>
      <span>Tags</span>
      <span class="tags-num">{tags.length}</span>
    </button>
    {#if ownsFilter && $selectedTags.size > 0}
      <span class="tag-controls">
        {#if $selectedTags.size > 1}
          <button
            type="button"
            class="tag-mode"
            title={$tagMatchMode === 'AND'
              ? 'Showing notes with every selected tag. Switch to notes with any of them.'
              : 'Showing notes with any selected tag. Switch to notes with all of them.'}
            onclick={() => tagMatchMode.update((m) => (m === 'AND' ? 'OR' : 'AND'))}
          >{$tagMatchMode === 'AND' ? 'Match all' : 'Match any'}</button>
        {/if}
        <button type="button" class="tag-clear" onclick={() => clearSelectedTags()}>Clear</button>
      </span>
    {/if}
  </div>
  {#if open}
    {#if tags.length >= FILTER_FROM}
      <div class="tag-filter">
        <input
          type="search"
          class="tag-filter-input"
          placeholder="Filter {tags.length} tags"
          aria-label="Filter tags"
          bind:value={filter}
          onkeydown={(e) => { if (e.key === 'Escape' && filter) { e.stopPropagation(); filter = ''; } }}
        />
      </div>
    {/if}
    <div class="tag-chips" class:capped={tags.length >= FILTER_FROM}>
      {#each shown as t (t.tag)}
        {@const picked = ownsFilter && $selectedTags.has(t.tag)}
        <button
          type="button"
          class="tag-pill"
          class:active={picked}
          aria-pressed={picked}
          title="#{t.tag} · {t.count}"
          onclick={() => onselect(t.tag)}
          oncontextmenu={(e) => onmenu(e, t.tag)}
          use:longpress
        >
          <span class="pill-name">{t.tag}</span>
          <span class="pill-count">{t.count}</span>
        </button>
      {:else}
        <p class="tag-none">No tag contains “{filter.trim()}”.</p>
      {/each}
    </div>
  {/if}
{/if}

<style>
  /* The Tags header row: its toggle, then the filter's controls. Styled as
     a Section Label, like Sidebar.svelte's .group-header (DESIGN.md: 700, 0.04em). */
  .tags-header {
    display: flex;
    align-items: center;
    gap: var(--space-1-5);
    margin: var(--space-2) var(--space-2) var(--space-0-5);
  }
  .tags-toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1-5);
    /* The desktop hit area (DESIGN.md: 28px). */
    min-height: var(--hit-target-desktop);
    padding: 0 var(--space-1);
    background: none;
    border: none;
    border-radius: var(--radius-sm);
    font: inherit;
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    letter-spacing: 0.04em;
    line-height: var(--leading-tight);
    text-transform: uppercase;
    color: var(--text-muted);
    cursor: pointer;
  }
  .tags-toggle:hover {
    background: var(--surface-hover);
    color: var(--text-secondary);
  }
  /* Sidebar.svelte has its own copy for the Views header. */
  .tags-caret { display: inline-flex; align-items: center; justify-content: center; }
  .tags-num {
    font-size: var(--size-xs);
    color: var(--text-muted);
    /* Opaque for the same reason as Sidebar.svelte's .folder-count — see that rule. */
    background: var(--surface-badge);
    padding: 0 5px;
    border-radius: var(--radius-sm);
    text-transform: none;
    letter-spacing: 0;
  }

  .tag-chips {
    display: flex;
    flex-wrap: wrap;
    align-content: flex-start;
    gap: var(--space-1);
    padding: var(--space-0-5) var(--space-3) var(--space-2) var(--space-4);
  }
  /* A long cloud scrolls inside about six lines of pills, so the accounts
     below stay in the column (spec 2026-11-12 §4). */
  .tag-chips.capped {
    max-height: calc(6 * (var(--size-xs) * var(--leading) + var(--space-1) * 2));
    overflow-y: auto;
    overscroll-behavior: contain;
    margin-bottom: var(--space-1);
    border-bottom: 1px solid var(--border-subtle);
  }
  .tag-none {
    margin: 0;
    font-size: var(--size-xs);
    line-height: var(--leading);
    color: var(--text-muted);
  }
  .tag-filter {
    padding: var(--space-0-5) var(--space-3) var(--space-1) var(--space-4);
  }
  /* DESIGN.md's input: an explicit themed background; focus is the app's
     shared :focus-visible ring (App.svelte). */
  .tag-filter-input {
    box-sizing: border-box;
    width: 100%;
    min-height: var(--hit-target-desktop);
    padding: var(--space-1) var(--space-2);
    font: inherit;
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .tag-filter-input::placeholder {
    color: var(--text-muted);
  }

  .tag-pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
    border: none;
    background: var(--surface-hover);
    color: var(--text-secondary);
    font-size: var(--size-xs);
    font-family: inherit;
    padding: 2px 4px 2px 8px;
    border-radius: var(--radius-sm);
    cursor: pointer;
    line-height: var(--leading);
  }
  .tag-pill:hover { background: var(--surface-active); }
  .tag-pill.active { background: var(--accent-tint); color: var(--accent-on-tint); }
  .pill-name {
    line-height: var(--leading);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 110px;
  }
  .pill-count {
    font-size: var(--size-xs);
    /* Same opaque-badge treatment as Sidebar.svelte's .folder-count, and no opacity. The old
       `opacity: 0.7` over a doubly-composited alpha background put this at
       2.77:1 on a resting pill — fading text is the most expensive way to
       de-emphasise something, because it costs contrast directly. The smaller
       font size already does that job. */
    background: var(--surface-badge);
    border-radius: var(--radius-sm);
    padding: 0 5px;
  }

  .tag-controls {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
  }

  /* The filter's two controls: plain words, neutral. Amber is for the
     selected tags themselves; red is for failures, never for Clear. */
  .tag-mode,
  .tag-clear {
    font: inherit;
    font-size: var(--size-xs);
    line-height: var(--leading-tight);
    color: var(--text-secondary);
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: var(--space-0-5) var(--space-1-5);
    cursor: pointer;
    white-space: nowrap;
    transition: background var(--dur-fast) var(--ease-out);
  }
  .tag-clear {
    border-color: transparent;
  }
  .tag-mode:hover,
  .tag-clear:hover {
    background: var(--surface-hover);
    color: var(--text);
  }

  /* Touch (critique 2026-11-10, P1): a coarse pointer gets the 48px Touch
     Target; a mouse keeps the desktop density. Sidebar.svelte carries the
     same rule for its own rows and buttons. */
  @media (pointer: coarse) {
    .tags-toggle,
    .tag-mode,
    .tag-clear {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-width: var(--touch-target);
      min-height: var(--touch-target);
    }
    /* Pills stay a cloud, not a list: 40px tall with 8px between them, so
       each still owns a 48px slot (WCAG 2.5.8's spacing allowance). */
    .tag-chips {
      gap: var(--space-2);
    }
    .tag-pill {
      min-height: calc(var(--touch-target) - var(--space-2));
      padding-left: var(--space-3);
      padding-right: var(--space-3);
    }
  }
</style>
