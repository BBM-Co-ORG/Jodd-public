<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import Modal from './Modal.svelte';
  import { refreshUnreviewed } from '../unreviewed';

  export let accountId: string;
  export let proposedAppends: { ai_result_id?: string; uuid: string; title: string; addition_text: string }[];
  export let onClose: () => void;
  export let onApplied: ((appliedUuids: string[]) => void) | undefined = undefined;

  let applying = false;
  let error = '';
  // Default: all proposed appends selected, matching DupReviewModal's
  // "default-select everything" convention.
  let selected: Record<string, boolean> = Object.fromEntries(
    proposedAppends.map((p) => [p.uuid, true]),
  );

  $: selectedCount = Object.values(selected).filter(Boolean).length;

  async function confirmApply() {
    if (selectedCount === 0) return;
    applying = true;
    error = '';
    try {
      const appends = proposedAppends
        .filter((p) => selected[p.uuid])
        .map((p) => ({ ai_result_id: p.ai_result_id ?? '', uuid: p.uuid, addition_text: p.addition_text }));
      const applied = await invoke<string[]>('apply_wiki_link_appends', {
        accountId,
        appends,
      });
      void refreshUnreviewed([accountId]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
      onApplied?.(applied);
      onClose();
    } catch (e) {
      error = `Failed to apply: ${e}`;
    } finally {
      applying = false;
    }
  }
</script>

<Modal
  title="Related notes found"
  size="md"
  onclose={onClose}
  primary={{
    label: applying
      ? 'Applying…'
      : selectedCount === 0
        ? 'Nothing selected'
        : `Add to ${selectedCount} note${selectedCount === 1 ? '' : 's'}`,
    onclick: confirmApply,
    disabled: selectedCount === 0,
    busy: applying,
  }}
>
  {#if error}
    <p class="error">{error}</p>
  {/if}
  <p class="summary">
    This note looks related to <strong>{proposedAppends.length}</strong>
    existing note{proposedAppends.length === 1 ? '' : 's'}. Review what would be
    added to each before confirming — nothing is written until you confirm.
  </p>

  {#each proposedAppends as p (p.uuid)}
    <label class="suggestion">
      <input type="checkbox" bind:checked={selected[p.uuid]} />
      <div class="suggestion-meta">
        <span class="suggestion-title">{p.title || 'Untitled'}</span>
        <span class="suggestion-addition">+ "{p.addition_text}"</span>
      </div>
    </label>
  {/each}

  {#snippet footer()}
    <button type="button" class="btn" onclick={onClose} disabled={applying}>Skip all</button>
  {/snippet}
</Modal>

<style>
  .error { color: var(--danger); font-size: var(--size); }
  .summary { font-size: var(--size-sm); color: var(--text-secondary); margin: 0 0 var(--space-3); }

  .suggestion {
    display: flex;
    align-items: flex-start;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    margin-bottom: var(--space-2);
    cursor: pointer;
  }
  .suggestion:hover { background: var(--surface-sunken); }
  .suggestion input[type="checkbox"] { margin-top: var(--space-0-5); }
  .suggestion-meta { display: flex; flex-direction: column; gap: var(--space-0-5); min-width: 0; }
  .suggestion-title { font-size: var(--size); font-weight: 600; color: var(--text); line-height: var(--leading); }
  .suggestion-addition { font-size: var(--size-sm); color: var(--text-secondary); font-style: italic; line-height: var(--leading); }

  .btn {
    font-family: inherit;
    font-size: var(--size-sm);
    padding: var(--space-1) var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { background: var(--surface-sidebar); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
