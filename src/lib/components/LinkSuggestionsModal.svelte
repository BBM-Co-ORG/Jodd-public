<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import Icon from './Icon.svelte';

  export let accountId: string;
  export let proposedAppends: { uuid: string; title: string; addition_text: string }[];
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
        .map((p) => ({ uuid: p.uuid, addition_text: p.addition_text }));
      const applied = await invoke<string[]>('apply_wiki_link_appends', {
        accountId,
        appends,
      });
      onApplied?.(applied);
      onClose();
    } catch (e) {
      error = `Failed to apply: ${e}`;
    } finally {
      applying = false;
    }
  }
</script>

<div class="backdrop" role="dialog" aria-modal="true" aria-labelledby="link-suggestions-title">
  <div class="modal">
    <header>
      <h2 id="link-suggestions-title">Related notes found</h2>
      <button class="close-x" onclick={onClose} title="Close" aria-label="Close"><Icon name="close" size={14} /></button>
    </header>

    <div class="body">
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
    </div>

    <footer>
      <button type="button" class="btn" onclick={onClose} disabled={applying}>Skip all</button>
      <button
        type="button"
        class="btn primary"
        onclick={confirmApply}
        disabled={applying || selectedCount === 0}
      >
        {#if applying}
          Applying…
        {:else if selectedCount === 0}
          Nothing selected
        {:else}
          Add to {selectedCount} note{selectedCount === 1 ? '' : 's'}
        {/if}
      </button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .modal {
    width: min(560px, 92vw);
    max-height: 88vh;
    background: var(--surface-editor);
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    display: flex;
    flex-direction: column;
    font-family: inherit;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 14px 18px 8px;
    border-bottom: 1px solid var(--border-subtle);
  }
  h2 { margin: 0; font-size: var(--size-md); font-weight: 600; color: var(--text); }
  .close-x { display: inline-flex; align-items: center; justify-content: center; background: none; border: none; font-size: var(--size-md); color: var(--text-muted); cursor: pointer; padding: 2px 6px; }
  .close-x:hover { color: var(--text); }

  .body { flex: 1; overflow-y: auto; padding: 12px 18px; }
  .error { color: var(--danger); font-size: var(--size); }
  .summary { font-size: var(--size-sm); color: var(--text-secondary); margin: 0 0 14px; }

  .suggestion {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 10px;
    border: 1px solid var(--border-subtle);
    border-radius: 8px;
    margin-bottom: 8px;
    cursor: pointer;
  }
  .suggestion:hover { background: var(--surface-sunken); }
  .suggestion input[type="checkbox"] { margin-top: 3px; }
  .suggestion-meta { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .suggestion-title { font-size: var(--size); font-weight: 600; color: var(--text); line-height: var(--leading); }
  .suggestion-addition { font-size: var(--size-sm); color: var(--text-secondary); font-style: italic; line-height: var(--leading); }

  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    padding: 10px 18px 14px;
    border-top: 1px solid var(--border-subtle);
  }
  .btn {
    font-family: inherit;
    font-size: var(--size-sm);
    padding: 6px 14px;
    border: 1px solid var(--border-subtle);
    border-radius: 5px;
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { background: var(--surface-sidebar); }
  .btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn.primary { background: var(--accent-action); color: var(--text-inverse); border-color: var(--accent-action); }
  .btn.primary:hover:not(:disabled) { background: var(--accent-hover); }
</style>
