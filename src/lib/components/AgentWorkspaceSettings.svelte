<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { accounts, refreshNotes } from '../stores/notes';
  import { hiddenFromAi } from '../stores/aiScope';
  import {
    hideableFolders, isHidden, withHidden, HOOK_SNIPPET,
    type AgentWorkspaceStatus,
  } from '../agentWorkspace';

  let status = $state<AgentWorkspaceStatus | null>(null);
  let choice = $state('');
  let busy = $state(false);
  let message = $state('');
  let folders = $state<Record<string, string[]>>({});
  let alive = true;

  async function load() {
    try {
      const s = await invoke<AgentWorkspaceStatus>('agent_workspace_status');
      if (!alive) return;
      status = s;
      if (!choice && s.eligible.length) choice = s.eligible[0].account_id;
    } catch {
      if (alive) message = 'Agent workspace settings are unavailable.';
    }
  }

  onMount(() => {
    void load();
    return () => { alive = false; };
  });

  async function enable() {
    if (!choice || busy) return;
    busy = true; message = '';
    try {
      await invoke('enable_agent_workspace', { accountId: choice });
      // Gotcha #6, measured in UAT 2026-10-06: the command's `remote-changed`
      // nudge goes through requestRefresh's 2 s throttle, which DROPS a
      // request rather than deferring it — enabling right after any other
      // refresh left the new pages out of the note list until a manual ⟳.
      // The manual refresh skips the throttle, so this is the route that
      // reaches the pixel.
      await $refreshNotes();
      await load();
      if (alive) message = 'Agent workspace enabled. Agents connected to Jodd can now remember what they learn.';
    } catch (e) {
      if (alive) message = `Could not enable the agent workspace: ${e}`;
    } finally {
      if (alive) busy = false;
    }
  }

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

  function emailOf(id: string | null): string {
    return $accounts.find(a => a.id === id)?.email ?? id ?? '';
  }
</script>

<div class="agent-workspace">
  <p>
    A shared memory for the AI agents you connect to Jodd (Claude Code, Claude Desktop and others).
    They save decisions, lessons and your preferences as pages in one folder, and read them back
    before they start work. Your own folders are never rewritten by agents.
  </p>

  {#if status?.error}
    <p class="error" role="alert">
      {status.scope_path} cannot be read ({status.error}). Fix it by hand; Jodd will not overwrite it.
    </p>
  {:else if status && !status.account_id}
    {#if status.eligible.length === 0}
      <p>No account here can hold the workspace: it needs an account where Jodd can create folders.</p>
    {:else}
      <label>Account
        <select bind:value={choice} disabled={busy}>
          {#each status.eligible as e (e.account_id)}
            <option value={e.account_id}>{e.email}</option>
          {/each}
        </select>
      </label>
      <button onclick={enable} disabled={busy || !choice}>{busy ? 'Enabling…' : 'Enable agent workspace'}</button>
    {/if}
  {:else if status}
    <p class="enabled">Enabled: <code>{status.folder}</code> in {emailOf(status.account_id)}.</p>

    <h4>Hidden from agents</h4>
    <p>Agents never see notes in these folders, in any tool.</p>
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

    <h4>Session briefing for Claude Code</h4>
    <p>Add this to Claude Code's settings so every session starts with what agents saved for that project:</p>
    <pre>{HOOK_SNIPPET}</pre>
  {/if}

  {#if message}<p class="message" role="status">{message}</p>{/if}
</div>

<style>
  .agent-workspace { display: flex; flex-direction: column; gap: 8px; }
  .folder { display: flex; gap: 6px; align-items: center; padding: 2px 0; }
  .error { color: var(--danger); }
  .count { color: var(--text-muted); }
  select { padding: 6px; font: inherit; background: var(--surface-panel); color: var(--text); }
  pre { white-space: pre-wrap; word-break: break-all; font-family: var(--font-mono); font-size: var(--size-xs); line-height: var(--leading); padding: 8px; border-radius: 6px; background: var(--surface-sunken); }
  h4 { margin: 8px 0 0; }
</style>
