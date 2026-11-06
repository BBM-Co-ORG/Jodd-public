<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { pickFolder } from '../backup/pickFolder';
  import {
    heldBack, hookSnippet, onAgentFilesSynced, permissionSnippet, summarize, withAccount,
    type AgentFilesView, type SyncOutcome,
  } from '../agentFiles';

  // Notes on disk for Claude Code (spec 2026-11-05 §5.7), desktop only. The
  // status on screen is re-read when this section opens and when the window
  // regains focus (a sync the hook ran), and on `agent-files-synced` (one the
  // app ran) — gotcha #6: nothing else tells this component.

  const FOLDER_NAME = 'Jodd for agents';

  let view = $state<AgentFilesView | null>(null);
  let pathDraft = $state('');
  let busy = $state(false);
  let syncing = $state(false);
  let message = $state('');
  let alive = true;

  async function load() {
    try {
      const v = await invoke<AgentFilesView>('agent_files_status');
      if (!alive) return;
      view = v;
      pathDraft = v.path;
    } catch {
      if (alive) message = 'These settings are unavailable.';
    }
  }

  onMount(() => {
    void load();
    const onFocus = () => { if (!busy && !syncing) void load(); };
    window.addEventListener('focus', onFocus);
    // The app's own live sync, which nobody clicked (gotcha #6).
    const stopSynced = onAgentFilesSynced(listen, onFocus);
    return () => { alive = false; window.removeEventListener('focus', onFocus); stopSynced(); };
  });

  /** Optimistic: the screen changes at once and goes back if saving fails. */
  async function save(next: { enabled: boolean; path: string; accounts: string[] }, thenSync: boolean) {
    if (!view || busy) return;
    const before = view;
    view = { ...view, ...next };
    busy = true; message = '';
    try {
      const saved = await invoke<AgentFilesView>('set_agent_mirror', {
        enabled: next.enabled,
        path: next.path === before.default_path ? null : next.path,
        accounts: next.accounts,
      });
      if (!alive) return;
      view = saved;
      pathDraft = saved.path;
      if (saved.notice) message = saved.notice;
    } catch (e) {
      if (!alive) return;
      view = before;
      pathDraft = before.path;
      message = String(e);
      return;
    } finally {
      if (alive) busy = false;
    }
    if (thenSync && view?.enabled) await syncNow(false);
  }

  function toggle(enabled: boolean) {
    if (!view) return;
    void save({ enabled, path: view.path, accounts: view.accounts }, enabled);
  }

  function toggleAccount(id: string, on: boolean) {
    if (!view) return;
    void save({ enabled: view.enabled, path: view.path, accounts: withAccount(view.accounts, id, on) }, view.enabled);
  }

  function applyPath() {
    if (!view || pathDraft.trim() === view.path) return;
    void save({ enabled: view.enabled, path: pathDraft.trim(), accounts: view.accounts }, view.enabled);
  }

  async function choose() {
    if (!view) return;
    const picked = await pickFolder(`Choose where to put the “${FOLDER_NAME}” folder`, () => !alive);
    if (!picked || !alive || !view) return;
    const sep = picked.includes('\\') && !picked.includes('/') ? '\\' : '/';
    const path = picked.endsWith(FOLDER_NAME) ? picked : `${picked.replace(/[\\/]+$/, '')}${sep}${FOLDER_NAME}`;
    pathDraft = path;
    void save({ enabled: view.enabled, path, accounts: view.accounts }, view.enabled);
  }

  async function syncNow(allowLargeDelete: boolean) {
    if (syncing) return;
    syncing = true; message = '';
    try {
      const out = await invoke<SyncOutcome>('sync_agent_files_now', { allowLargeDelete });
      if (!alive) return;
      if (view) view = { ...view, last: out.mirror };
      if (out.mirror_error) message = `Sync failed: ${out.mirror_error}`;
    } catch (e) {
      if (alive) message = `Sync failed: ${e}`;
    } finally {
      if (alive) syncing = false;
    }
  }

  function emailOf(id: string): string {
    return view?.choices.find(c => c.account_id === id)?.email ?? id;
  }
