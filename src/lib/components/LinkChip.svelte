<script lang="ts">
  // The chip under a clicked link: what it points at, and what can be done with
  // it. It only presents buttons; every action is `chipActions`' (links/
  // handlers.ts) run against the editor's `LinkContext`, and the editor decides
  // where it sits (`style`) and when it goes (`onclose`).
  //
  // Nothing here may take focus from the note: the selection in the editor IS
  // the caret, and a button that focuses itself drops it (and the soft keyboard
  // on Android). Same rule, same two handlers as FormatBar.svelte: a tap's
  // compatibility mousedown is where WebKit and Chromium move focus, and a mouse
  // on a Chromebook-class device sends no cancelled pointerdown first, so BOTH
  // are prevented. No `disabled` anywhere — it would stop the pointerdown.
  import { chipActions } from '../links/handlers';
  import type { LinkAction, LinkContext } from '../links/handlers';
  import type { LinkTarget } from '../links/target';

  let {
    target,
    preview,
    style,
    ctx,
    canEdit = true,
    onclose,
  }: {
    target: LinkTarget;
    /** A note link's resolved note, `'missing'` when none matches, null for
     *  every other kind (or while it has not resolved). */
    preview: { title: string; firstLine: string } | 'missing' | null;
    /** Position, from the editor. */
    style: string;
    ctx: LinkContext;
    /** False on a note the user cannot edit (read-only account, or a note the
     *  remote locked): Edit link and Remove link would change it. */
    canEdit?: boolean;
    onclose: () => void;
  } = $props();

  const EDITS = new Set(['edit', 'remove']);
  const actions = $derived<LinkAction[]>(
    preview === 'missing'
      ? chipActions(target).filter((a) => a.id === 'copy')
      : chipActions(target).filter((a) => canEdit || !EDITS.has(a.id)),
  );

  function keepFocus(e: Event): void {
    e.preventDefault();
  }

  function activate(action: LinkAction): void {
    // `finally`: a throwing action (a clipboard refusal, a caller's editLink)
    // must still close the chip, or a second click re-runs the failure. The
    // error is not swallowed.
    try {
      action.run(ctx);
    } finally {
      onclose();
    }
  }
</script>

<div class="link-chip" role="toolbar" aria-label="Link" {style}>
  <div class="target">
    {#if preview === 'missing'}
      <span class="missing">No note called “{target.key ?? target.label}”</span>
    {:else if preview}
      <span class="title">{preview.title}</span>
      {#if preview.firstLine}<span class="first-line">{preview.firstLine}</span>{/if}
    {:else}
      <span class="uri">{target.uri}</span>
    {/if}
  </div>
  <div class="actions">
    {#each actions as action (action.id)}
      <button
        type="button"
        data-action={action.id}
        onpointerdown={keepFocus}
        onmousedown={keepFocus}
        onclick={() => activate(action)}
      >{action.label}</button>
    {/each}
  </div>
</div>

<style>
  .link-chip {
    position: absolute;
    z-index: var(--z-sticky);
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    max-width: min(360px, 90vw);
    /* The placement sets a max-height; on a phone a long URI can exceed it. */
    overflow-y: auto;
    padding: var(--space-2);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow-menu);
    color: var(--text);
    font-family: var(--font-sans);
    font-size: var(--size);
    line-height: var(--leading-tight);
  }

  .target {
    display: flex;
    flex-direction: column;
    gap: var(--space-0-5);
    min-width: 0;
  }
  .title {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .first-line {
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* The full URI, never truncated: it is what the user is deciding on. */
  .uri {
    color: var(--text-secondary);
    overflow-wrap: anywhere;
    user-select: text;
  }
  .missing {
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .actions button {
    min-height: var(--hit-target-desktop);
    padding: 0 var(--space-3);
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--accent-action);
    font: inherit;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }
  .actions button:hover {
    background: var(--surface-hover);
  }
  .actions button:active {
    background: var(--surface-active);
  }
</style>
