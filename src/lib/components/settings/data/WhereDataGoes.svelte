<script lang="ts">
  // Settings › Privacy & Data › Where your data goes (spec 2026-11-11 §1): every
  // channel that can send data off this device, read-only, each with a link
  // to where it is set. Reads only flag files and memory, never the
  // credential store (D7, gotcha #15): opening this page must not prompt.
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { invoke } from '@tauri-apps/api/core';
  import { accounts } from '../../../stores/notes';
  import { isAndroid } from '../../../stores/platform';
  import { autoCheck } from '../../../appUpdate';
  import { whereDataGoes, type Row, type Status } from './whereDataGoes';
  import { sectionLabel, type SettingsSection } from '../../../settings/route';

  let { leave }: { leave: (to: SettingsSection) => Promise<boolean> } = $props();

  let rows = $state<Row[] | null>(null);

  /**
   * The value, or undefined when the call failed: that row reads "Unknown".
   * Each call site names its command literally, so the IPC contract test
   * (lib.rs `ipc_contract`) can check every one is registered.
   */
  async function read<T>(cmd: string, call: Promise<T>): Promise<T | undefined> {
    try {
      return await call;
    } catch (e) {
      console.warn(`Privacy & Data: ${cmd} failed`, e);
      return undefined;
    }
  }

  onMount(() => {
    let alive = true;
    void (async () => {
      const android = get(isAndroid);
      const active = get(accounts).filter((a) => a.status === undefined || a.status === 'active');
      const [app, presets, stt, links, youtube, tvs, backups, workspace, files, skills, llms] = await Promise.all([
        read('get_app_llm_config', invoke<Status['app']>('get_app_llm_config')),
        read('list_agent_cli_presets', invoke<Status['presets']>('list_agent_cli_presets')),
        read('get_stt_config', invoke<Status['stt']>('get_stt_config')),
        read('get_link_settings', invoke<Status['links']>('get_link_settings')),
        read('get_youtube_key_status', invoke<boolean | null>('get_youtube_key_status')),
        read('list_tvs', invoke<unknown[]>('list_tvs')),
        read('backup_destinations', invoke<unknown[]>('backup_destinations')),
        android ? undefined : read('agent_workspace_status', invoke<Status['workspace']>('agent_workspace_status')),
        android ? undefined : read('agent_files_status', invoke<Status['files']>('agent_files_status')),
        android ? undefined : read('agent_skills_view', invoke<Status['skills']>('agent_skills_view')),
        Promise.all(active.map((a) => read('get_llm_settings', invoke<NonNullable<Status['accounts'][number]['llm']>>('get_llm_settings', { accountId: a.id })))),
      ]);
      if (!alive) return;
      rows = whereDataGoes({
        android,
        app,
        presets,
        stt,
        links,
        youtube,
        tvs,
        backups,
        workspace,
        files,
        skills,
        updateAutoCheck: get(autoCheck),
        accounts: active.map((a, i) => ({ id: a.id, email: a.email, llm: llms[i] ?? null })),
      });
    })();
    return () => { alive = false; };
  });
</script>

{#if rows === null}
  <p class="help" role="status">Reading…</p>
{:else}
  <ul class="channels">
    {#each rows as r (r.id)}
      <li class="channel" data-channel={r.id}>
        <div class="channel-text">
          <span class="channel-name">{r.name}</span>
          {#each r.lines as line, i (i)}<span class="channel-line">{line}</span>{/each}
          {#each r.warn as w, i (i)}<p class="help warn">{w}</p>{/each}
        </div>
        {#if r.change}
          {@const to = r.change}
          <!-- Names the page it opens: nothing is changed from this row. -->
          <button type="button" class="link-btn" aria-label={r.name === sectionLabel(to) ? `Open ${r.name}` : `Open ${sectionLabel(to)} for ${r.name}`} onclick={() => void leave(to)}>{sectionLabel(to)} ›</button>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .channels {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .channel {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-2) 0;
    border-top: 1px solid var(--border);
  }
  .channel:first-child { border-top: 0; }
  .channel-text {
    display: flex;
    flex-direction: column;
    gap: var(--space-0-5);
    min-width: 0;
  }
  .channel-name {
    font-size: var(--size);
    font-weight: var(--weight-semibold);
    color: var(--text);
  }
  .channel-line {
    font-size: var(--size-sm);
    color: var(--text-secondary);
    line-height: var(--leading);
    overflow-wrap: anywhere;
  }
  .help {
    font-size: var(--size-sm);
    color: var(--text-muted);
    margin: var(--space-1) 0 0;
    line-height: var(--leading);
  }
  .help.warn {
    color: var(--text);
    background: var(--warn-wash);
    border-left: 3px solid var(--accent);
    padding: var(--space-1-5) var(--space-2);
    border-radius: var(--radius-sm);
  }
  .link-btn {
    flex: none;
    background: none;
    border: none;
    color: var(--accent-action);
    cursor: pointer;
    padding: 0;
    font: inherit;
    font-size: var(--size-sm);
    text-decoration: underline;
    min-height: var(--hit-target-desktop);
  }
  .link-btn:focus-visible { box-shadow: var(--ring-focus); }
  :global(.modal.touch) .link-btn { min-height: var(--touch-target); }
</style>
