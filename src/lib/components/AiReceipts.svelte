<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { type AiReceipt, usageLabel, stageLabel } from '../aiReceipts';
  let { requestId = '', history = false }: { requestId?: string; history?: boolean } = $props();
  let rows = $state<AiReceipt[]>([]);
  let error = $state('');
  let days = $state(30);
  let exported = $state('');
  let exportGeneration = 0;
  let revision = $state(0);
  $effect(() => {
    const id = requestId;
    const all = history;
    void revision;
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    rows = [];
    error = '';
    if (!all && !id) return;
    async function load() {
      try {
        const result = await invoke<AiReceipt[]>('list_ai_receipts', { requestId: all ? null : id });
        if (alive) { rows = Array.isArray(result) ? result : []; error = ''; }
      } catch { if (alive) error = 'Execution details are unavailable.'; }
      if (alive) timer = setTimeout(load, 1000);
    }
    void load();
    if (all) void invoke<number>('get_ai_receipt_retention').then(value => { if (alive) days = value; }).catch(() => {});
    return () => { alive = false; clearTimeout(timer); };
  });
  async function remove(runId: string | null) {
    exportGeneration++; exported = '';
    try { await invoke('delete_ai_receipts', { runId }); exported = ''; revision++; }
    catch { error = 'Could not delete execution metadata.'; }
  }
  async function retain(event: Event) {
    exportGeneration++; exported = '';
    const next = Number((event.target as HTMLSelectElement).value);
    try { await invoke('set_ai_receipt_retention', { days: next }); days = next; exported = ''; revision++; }
    catch { error = 'Could not save retention preference.'; revision++; }
  }
  async function exportMetadata() {
    const ownGeneration = ++exportGeneration;
    try { const value = await invoke<string>('export_ai_receipts'); if (ownGeneration === exportGeneration) exported = value; }
    catch { error = 'Could not export execution metadata.'; }
  }
</script>

{#if history || requestId}
  <div class="ai-receipts">
    {#each rows as row (row.run_id)}
      {#each row.steps.filter(s => s.outcome === 'running') as step (step.id)}
        <p role="status">{stageLabel(step.stage)}…</p>
      {/each}
    {/each}
    <details>
      <summary>Execution receipts{rows.length ? ` (${rows.length})` : ''}</summary>
      <p>Stored on this device: execution metadata only. No note text, prompts, answers or conversation history. Run IDs do not authorize changes.</p>
      {#if history}
        <label>Keep metadata
          <select value={days} onchange={retain}>
            <option value={0}>This session only</option><option value={7}>7 days</option>
            <option value={30}>30 days</option><option value={90}>90 days</option>
          </select>
        </label>
        <p>Up to 200 runs. Expired metadata is removed when receipt storage is accessed. Deletion also removes in-flight metadata.</p>
        <div class="controls">
          <button type="button" onclick={() => remove(null)}>Delete all receipts</button>
          <button type="button" onclick={exportMetadata}>Export redacted metadata</button>
        </div>
        {#if exported}<label>Redacted export<textarea readonly value={exported} aria-label="Redacted receipt export"></textarea></label>{/if}
      {/if}
      {#if error}<p role="alert">{error}</p>{/if}
      {#if !rows.length && !error}<p>No execution metadata available.</p>{/if}
      {#each rows as row (row.run_id)}
        <article>
          <p>{row.calls.length} provider attempt(s) · {row.prompt_version}</p>
          {#if row.storage_failed}<p role="alert">Metadata could not be saved on this device.</p>{/if}
          <p>Run {row.run_id}</p>
          {#each row.steps as step (step.id)}
            <p>{step.kind.replaceAll('_', ' ')} · {step.outcome} · {step.latency_ms} ms</p>
            <p>Scope/version: {step.scope_version ? 'opaque snapshot recorded' : 'unknown'}</p>
            {#if step.metrics}<p>{Object.entries(step.metrics).map(([name, value]) => `${name.replaceAll('_', ' ')}: ${value}`).join(' · ')}</p>{/if}
            {#if step.checks.length}<p>Checks: {step.checks.map(c => c.replaceAll('_', ' ')).join(', ')}</p>{/if}
          {/each}
          <ol>
            {#each row.calls as call}
              <li>{call.provider === 'agent_cli' ? 'Agent CLI' : 'HTTP'} · {call.model ? `${call.model} (configured)` : 'model unknown or redacted'} · {stageLabel(call.stage)} · {call.outcome} · {call.latency_ms} ms<br />{usageLabel(call.usage)}</li>
            {/each}
          </ol>
          <p>Agent CLI attempts count subprocess runs; internal model calls are unknown. Unknown usage is not zero. Reported tokens are provider-reported; cancellation cannot undo charges. Citation ID checks do not prove factual support.</p>
          {#if history}<button type="button" onclick={() => remove(row.run_id)}>Delete receipt</button>{/if}
        </article>
      {/each}
    </details>
  </div>
{/if}
<style>
  .ai-receipts { font-size: 0.8rem; color: var(--text-muted); min-width: 0; }
  summary { cursor: pointer; padding: 0.5rem 0; }
  p, li { overflow-wrap: anywhere; }
  article { border-top: 1px solid var(--border); margin-top: 0.75rem; padding-top: 0.5rem; }
  .controls { display: flex; flex-wrap: wrap; gap: 0.5rem; margin: 0.5rem 0; }
  textarea { width: 100%; min-height: 9rem; box-sizing: border-box; }
  select, button { max-width: 100%; }
  select, textarea { background: var(--surface-panel); color: var(--text); border: 1px solid var(--border); }
</style>