</script>

<div class="agent-files">
  <p class="help">A read-only copy of your notes as Markdown files, for Claude Code to browse and search.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Jodd keeps the copy up to date each time a Claude Code session starts, using the hook below.
      Folders you hide from AI are left out, and secrets such as passwords and API keys are replaced
      by a mask. The copy is not encrypted, and only the accounts you choose are in it. Turning this
      off removes it.
    </p>
  </details>

  {#if view?.error}
    <p class="error" role="alert">The agent settings file cannot be read ({view.error}). Fix it by hand; Jodd will not overwrite it.</p>
  {:else if view}
    <label class="row">
      <input type="checkbox" checked={view.enabled} disabled={busy} onchange={e => toggle(e.currentTarget.checked)} />
      Keep a copy of my notes on disk for Claude Code
    </label>

    {#if view.enabled}
      <div class="field">
        <span class="label">Folder</span>
        <div class="path">
          <input type="text" bind:value={pathDraft} disabled={busy} onchange={applyPath} aria-label="Folder for the copy" />
          <button onclick={choose} disabled={busy}>Choose…</button>
        </div>
      </div>

      <fieldset class="field">
        <legend class="label">Accounts</legend>
        {#if view.choices.length === 0}
          <p class="help">No accounts yet.</p>
        {/if}
        {#each view.choices as c (c.account_id)}
          <label class="row">
            <input
              type="checkbox"
              checked={view.accounts.includes(c.account_id)}
              disabled={busy}
              onchange={e => toggleAccount(c.account_id, e.currentTarget.checked)}
            />
            {c.email}
          </label>
        {/each}
      </fieldset>

      <div class="row">
        <button onclick={() => syncNow(false)} disabled={busy || syncing || view.accounts.length === 0}>
          {syncing ? 'Syncing…' : 'Sync now'}
        </button>
        {#if view.last}<span class="help">{summarize(view.last)}</span>{/if}
      </div>

      {#if view.last}
        {#each view.last.accounts.filter(a => a.skipped) as a (a.account_id)}
          <p class="help">{emailOf(a.account_id)}: {a.skipped}</p>
        {/each}
        {#each view.last.problems as p, i (i)}
          <p class="warn" role="status">{p}</p>
        {/each}
        {#if heldBack(view.last)}
          <button class="danger" onclick={() => syncNow(true)} disabled={syncing}>Apply anyway</button>
        {/if}
      {/if}

      <h4>Hook for Claude Code</h4>
      <p class="help">Add this to Claude Code's settings (it replaces Jodd's briefing-only hook, if you added that):</p>
      <pre>{hookSnippet(true)}</pre>
      <p class="help">And this, so Claude Code can read the folder without asking each time:</p>
      <pre>{permissionSnippet(view.path)}</pre>
    {/if}
  {/if}

  {#if message}<p class="message" role="status">{message}</p>{/if}
</div>

<style>
  .agent-files { display: flex; flex-direction: column; gap: 8px; }
  .help { font-size: var(--size-sm); color: var(--text-muted); line-height: var(--leading); }
  .error, .danger { color: var(--danger); }
  .warn { font-size: var(--size-sm); color: var(--warning, var(--danger)); }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .field { display: flex; flex-direction: column; gap: 4px; border: 0; padding: 0; margin: 0; }
  .label { font-size: var(--size-sm); font-weight: 600; }
  .path { display: flex; gap: 6px; }
  .path input { flex: 1; min-width: 0; padding: 6px; font: inherit; background: var(--surface-panel); color: var(--text); }
  pre { white-space: pre-wrap; word-break: break-all; font-family: var(--font-mono); font-size: var(--size-xs); line-height: var(--leading); padding: 8px; border-radius: 6px; background: var(--surface-sunken); }
  h4 { margin: 8px 0 0; }
</style>
