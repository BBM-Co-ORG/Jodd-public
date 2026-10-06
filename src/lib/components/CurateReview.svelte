<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { refreshNotes } from '../stores/notes';
  import Icon from './Icon.svelte';
  import {
    appendPreview, choices, folderName, sameAction, summaryLine,
    type CurateAction, type NoteText, type Proposal, type ScanSummary,
  } from '../curate';

  let { accountId, onClose }: { accountId: string; onClose: () => void } = $props();

  let proposals = $state<Proposal[]>([]);
  let chosen = $state<Record<number, CurateAction>>({});
  let previews = $state<Record<number, NoteText[]>>({});
  let busy = $state<Record<number, boolean>>({});
  let errors = $state<Record<number, string>>({});
  let scanning = $state(false);
  let summary = $state('');
  let loadError = $state('');
  let alive = true;

  const KIND_TITLE = { duplicate: 'Duplicates', misfiled: 'Misfiled notes', secret: 'Secrets in notes' } as const;

  async function load() {
    try {
      const list = await invoke<Proposal[]>('curate_list', { accountId });
      if (!alive) return;
      proposals = list;
      for (const p of list) if (!chosen[p.id]) chosen[p.id] = p.payload.action;
    } catch (e) {
      if (alive) loadError = String(e);
    }
  }

  onMount(() => {
    void load();
    return () => { alive = false; };
  });

  async function scan() {
    if (scanning) return;
    scanning = true; summary = '';
    try {
      const s = await invoke<ScanSummary>('curate_scan', { accountId, requestId: crypto.randomUUID() });
      if (alive) summary = summaryLine(s);
      await load();
    } catch (e) {
      if (alive) summary = `Could not finish: ${e}`;
    } finally {
      if (alive) scanning = false;
    }
  }

  async function togglePreview(p: Proposal) {
    if (previews[p.id]) { const { [p.id]: _, ...rest } = previews; previews = rest; return; }
    try {
      const texts = await invoke<NoteText[]>('curate_preview', { accountId, id: p.id });
      if (alive) previews = { ...previews, [p.id]: texts };
    } catch (e) {
      if (alive) errors = { ...errors, [p.id]: String(e) };
    }
  }

  async function decide(p: Proposal, approve: boolean) {
    if (busy[p.id]) return;
    busy = { ...busy, [p.id]: true };
    errors = { ...errors, [p.id]: '' };
    try {
      if (approve) {
        await invoke('curate_apply', { accountId, id: p.id, action: chosen[p.id] ?? p.payload.action });
        // Gotcha #6: apply wrote SQLite from Rust; the list learns of it here.
        await $refreshNotes();
      } else {
        await invoke('curate_dismiss', { accountId, id: p.id });
      }
      if (alive) proposals = proposals.filter(x => x.id !== p.id);
    } catch (e) {
      if (alive) { errors = { ...errors, [p.id]: String(e) }; await load(); }
    } finally {
      if (alive) busy = { ...busy, [p.id]: false };
    }
  }

  const grouped = $derived((['secret', 'duplicate', 'misfiled'] as const)
    .map(kind => ({ kind, items: proposals.filter(p => p.kind === kind) }))
    .filter(g => g.items.length > 0));
</script>

