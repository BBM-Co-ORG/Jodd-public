<script lang="ts">
  // The note's tags (spec 2026-10-20 §6.2), opened from the metadata line's
  // #N or ⋯ › Tags…. It replaces the row of chips that sat under the title.
  //
  // It edits nothing itself: `onAdd` / `onRemove` are NoteEditor's own paths
  // (`addNormalizedTag` → `appendTagToBody`, `removeTag` → delete the `#`),
  // which run as edit commands on the live editor, so Cmd+Z takes them back.
  // While this popover's field has focus, they borrow the editor's focus and
  // give it back (bodyEdits `takeFocus`) — the same as the old row's field.
  //
  // Phone: Popover is a Modal sheet there, a modal <dialog>, and the page
  // behind a modal dialog is inert — the editor cannot take focus and an edit
  // command run against it is refused (measured in Chrome: execCommand
  // returns false and nothing changes). So the sheet keeps its adds and
  // removes, shows them at once, and hands them on in order once it has
  // closed and the editor is live again.
  //
  // A sheet edit that can no longer land is dropped, never applied: the
  // caller closing it (the note changed) or tags turning read-only while it
  // is open clears what was pending. A rotation out of the sheet hands the
  // pending edits on at once, so later ones cannot overtake them — and when
  // the rotation destroys this component with them (App mounts a separate
  // editor per layout), `onabandon` hands them to the caller to pass on.
  //
  // Read-only (`editable` false — iCloud, gotcha #38, where a tag is an
  // object Jodd cannot create; or a note that takes no edits): the tags are
  // listed, with no field and no ×, and `readOnlyHint` says where they are
  // edited (none for a note that is not saved yet).
  import { onDestroy, tick, untrack } from 'svelte';
  import type { TagEdit } from '../editor/tagEditHandoff';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { normalizeTag } from '../editor/bodyTags';
  import Popover from './Popover.svelte';

  interface Props {
    anchor: HTMLElement;
    open: boolean;
    onclose: () => void;
    /** The note's tags, normalized, in body order. */
    tags: string[];
    editable: boolean;
    /** The account's other tags matching what is typed (normalized or not). */
    suggestions: (q: string) => string[];
    /** Gets a normalized tag that is not on the note. */
    onAdd: (tag: string) => void;
    onRemove: (tag: string) => void;
    /** Read-only: the one line saying where tags are edited; null, none. */
    readOnlyHint?: string | null;
    /** Why tags are read-only, behind Learn more; none, no disclosure. */
    readOnlyReason?: string | null;
    /**
     * The sheet is being destroyed with edits still queued (a rotation swaps
     * the editor it belongs to): they are handed over, in order. Unset, they
     * are dropped.
     */
    onabandon?: (ops: TagEdit[]) => void;
  }

  let {
    anchor,
    open,
    onclose,
    tags,
    editable,
    suggestions,
    onAdd,
    onRemove,
    readOnlyHint = 'Add or remove tags in Apple Notes.',
    readOnlyReason = null,
    onabandon,
  }: Props = $props();

  const uid = $props.id();
  const listId = `${uid}-tags-suggest`;

  // The same gate Popover uses for its sheet.
  const asSheet = $derived($isAndroid && $androidLayoutMode === 'phone');

  type Op = TagEdit;
  // Edits made in the phone sheet, not yet applied.
  let pending = $state<Op[]>([]);
  const shown = $derived.by(() => {
    let out = [...tags];
    for (const p of pending) {
      if (p.op === 'add') {
        if (!out.includes(p.tag)) out.push(p.tag);
      } else {
        out = out.filter((t) => t !== p.tag);
      }
    }
    return out;
  });

  let query = $state('');
  let focused = $state(false);
  let highlight = $state(-1); // -1: Enter adds what was typed
  let inputEl: HTMLInputElement | undefined = $state();
  const options = $derived(editable ? suggestions(query).filter((t) => !shown.includes(t)) : []);
  const listShown = $derived(focused && options.length > 0);
  // Whatever changes the list — typing, an add, the account's tags — the
  // highlight starts over, so it never points at a stale row.
  $effect(() => {
    void options;
    untrack(() => (highlight = -1));
  });

  function apply(ops: Op[]): void {
    for (const o of ops) (o.op === 'add' ? onAdd : onRemove)(o.tag);
  }

  function add(tag: string): void {
    if (!tag || shown.includes(tag)) return;
    if (!asSheet) {
      onAdd(tag);
      return;
    }
    // Re-adding a tag removed in this sheet only cancels the remove.
    const removed = pending.findIndex((p) => p.op === 'remove' && p.tag === tag);
    pending = removed >= 0 ? pending.filter((_, i) => i !== removed) : [...pending, { op: 'add', tag }];
  }

  function remove(tag: string): void {
    if (!asSheet) {
      // Keep focus in the popover, not on the row about to go: the editor
      // gives focus back to whatever had it.
      inputEl?.focus();
      onRemove(tag);
      return;
    }
    const added = pending.findIndex((p) => p.op === 'add' && p.tag === tag);
    pending = added >= 0 ? pending.filter((_, i) => i !== added) : [...pending, { op: 'remove', tag }];
  }

  function commit(): void {
    const tag = normalizeTag(query);
    query = '';
    highlight = -1;
    add(tag);
  }

  function pick(tag: string): void {
    query = '';
    highlight = -1;
    add(tag);
  }

  function onKeydown(e: KeyboardEvent): void {
    // An IME composing (Thai on Gboard, Japanese, …) owns Enter and the
    // arrows: they commit or pick a candidate, not a tag.
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (options.length) highlight = (highlight + 1) % options.length;
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (options.length) highlight = (highlight - 1 + options.length) % options.length;
    } else if (e.key === 'Enter') {
      e.preventDefault();
      // A highlighted suggestion wins; otherwise what was typed.
      if (highlight >= 0 && highlight < options.length) pick(options[highlight]);
      else commit();
    }
    // Escape is Popover's (or the sheet's): it closes.
  }

  // Closed by the user (Esc, outside, the sheet's close or Back). The sheet's
  // edits run once it has gone — after the next render, when the editor
  // behind it is no longer inert.
  function close(): void {
    const ops = pending;
    pending = [];
    query = '';
    highlight = -1;
    onclose();
    if (!ops.length) return;
    void tick().then(() => apply(ops));
  }

  // Closed by the caller (the note changed): nothing carries over.
  $effect(() => {
    if (open) return;
    untrack(() => {
      pending = [];
      query = '';
      highlight = -1;
    });
  });

  // Tags turned read-only while the sheet was open (the note was locked,
  // the account's capabilities arrived): what was pending cannot land.
  $effect(() => {
    if (editable) return;
    untrack(() => (pending = []));
  });

  // Rotated out of the sheet with edits pending: the editor is live again
  // once the sheet has unmounted, and from here on edits apply at once — so
  // the pending ones go first.
  $effect(() => {
    if (asSheet) return;
    untrack(() => {
      if (!pending.length) return;
      const ops = pending;
      pending = [];
      void tick().then(() => apply(ops));
    });
  });

  // Destroyed with edits still queued — no close path ran (Back, ✕, drag
  // and the scrim all apply them): the editor this sheet belongs to is going
  // too, so they are handed over rather than lost.
  onDestroy(() => {
    if (!pending.length) return;
    const ops = pending;
    pending = [];
    onabandon?.(ops);
  });

  // Desktop: the field takes the caret on open, as clicking the old row's
  // did. Not on Android, phone or tablet, where focus raises the keyboard.
  $effect(() => {
    if (!open || !editable || $isAndroid || !inputEl) return;
    const el = inputEl;
    untrack(() => el.focus());
  });
