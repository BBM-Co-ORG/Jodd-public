<script lang="ts">
  // The live strip under a view note's property block (spec §8.5). Quiet, like
  // the editor's other meta lines; all logic lives in views/preview.ts.
  import { onDestroy } from 'svelte';
  import { createViewPreview, isViewsLabel, type PreviewState } from '../views/preview';
  import { viewDataRevision } from '../views/viewsStore';

  interface Props {
    /** Identity of the open note: a change re-runs the preview once. */
    noteKey: string;
    accountId: string;
    label: string | null | undefined;
    title: string;
    /** The editor's current body HTML, read when the debounce fires. */
    getBody: () => string;
    /** Bumped by the editor on every input and every body from outside. */
    editRev: number;
  }
  let { noteKey, accountId, label, title, getBody, editRev }: Props = $props();

  let state = $state<PreviewState>({ kind: 'idle' });
  let shownFor = '';
  const preview =createViewPreview((s) => { state = s; });

  $effect(() => {
    // Re-run on a note switch, an edit, a body from outside, or the rows any
    // view reads changing (`viewDataRevision`: the count covers the whole
    // account, gotcha #6). The request carries no stale state.
    void editRev;
    void $viewDataRevision;
    // A different note starts with no strip; an edit keeps the last answer
    // until the new one arrives (no flicker while typing).
    if (noteKey !== shownFor) { shownFor = noteKey; state = { kind: 'idle' }; }
    preview.schedule({ accountId, label, title, body: getBody });
    return () => preview.cancel();
  });

  onDestroy(() => preview.cancel());
</script>

{#if isViewsLabel(label) && state.kind !== 'idle'}
  <div class="view-preview" data-testid="view-preview" class:has-error={state.kind === 'error'} role="status" aria-live="polite">
    {#if state.kind === 'ok'}
      <span class="vp-summary">{state.summary}</span>
      {#each state.notices as n}<span class="vp-notice">{n}</span>{/each}
    {:else}
      {#each state.errors as e}<span class="vp-error">{e}</span>{/each}
    {/if}
  </div>
{/if}

<style>
  .view-preview {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0 0 var(--space-2, 8px);
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--text-muted);
  }
  .view-preview.has-error .vp-error {
    color: var(--danger);
  }
  .vp-notice {
    color: var(--text-muted);
  }
</style>
