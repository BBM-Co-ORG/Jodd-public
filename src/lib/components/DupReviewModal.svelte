<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import type { OrphanGroup } from '../types';
  import Modal from './Modal.svelte';

  export let accountId: string;
  export let onClose: () => void;
  // Called after a successful trash so the parent can refresh the dup pill.
  export let onTrashed: ((count: number) => void) | undefined = undefined;

  let groups: OrphanGroup[] = [];
  let loading = true;
  let trashing = false;
  let loadError = '';
  // Per-orphan checkbox state. Default: all orphans selected for trash.
  // Keyed by orphan id, value = true means "include in trash batch".
  let selected: Record<string, boolean> = {};

  onMount(async () => {
    await loadPreview();
  });

  async function loadPreview() {
    loading = true;
    loadError = '';
    try {
      groups = await invoke<OrphanGroup[]>('preview_orphans', { accountId });
      // Default-select every orphan id.
      const sel: Record<string, boolean> = {};
      for (const g of groups) for (const o of g.orphans) sel[o.id] = true;
      selected = sel;
    } catch (e) {
      loadError = String(e);
    } finally {
      loading = false;
    }
  }

  $: totalOrphans = groups.reduce((acc, g) => acc + g.orphans.length, 0);
  $: selectedCount = Object.values(selected).filter(Boolean).length;

  function toggleAllInGroup(g: OrphanGroup, value: boolean) {
    const next = { ...selected };
    for (const o of g.orphans) next[o.id] = value;
    selected = next;
  }

  async function confirmTrash() {
    if (selectedCount === 0) return;
    trashing = true;
    try {
      const ids = Object.entries(selected).filter(([, v]) => v).map(([k]) => k);
      const trashed = await invoke<number>('trash_specific_messages', {
        accountId,
        messageIds: ids,
      });
      onTrashed?.(trashed);
      onClose();
    } catch (e) {
      loadError = `Trash failed: ${e}`;
    } finally {
      trashing = false;
    }
  }

  function fmtDate(s: string): string {
    if (!s) return '';
    const d = new Date(s);
    if (isNaN(d.getTime())) return s;
    return d.toLocaleString(undefined, {
      year: 'numeric', month: 'short', day: 'numeric',
      hour: '2-digit', minute: '2-digit', hour12: false,
    });
  }

  function shortLabel(label: string): string {
    return label.replace(/^Notes\/?/, '') || '(root)';
  }
</script>

<Modal
  title="Review duplicates"
  size="lg"
  dirty={trashing}
  dirtyMessage="Trashing is in progress. Close anyway?"
  onclose={onClose}
>
  {#if loading}
    <p class="placeholder">Scanning Gmail for duplicates… this can take ~10s for large mailboxes.</p>
  {:else if loadError}
    <p class="error">{loadError}</p>
  {:else if groups.length === 0}
    <p class="placeholder">No duplicates found.</p>
  {:else}
    <p class="summary">
      <strong>{totalOrphans}</strong> duplicate message{totalOrphans === 1 ? '' : 's'}
      across <strong>{groups.length}</strong> note{groups.length === 1 ? '' : 's'}.
      Trashed messages stay in Gmail Trash for 30 days.
    </p>

    {#each groups as g (g.uuid)}
      <section class="group">
        <div class="group-header">
          <span class="group-title" title={g.keeper.title || '(untitled)'}>{g.keeper.title || '(untitled)'}</span>
          <span class="group-folder">in {shortLabel(g.keeper.label)}</span>
          <button
            type="button"
            class="link-btn"
            onclick={() => toggleAllInGroup(g, !g.orphans.every((o) => selected[o.id]))}
          >
            {g.orphans.every((o) => selected[o.id]) ? 'deselect all' : 'select all'}
          </button>
        </div>

        <div class="version keeper">
          <span class="version-tag keeper-tag">KEEP</span>
          <div class="version-meta">
            <span class="version-date">{fmtDate(g.keeper.date)}</span>
            <span class="version-preview" title={g.keeper.body_preview || '(no body)'}>{g.keeper.body_preview || '(no body)'}</span>
          </div>
        </div>

        {#each g.orphans as o (o.id)}
          <label class="version orphan">
            <input
              type="checkbox"
              bind:checked={selected[o.id]}
            />
            <span class="version-tag orphan-tag">TRASH</span>
            <div class="version-meta">
              <span class="version-date">{fmtDate(o.date)}</span>
              <span class="version-preview" title={o.body_preview || '(no body)'}>{o.body_preview || '(no body)'}</span>
            </div>
          </label>
        {/each}
      </section>
    {/each}
  {/if}
  {#snippet footer()}
    <div class="dup-actions">
      <!-- Moving notes to the trash is destructive: danger, left, never primary. -->
      <button
        type="button"
        class="btn danger"
        onclick={confirmTrash}
        disabled={loading || trashing || selectedCount === 0}
      >
        {#if trashing}
          Trashing…
        {:else if selectedCount === 0}
          Nothing selected
        {:else}
          Move {selectedCount} to Trash
        {/if}
      </button>
      <button type="button" class="btn cancel" onclick={onClose} disabled={trashing}>Cancel</button>
    </div>
  {/snippet}
</Modal>

<style>
  .placeholder { color: var(--text-muted); font-style: italic; font-size: var(--size); }
  .error { color: var(--danger); font-size: var(--size); }
  .summary { font-size: var(--size-sm); color: var(--text-secondary); margin: 0 0 14px; }

  .group {
    border: 1px solid var(--border-subtle);
    border-radius: 8px;
    margin-bottom: 10px;
    overflow: hidden;
  }
  .group-header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 8px 12px;
    background: var(--surface-list);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--size-sm);
  }
  .group-title {
    font-weight: 600;
    color: var(--text);
    line-height: var(--leading);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1 1 auto;
    min-width: 0;
  }
  .group-folder { color: var(--text-muted); font-size: var(--size-xs); line-height: var(--leading); }
  .link-btn {
    background: none;
    border: none;
    color: var(--accent-action);
    font-size: var(--size-xs);
    cursor: pointer;
    padding: 0;
    font-family: inherit;
  }
  .link-btn:hover { text-decoration: underline; }

  .version {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 8px 12px;
    border-top: 1px solid var(--border-subtle);
  }
  .version:first-of-type { border-top: none; }
  .version.orphan {
    cursor: pointer;
  }
  .version.orphan:hover {
    background: var(--surface-sunken);
  }
  .version input[type="checkbox"] {
    margin-top: 3px;
  }
  .version-tag {
    font-size: var(--size-xs);
    font-weight: 700;
    letter-spacing: 0.5px;
    padding: 2px 5px;
    border-radius: 3px;
    margin-top: 1px;
    flex-shrink: 0;
  }
  .keeper-tag {
    background: var(--success-wash);
    color: var(--success);
  }
  .orphan-tag {
    background: var(--danger-wash);
    color: var(--danger);
  }
  .version-meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1 1 auto;
    min-width: 0;
  }
  .version-date {
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  .version-preview {
    font-size: var(--size-sm);
    color: var(--text);
    line-height: var(--leading);
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .dup-actions { display: flex; align-items: center; gap: var(--space-2); width: 100%; }
  .btn.cancel { margin-left: auto; }
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
  .btn.danger {
    background: var(--danger);
    color: var(--text-inverse);
    border-color: var(--danger);
  }
  .btn.danger:hover:not(:disabled) { background: var(--danger); filter: brightness(1.1); }
</style>
