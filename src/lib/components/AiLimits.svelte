<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import type { AiLimits } from '../aiLimits';
  let limits = $state<AiLimits | null>(null);
  let message = $state('');
  let saving = $state(false);
  let alive = true;
  // What is on disk, as last loaded or saved; isDirty() compares against it.
  let baseline = $state('');
  onMount(() => {
    void invoke<AiLimits>('get_ai_limits').then(value => { if (alive) { limits = value; baseline = JSON.stringify(value); } })
      .catch(() => { if (alive) message = 'AI limits are unavailable.'; });
    return () => { alive = false; };
  });
  const SAVE_FAILED = 'Could not save AI limits. Check the values and try again.';
  // Section-form contract (lib/settings/sectionForm.ts). No Save button of its
  // own: Settings › AI's one Save calls save().
  export function isDirty(): boolean {
    return limits !== null && JSON.stringify(limits) !== baseline;
  }
  // The write in progress; a re-entrant save() returns it rather than
  // resolving before the write finishes.
  let inFlight: Promise<void> | null = null;
  // Throws a user-readable message when the write fails.
  export function save(): Promise<void> {
    if (inFlight) return inFlight;
    if (!limits) return Promise.reject(new Error('AI limits are unavailable.'));
    const run = write(limits).finally(() => { if (inFlight === run) inFlight = null; });
    inFlight = run;
    return run;
  }
  async function write(current: AiLimits): Promise<void> {
    saving = true; message = '';
    try {
      const sent = { ...current };
      await invoke('set_ai_limits', { settings: sent });
      baseline = JSON.stringify(sent);
      if (alive) message = 'AI limits saved. Existing workflows keep their workflow allowance; session capacity and enrichment are checked before further attempts.';
    } catch {
      // Shown once, by the Settings page above this section.
      throw new Error(SAVE_FAILED);
    } finally { if (alive) saving = false; }
  }
</script>
<details class="ai-limits">
  <summary>AI limits and automatic enrichment</summary>
  <p class="help">Off by default. Jodd can suggest a folder and related links after saving an AI result.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>Each follow-up uses the same workflow allowance.</p>
    <p>Planning units estimate full request bytes plus output allowance; they are not billed tokens or money. Reported HTTP usage reconciles the allowance. Unknown usage, including unreported failures and cancellations, keeps its reserved units. Cancellation does not reverse charges.</p>
    <p>Retries, source summaries, synthesis, conversation history and follow-ups share the allowance. When enrichment is enabled, unused capacity stays reserved for follow-ups for 10 minutes after work finishes; otherwise it is released. Session capacity resets when the app restarts.</p>
    <p>CLI attempts count subprocesses. Internal model calls, tokens and output limits remain unknown. HTTP limits depend on provider support. These are best-effort usage controls, not a hard spending cap. Subscriptions are not free usage. No model override, provider fallback or currency estimate is applied.</p>
  </details>
  {#if limits}
    <fieldset disabled={saving}>
      <label class="toggle"><input type="checkbox" bind:checked={limits.automatic_enrichment} /> Automatically suggest folders and related links</label>
      <label>Concurrent work <input type="number" min="1" max="8" bind:value={limits.max_concurrent} /></label>
      <label>Attempts per workflow <input type="number" min="1" max="64" bind:value={limits.max_attempts} /></label>
      <label>Planning units per workflow <input type="number" min="256" max="10000000" bind:value={limits.workflow_units} /></label>
      <label>Planning units per app session <input type="number" min="256" max="100000000" bind:value={limits.session_units} /></label>
      <label>Output token allowance per attempt <input type="number" min="256" max="32768" bind:value={limits.output_tokens} /></label>
      <label>HTTP output limit support <select bind:value={limits.output_parameter}>
        <option value="max_tokens">max_tokens</option>
        <option value="max_completion_tokens">max_completion_tokens</option>
        <option value="unsupported">Unsupported — best effort only</option>
      </select></label>
    </fieldset>
  {/if}
  {#if message}<p role="status">{message}</p>{/if}
</details>
<style>
  .ai-limits { margin: 12px 0; color: var(--text); font-size: var(--size-sm); }
  summary { cursor: pointer; font-weight: 600; }
  p { color: var(--text-muted); line-height: 1.5; }
  fieldset { border: 0; padding: 0; display: grid; gap: 10px; min-width: 0; }
  label { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; justify-content: space-between; }
  .toggle { justify-content: flex-start; }
  input, select { color: var(--text); background: var(--surface-panel); border: 1px solid var(--border); border-radius: 6px; padding: 6px; max-width: 100%; }
  input[type=number] { width: 130px; }
</style>