<div class="backdrop" role="presentation" onclick={onClose}></div>
<div class="modal" role="dialog" aria-modal="true" aria-labelledby="curate-title">
  <header>
    <h2 id="curate-title">Organize</h2>
    <button class="icon" aria-label="Close" onclick={onClose}><Icon name="close" size={14} /></button>
  </header>
  <p class="intro">
    Jodd looks for duplicate notes, notes left unfiled, and passwords or keys in plain text.
    Nothing changes until you approve it, and a removed note goes to the trash, where you can restore it.
  </p>
  <div class="actions">
    <button class="primary" onclick={scan} disabled={scanning}>{scanning ? 'Looking…' : 'Find problems'}</button>
    {#if summary}<span class="summary" role="status">{summary}</span>{/if}
  </div>
  {#if loadError}<p class="error" role="alert">{loadError}</p>{/if}

  <div class="list">
    {#each grouped as g (g.kind)}
      <h3>{KIND_TITLE[g.kind]}</h3>
      {#each g.items as p (p.id)}
        <article class="card" class:stale={p.status !== 'pending'} data-proposal={p.id}>
          <ul class="notes">
            {#each p.payload.notes as n (n.uuid)}
              <li><strong>{n.title}</strong> <span class="meta">{folderName(n.label)} · {n.chars.toLocaleString()} characters</span></li>
            {/each}
          </ul>
          <p class="reason">{p.payload.reason}</p>
          {#if p.payload.evidence.length}<p class="evidence">{p.payload.evidence.join(' · ')}</p>{/if}

          {#if p.status !== 'pending'}
            <p class="error">{p.error ?? 'This changed since it was proposed.'} Find problems again for a fresh proposal.</p>
            <div class="buttons"><button onclick={() => decide(p, false)} disabled={busy[p.id]}>Dismiss</button></div>
          {:else}
            {@const opts = choices(p)}
            {#if opts.length > 1}
              <label class="choice">Action
                <select onchange={(e) => { chosen = { ...chosen, [p.id]: opts[(e.currentTarget as HTMLSelectElement).selectedIndex].action }; }}>
                  {#each opts as o, i (i)}
                    <option selected={sameAction(o.action, chosen[p.id] ?? p.payload.action)}>{o.label}</option>
                  {/each}
                </select>
              </label>
            {:else if opts.length === 1}
              <p class="choice">{opts[0].label}</p>
            {/if}

            <div class="buttons">
              <button onclick={() => togglePreview(p)}>{previews[p.id] ? 'Hide preview' : 'Preview'}</button>
              <button onclick={() => decide(p, false)} disabled={busy[p.id]}>Dismiss</button>
              <button class="primary" onclick={() => decide(p, true)} disabled={busy[p.id]}>{busy[p.id] ? 'Applying…' : 'Approve'}</button>
            </div>

            {#if previews[p.id]}
              {@const a = chosen[p.id] ?? p.payload.action}
              <div class="preview" class:single={previews[p.id].length === 1}>
                {#each previews[p.id] as t (t.uuid)}
                  <section>
                    <h4>{t.title} <span class="meta">{folderName(t.label)}</span>
                      {#if a.type === 'keep'}<span class="tag">{a.keep === t.uuid ? 'kept' : 'to trash'}</span>{/if}
                      {#if a.type === 'append'}<span class="tag">{a.into === t.uuid ? 'kept' : 'appended, then trashed'}</span>{/if}
                    </h4>
                    <pre>{t.text}</pre>
                  </section>
                {/each}
              </div>
              {#if a.type === 'append'}
                <section class="merged"><h4>Result</h4><pre>{appendPreview(previews[p.id], a.into)}</pre></section>
              {/if}
            {/if}
          {/if}
          {#if errors[p.id]}<p class="error" role="alert">{errors[p.id]}</p>{/if}
        </article>
      {/each}
    {:else}
      {#if !scanning}<p class="empty">No open proposals. Use Find problems to look.</p>{/if}
    {/each}
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: var(--overlay, var(--surface-active)); z-index: 40; }
  .modal { position: fixed; inset: 5vh 6vw; z-index: 41; display: flex; flex-direction: column; gap: 10px; padding: 16px 20px; border-radius: 10px; background: var(--surface-panel); color: var(--text); overflow: hidden; }
  header { display: flex; align-items: center; justify-content: space-between; }
  h2 { margin: 0; }
  .icon { background: none; border: none; color: var(--text); cursor: pointer; }
  .intro, .meta, .evidence, .empty, .summary { color: var(--text-muted); }
  .actions { display: flex; gap: 12px; align-items: center; }
  .list { overflow: auto; display: flex; flex-direction: column; gap: 10px; padding-bottom: 12px; }
  h3 { margin: 6px 0 0; }
  .card { border: 1px solid var(--border, var(--surface-active)); border-radius: 8px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .card.stale { opacity: 0.75; }
  .notes { margin: 0; padding-left: 18px; }
  .reason { margin: 0; }
  .evidence { margin: 0; font-size: var(--size-sm); }
  .choice select { margin-left: 6px; padding: 4px; font: inherit; background: var(--surface-panel); color: var(--text); max-width: 100%; }
  .buttons { display: flex; gap: 8px; justify-content: flex-end; }
  .preview { display: grid; grid-template-columns: repeat(auto-fit, minmax(260px, 1fr)); gap: 8px; }
  .preview.single { grid-template-columns: 1fr; }
  pre { white-space: pre-wrap; word-break: break-word; max-height: 320px; overflow: auto; margin: 0; padding: 8px; border-radius: 6px; background: var(--surface-sunken); font-family: var(--font-mono); font-size: var(--size-xs); line-height: var(--leading); }
  h4 { margin: 0 0 4px; font-size: var(--size-sm); }
  .tag { margin-left: 6px; padding: 1px 6px; border-radius: 8px; background: var(--surface-active); font-size: var(--size-xs); }
  .error { color: var(--danger); margin: 0; }
  /* UAT 2026-10-06: unstyled, "Find problems" read as a caption, not a button. */
  button:not(.icon) { padding: 4px 12px; border-radius: 6px; border: 1px solid var(--border, var(--surface-active)); background: var(--surface-panel); color: var(--text); font: inherit; cursor: pointer; }
  button:not(.icon):disabled { opacity: 0.6; cursor: default; }
  .primary { font-weight: 600; background: var(--accent, var(--surface-active)); color: var(--text-inverse); }
</style>
