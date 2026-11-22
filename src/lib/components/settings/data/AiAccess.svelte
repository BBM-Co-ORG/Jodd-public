<script lang="ts">
  // Settings › Privacy & Data › AI access by account (spec 2026-11-11 §2): the two
  // per-account gates side by side — whether the account's data may go to
  // Jodd-managed AI at all, and which of its folders never leave. The switch
  // moved from the account page (D2) and applies at once (D3); the folder
  // block moved from the old Privacy page with its logic unchanged. Which
  // folders are hidden is always on screen (E4); only the chooser folds.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { accounts, accountDisplay } from '../../../stores/notes';
  import { hiddenFromAi } from '../../../stores/aiScope';
  import { hideableFolders, isHidden, withHidden, type AgentWorkspaceStatus } from '../../../agentWorkspace';
  import { DATA_ALLOWED_LABEL } from '../../../aiConsent';
  import { ALLOW_COVERS, HIDDEN_FOLDERS_HELP, PROVIDER_DISABLED } from '../../../settings/governanceCopy';

  let { leave }: { leave: (to: string) => Promise<boolean> } = $props();

  type Permission = { provider: string; data_allowed?: boolean | null };

  let status = $state<AgentWorkspaceStatus | null>(null);
  let statusError = $state('');
  let folders = $state<Record<string, string[]>>({});
  let perms = $state<Record<string, Permission | null>>({});
  let message = $state('');
  let alive = true;

  const active = $derived($accounts.filter((a) => a.status === undefined || a.status === 'active'));

  onMount(() => {
    void (async () => {
      try {
        const s = await invoke<AgentWorkspaceStatus>('agent_workspace_status');
        if (alive) status = s;
      } catch {
        if (alive) statusError = 'Hidden folders are unavailable.';
      }
    })();
    for (const a of active) {
      void invoke<Permission>('get_llm_settings', { accountId: a.id })
        .then((p) => { if (alive) perms = { ...perms, [a.id]: p }; })
        .catch(() => { if (alive) perms = { ...perms, [a.id]: null }; });
    }
    return () => { alive = false; };
  });

  /** The same rule as Rust `account_allowed` (the caller lists active accounts only). */
  const allowedOf = (p: Permission | null | undefined) => !!p && p.provider !== 'disabled' && p.data_allowed === true;

  // Local-first: the switch moves now; a refusal puts back this account only,
  // so a second account switched while this one was saving keeps its change.
  async function setAllowed(accountId: string, email: string, next: boolean) {
    const prev = perms[accountId];
    if (!prev) return;
    perms = { ...perms, [accountId]: { ...prev, data_allowed: next } };
    message = '';
    try {
      if (next) await invoke('allow_ai_for_account', { accountId });
      else await invoke('deny_ai_for_account', { accountId });
    } catch (e) {
      if (!alive) return;
      const cur = perms[accountId];
      if (cur) perms = { ...perms, [accountId]: { ...cur, data_allowed: prev.data_allowed } };
      message = `Could not change AI access for ${email}: ${e}`;
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

  // Moved from PrivacyPage: undo only this folder on a refusal.
  async function toggle(accountId: string, folder: string, hide: boolean) {
    if (!status) return;
    status = { ...status, hidden: withHidden(status.hidden, accountId, folder, hide) };
    hiddenFromAi.set(status.hidden);
    message = '';
    try {
      await invoke('set_folder_hidden_from_agents', { accountId, folder, hidden: hide });
    } catch (e) {
      if (alive && status) {
        status = { ...status, hidden: withHidden(status.hidden, accountId, folder, !hide) };
        hiddenFromAi.set(status.hidden);
        message = `Could not change ${folder}: ${e}`;
      }
    }
  }

  const short = (f: string) => f.replace(/^Notes\//, '');
</script>

<p class="help">{HIDDEN_FOLDERS_HELP}</p>
<p class="help">{ALLOW_COVERS}</p>

{#if status?.error}
  <p class="error" role="alert">
    {status.scope_path} cannot be read ({status.error}). Fix it by hand; Jodd will not overwrite it.
  </p>
{:else if statusError}
  <p class="error" role="alert">{statusError}</p>
{/if}

{#each active as a (a.id)}
  {@const p = perms[a.id]}
  {@const hidden = status?.hidden[a.id] ?? []}
  <div class="account" data-account-id={a.id}>
    <span class="account-name">{accountDisplay(a)}</span>
    {#if p === null}
      <p class="error">AI access for this account cannot be read.</p>
    {:else}
      <label class="toggle-row">
        <input
          type="checkbox"
          checked={allowedOf(p)}
          disabled={!p || p.provider === 'disabled'}
          onchange={(e) => setAllowed(a.id, a.email, (e.currentTarget as HTMLInputElement).checked)}
        />
        <span>{DATA_ALLOWED_LABEL}</span>
      </label>
      {#if p?.provider === 'disabled'}
        <p class="help">
          {PROVIDER_DISABLED}
          <button type="button" class="link-btn" onclick={() => void leave('accounts/' + a.id)}>Open account settings</button>
        </p>
      {/if}
    {/if}
    {#if status && !status.error}
      <p class="hidden-names">
        {hidden.length ? `Hidden from AI: ${hidden.map(short).join(', ')}` : 'No folders hidden'}
      </p>
      <details class="folders" ontoggle={(e) => { if ((e.currentTarget as HTMLDetailsElement).open) void loadFolders(a.id); }}>
        <summary>Choose folders</summary>
        {#if folders[a.id]}
          {#each hideableFolders(folders[a.id], status.folder) as f (f)}
            <label class="folder">
              <input
                type="checkbox"
                checked={isHidden(status.hidden, a.id, f)}
                disabled={!(status.hidden[a.id] ?? []).includes(f) && isHidden(status.hidden, a.id, f)}
                onchange={(e) => toggle(a.id, f, (e.currentTarget as HTMLInputElement).checked)}
              />
              {short(f)}
            </label>
          {:else}
            <p>No folders.</p>
          {/each}
        {:else}
          <p>Loading…</p>
        {/if}
      </details>
    {/if}
  </div>
{:else}
  <p class="help">No active account.</p>
{/each}
{#if message}<p class="message" role="status">{message}</p>{/if}

<style>
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0; line-height: var(--leading); }
  .account {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2) 0;
    border-top: 1px solid var(--border);
  }
  .account:first-of-type { margin-top: var(--space-1); }
  .account-name { font-size: var(--size); font-weight: var(--weight-semibold); color: var(--text); overflow-wrap: anywhere; }
  .toggle-row { display: flex; align-items: center; gap: var(--space-2); font-size: var(--size); color: var(--text); cursor: pointer; }
  .toggle-row input { margin: 0; }
  .hidden-names { margin: 0; font-size: var(--size-sm); color: var(--text-secondary); line-height: var(--leading); }
  summary { cursor: pointer; font-size: var(--size-sm); color: var(--accent-action); }
  .folder { display: flex; gap: var(--space-1-5); align-items: center; padding: var(--space-0-5) 0; }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font: inherit; font-size: var(--size-sm); text-decoration: underline;
  }
  .error { color: var(--danger); font-size: var(--size-sm); margin: 0; }
  .message { font-size: var(--size-sm); color: var(--text-secondary); margin: var(--space-1) 0 0; }
</style>
