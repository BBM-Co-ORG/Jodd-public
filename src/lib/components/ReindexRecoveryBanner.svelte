<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';

  let visible = false;
  let reindexing = false;

  onMount(async () => {
    try {
      visible = await invoke<boolean>('needs_reindex_after_recovery');
    } catch (e) {
      console.error('needs_reindex_after_recovery failed', e);
    }
  });

  async function reindexAll() {
    reindexing = true;
    try {
      const accounts = await invoke<Array<{ id: string; backend_kind: string }>>('list_accounts');
      for (const acct of accounts) {
        if (acct.backend_kind === 'gmail') {
          await invoke('index_account', { accountId: acct.id });
        }
      }
      await invoke('clear_reindex_marker');
      visible = false;
    } catch (e) {
      console.error('reindex after recovery failed', e);
      alert(`Re-index failed: ${e}`);
    } finally {
      reindexing = false;
    }
  }

  async function dismiss() {
    try {
      await invoke('clear_reindex_marker');
    } catch (e) {
      console.error('clear_reindex_marker failed', e);
    }
    visible = false;
  }
</script>

{#if visible}
  <div class="reindex-banner" role="alert">
    <span>
      Jodd's local note cache was rebuilt after a security key mismatch.
      Re-index your Gmail accounts to restore your notes.
    </span>
    <button on:click={reindexAll} disabled={reindexing}>
      {reindexing ? 'Re-indexing…' : 'Re-index now'}
    </button>
    <button on:click={dismiss} disabled={reindexing}>Dismiss</button>
  </div>
{/if}

<style>
  .reindex-banner {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.5rem 1rem;
    background: var(--color-warning-bg, #fff3cd);
    color: var(--color-warning-text, #664d03);
    border-bottom: 1px solid var(--color-warning-border, #ffe69c);
    font-size: 0.9rem;
  }
</style>
