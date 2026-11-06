<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { listen } from '@tauri-apps/api/event';
  import { hookSnippet, onAgentFilesSynced, type SyncOutcome } from '../agentFiles';
  import { skillStateLine, SKILL_PREFIX, type SkillsView } from '../agentSkills';

  // Skills for Claude Code (spec 2026-11-05 §7), desktop only. Re-read on
  // open, when the window regains focus (a sync the hook ran), and on
  // `agent-files-synced` (one the app ran) — gotcha #6.

  let view = $state<SkillsView | null>(null);
  let busy = $state(false);
  let message = $state('');
  let alive = true;

  async function load() {
    try {
      const v = await invoke<SkillsView>('agent_skills_view');
      if (alive) view = v;
    } catch {
      if (alive) message = 'Skill settings are unavailable.';
    }
  }

  onMount(() => {
    void load();
    const onFocus = () => { if (!busy) void load(); };
    window.addEventListener('focus', onFocus);
    // The app's own live sync, which nobody clicked (gotcha #6).
    const stopSynced = onAgentFilesSynced(listen, onFocus);
    return () => { alive = false; window.removeEventListener('focus', onFocus); stopSynced(); };
  });

  /** Optimistic: the row goes at once and comes back if removing fails. */
  async function stop(name: string) {
    if (!view || busy) return;
    const before = view;
    view = { ...view, skills: view.skills.filter(s => s.name !== name) };
    busy = true; message = '';
    try {
      const v = await invoke<SkillsView>('remove_agent_skill', { name });
      if (alive) view = v;
    } catch (e) {
      if (alive) { view = before; message = String(e); }
    } finally {
      if (alive) busy = false;
    }
  }

  async function syncNow() {
    if (busy) return;
    busy = true; message = '';
    try {
      const out = await invoke<SyncOutcome>('sync_agent_files_now', { allowLargeDelete: false });
      if (out.skills_error && alive) message = `Sync failed: ${out.skills_error}`;
      await load();
    } catch (e) {
      if (alive) message = `Sync failed: ${e}`;
    } finally {
      if (alive) busy = false;
    }
  }
</script>

<div class="agent-skills">
  <p class="help">
    Publish a folder as a skill from its menu (⋯ › Publish as Claude Code skill…). Claude Code then follows
    those notes on its own when a task matches — a brand guide when it makes slides, a playbook when it
    starts a project.
  </p>
  <p class="help">
    To tie a skill to one project, add a line <code>- [skill] {SKILL_PREFIX}name — what for</code> to the project's page in
    the agent workspace, or ask the agent to remember it. Claude Code is then told to use it whenever it works on that project.
  </p>

  {#if view?.error}
    <p class="error" role="alert">The agent settings file cannot be read ({view.error}). Fix it by hand; Jodd will not overwrite it.</p>
  {:else if view}
    {#if view.skills.length === 0}
      <p class="help">No skills yet.</p>
    {:else}
      <ul class="skills">
        {#each view.skills as s (s.name)}
          <li>
            <div class="head">
              <code>{SKILL_PREFIX}{s.name}</code>
              <span class="where">{s.folder} · {s.email || s.account_id}</span>
              <button class="danger" onclick={() => stop(s.name)} disabled={busy}>Stop publishing</button>
            </div>
            <p class="help">{skillStateLine(s)}</p>
            {#if s.status?.waiting_for_review.length}
              <p class="help">Waiting for review: {s.status.waiting_for_review.join(', ')}</p>
            {/if}
            {#each s.status?.problems ?? [] as p, i (i)}<p class="warn">{p}</p>{/each}
          </li>
        {/each}
      </ul>
      <div class="row">
        <button onclick={syncNow} disabled={busy}>{busy ? 'Syncing…' : 'Sync now'}</button>
        <span class="help">Skills are in {view.skills_root}.</span>
      </div>
    {/if}
    {#each view.problems as p, i (i)}<p class="warn">{p}</p>{/each}
    {#if view.skills.length}
      <p class="help">Skills update each time a Claude Code session starts, with this hook in Claude Code's settings (the same one as for notes on disk):</p>
      <pre>{hookSnippet(true)}</pre>
    {/if}
  {/if}

  {#if message}<p class="message" role="status">{message}</p>{/if}
</div>

<style>
  .agent-skills { display: flex; flex-direction: column; gap: 8px; }
  .help { font-size: var(--size-sm); color: var(--text-muted); line-height: var(--leading); margin: 0; }
  .error, .danger { color: var(--danger); }
  .warn { font-size: var(--size-sm); color: var(--warning, var(--danger)); margin: 0; }
  .skills { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 10px; }
  .head { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .where { font-size: var(--size-sm); color: var(--text-muted); flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  pre { white-space: pre-wrap; word-break: break-all; font-family: var(--font-mono); font-size: var(--size-xs); line-height: var(--leading); padding: 8px; border-radius: 6px; background: var(--surface-sunken); }
</style>