</script>

<Popover {anchor} {open} onclose={close} label="Tags" placement="bottom-end">
  <!-- A dialog on desktop and tablet; on phone Popover is already a Modal
       sheet, so the role would nest. -->
  <div
    class="tags-pop"
    class:touch={$isAndroid}
    data-testid="tags-popover"
    role={asSheet ? undefined : 'dialog'}
    aria-label={asSheet ? undefined : 'Tags'}
  >
    {#if shown.length}
      <ul class="tags-list">
        {#each shown as tag (tag)}
          <li class="tags-row">
            <span class="tags-name">#{tag}</span>
            {#if editable}
              <button
                type="button"
                class="tags-remove"
                title="Remove tag"
                aria-label="Remove tag {tag}"
                onclick={() => remove(tag)}
              >×</button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}

    {#if editable}
      {#snippet suggestList()}
        <ul id={listId} class="tags-suggest" class:in-sheet={asSheet} role="listbox" aria-label="Existing tags">
          {#each options as s, i (s)}
            <!-- mousedown, not click: the field keeps focus, so the list
                 is still there when the pick lands. -->
            <li
              id="{listId}-{i}"
              class="tags-option"
              class:highlight={i === highlight}
              role="option"
              aria-selected={i === highlight}
              onmousedown={(e) => {
                e.preventDefault();
                pick(s);
              }}
            >#{s}</li>
          {/each}
        </ul>
      {/snippet}
      <div class="tags-field">
        <!-- In the phone sheet the list sits in the sheet, ABOVE the field:
             below it, it hung past the sheet's bottom edge, under the
             keyboard or the navigation bar (live pass 2026-10-21). The
             sheet's padding already clears both. -->
        {#if listShown && asSheet}{@render suggestList()}{/if}
        <input
          bind:this={inputEl}
          bind:value={query}
          class="tag-input"
          type="text"
          placeholder="Add a tag"
          aria-label="Add a tag"
          autocomplete="off"
          autocapitalize="off"
          spellcheck="false"
          role="combobox"
          aria-expanded={listShown}
          aria-controls={listShown ? listId : undefined}
          aria-activedescendant={listShown && highlight >= 0 ? `${listId}-${highlight}` : undefined}
          aria-autocomplete="list"
          onkeydown={onKeydown}
          onfocus={() => (focused = true)}
          onblur={() => (focused = false)}
        />
        {#if listShown && !asSheet}{@render suggestList()}{/if}
      </div>
    {:else}
      {#if readOnlyHint}<p class="tags-hint">{readOnlyHint}</p>{/if}
      {#if readOnlyReason}
        <details class="tags-more">
          <summary>Learn more</summary>
          <p>{readOnlyReason}</p>
        </details>
      {/if}
    {/if}
  </div>
</Popover>

<style>
  .tags-pop {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    width: min(280px, calc(100vw - 2 * var(--space-4)));
    padding: var(--space-2);
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text);
  }
  /* The sheet is full width already. */
  :global([data-presentation='sheet']) .tags-pop {
    width: auto;
  }
  .tags-list {
    display: flex;
    flex-direction: column;
    gap: var(--space-0-5);
    margin: 0;
    padding: 0;
    list-style: none;
    max-height: calc(var(--touch-target) * 5);
    overflow-y: auto;
  }
  .tags-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--hit-target-desktop);
    padding: 0 var(--space-1) 0 var(--space-2);
    border-radius: var(--radius-sm);
  }
  .tags-row:hover {
    background: var(--surface-hover);
  }
  .tags-name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-secondary);
  }
  .tags-remove {
    flex: none;
    min-width: var(--hit-target-desktop);
    min-height: var(--hit-target-desktop);
    padding: 0;
    border: none;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--size-md);
    line-height: var(--leading-tight);
    cursor: pointer;
  }
  .tags-remove:hover {
    color: var(--danger);
  }
  .tags-field {
    position: relative;
  }
  .tag-input {
    box-sizing: border-box;
    width: 100%;
    min-height: var(--hit-target-desktop);
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-editor);
    color: var(--text);
    font: inherit;
  }
  .tag-input::placeholder {
    color: var(--text-muted);
  }
  .tags-suggest {
    position: absolute;
    top: 100%;
    left: 0;
    right: 0;
    z-index: 1;
    margin: var(--space-0-5) 0 0;
    padding: var(--space-1);
    list-style: none;
    max-height: calc(var(--hit-target-desktop) * 8);
    overflow-y: auto;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow-card);
  }
  /* In the phone sheet: in the flow, above the field (see the markup). */
  .tags-suggest.in-sheet {
    position: static;
    margin: 0 0 var(--space-0-5);
    max-height: calc(var(--touch-target) * 4);
    box-shadow: none;
  }
  .tags-option {
    padding: var(--space-1) var(--space-2);
    color: var(--text-secondary);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .tags-option:hover,
  .tags-option.highlight {
    background: var(--accent-tint);
    color: var(--accent-on-tint);
  }
  .tags-hint {
    margin: 0;
    color: var(--text-secondary);
  }
  .tags-more {
    color: var(--text-muted);
  }
  .tags-more summary {
    cursor: pointer;
  }
  .tags-more p {
    margin: var(--space-1) 0 0;
  }
  /* Android: finger-sized rows, × and field. */
  .tags-pop.touch .tags-row,
  .tags-pop.touch .tag-input,
  .tags-pop.touch .tags-option {
    min-height: var(--touch-target);
  }
  .tags-pop.touch .tags-remove {
    min-width: var(--touch-target);
    min-height: var(--touch-target);
  }
  .tags-pop.touch .tags-option {
    display: flex;
    align-items: center;
  }
</style>
