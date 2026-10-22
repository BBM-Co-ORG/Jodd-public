<script lang="ts">
  // Settings › Privacy, on every platform (spec §4.3.1): the folders hidden
  // from agents — which also gate what leaves the device for an AI provider —
  // and the AI receipts. Folder toggles and retention apply at once; deleting
  // receipts is its own action. The hidden-folders block moved verbatim from
  // AgentWorkspaceSettings.svelte, which keeps only the desktop enable step.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { accounts } from '../../stores/notes';
  import { hiddenFromAi } from '../../stores/aiScope';
  import { hideableFolders, isHidden, withHidden, type AgentWorkspaceStatus } from '../../agentWorkspace';
  import AiReceipts from '../AiReceipts.svelte';

  let status = $state<AgentWorkspaceStatus | null>(null);
  let folders = $state<Record<string, string[]>>({});
  let message = $state('');
  let alive = true;

  onMount(() => {
    void (async () => {
      try {
        const s = await invoke<AgentWorkspaceStatus>('agent_workspace_status');
        if (alive) status = s;
      } catch {
        if (alive) message = 'Agent workspace settings are unavailable.';
      }
    })();
    return () => { alive = false; };
  });

  async function loadFolders(accountId: string) {
    if (folders[accountId]) return;
    try {
      const list = await invoke<string[]>('list_folders', { accountId });
      if (alive) folders = { ...folders, [accountId]: list };
    } catch {
      if (alive) folders = { ...folders, [accountId]: [] };
    }
  }

  // Local-first doctrine: the checkbox moves now; a refusal rolls it back.
  async function toggle(accountId: string, folder: string, hide: boolean) {
    if (!status) return;
    status = { ...status, hidden: withHidden(status.hidden, accountId, folder, hide) };
    hiddenFromAi.set(status.hidden);
    message = '';
    try {
      await invoke('set_folder_hidden_from_agents', { accountId, folder, hidden: hide });
    } catch (e) {
      if (alive && status) {
        // Undo only this folder: restoring a whole snapshot would also undo
        // a second toggle made while this one was in flight.
        status = { ...status, hidden: withHidden(status.hidden, accountId, folder, !hide) };
        hiddenFromAi.set(status.hidden);
        message = `Could not change ${folder}: ${e}`;
      }
    }
  }
</script>

<section class="section">
  <h4>Hidden from agents</h4>
  {#if status?.error}
    <p class="error" role="alert">
      {status.scope_path} cannot be read ({status.error}). Fix it by hand; Jodd will not overwrite it.
    </p>
  {:else if status}
    <p class="help">Agents never see notes in these folders, in any tool.</p>
    {#each $accounts.filter(a => a.status === undefined || a.status === 'active') as a (a.id)}
      <details ontoggle={(e) => { if ((e.currentTarget as HTMLDetailsElement).open) void loadFolders(a.id); }}>
        <summary>{a.email} {#if (status.hidden[a.id] ?? []).length}<span class="count">({(status.hidden[a.id] ?? []).length} hidden)</span>{/if}</summary>
        {#if folders[a.id]}
          {#each hideableFolders(folders[a.id], status.folder) as f (f)}
            <label class="folder">
              <input
                type="checkbox"
                checked={isHidden(status.hidden, a.id, f)}
                disabled={!(status.hidden[a.id] ?? []).includes(f) && isHidden(status.hidden, a.id, f)}
                onchange={(e) => toggle(a.id, f, (e.currentTarget as HTMLInputElement).checked)}
              />
              {f.replace(/^Notes\//, '')}
            </label>
          {:else}
            <p>No folders.</p>
          {/each}
        {:else}
          <p>Loading…</p>
        {/if}
      </details>
    {/each}
  {/if}
  {#if message}<p class="message" role="status">{message}</p>{/if}
</section>

<section class="section">
  <h4>AI receipts</h4>
  <AiReceipts history />
</section>

<style>
  .section { margin-bottom: var(--space-4); display: flex; flex-direction: column; gap: var(--space-2); }
  h4 { margin: 0; font-size: var(--size-md); font-weight: 600; }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0; line-height: var(--leading); }
  summary { cursor: pointer; }
  .folder { display: flex; gap: var(--space-1-5); align-items: center; padding: var(--space-0-5) 0; }
  .error { color: var(--danger); }
  .count { color: var(--text-muted); }
</style>
