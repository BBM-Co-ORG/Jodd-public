<script lang="ts">
  // Research Studio's dialog (spec 2026-10-23 §3): pick an output, optionally
  // a focus, and which of the folder's notes go in; see the estimate; run it
  // with progress and cancel. The note saves directly and opens — no preview.
  // Below the folder's notes, notes from other folders (spec 2026-10-25 §3)
  // can be ticked in as extra sources; they are never ticked by default.
  // Open while `studioRequest` is set (the folder menu sets it).
  import { onDestroy, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import { invoke, Channel } from '@tauri-apps/api/core';
  import Modal from './Modal.svelte';
  import { selectedFolder } from '../stores/notes';
  import type { Pane } from '../stores/phoneNav';
  import type { ExtractedNote } from '../types';
  import { viewName } from '../viewName';
  import { displayTitle } from '../textDisplay';
  import { stripWorkflowMarkers } from './sidebar/folderTree';
  import { isConsentRefusal, offerAiConsent } from '../aiConsent';
  import { refreshUnreviewed } from '../unreviewed';
  import { openCreatedNote } from '../openCreatedNote';
  import {
    OUTPUTS,
    OUTPUT_GROUPS,
    PLAN_DEBOUNCE_MS,
    studioRequest,
    progressLine,
    estimateLine,
    relatedReasons,
    type RelatedNote,
    type StudioRelated,
    type StudioOutput,
    type StudioPlan,
    type StudioProgress,
    type PlanNote,
  } from '../studio';

  type Request = { accountId: string; label: string };

  let output = $state<StudioOutput>('briefing');
  let focus = $state('');
  // null until the person changes a tick: the backend then applies its
  // default (everything but earlier Studio notes). Once set: the ticked folder
  // notes in plan order, then the ticked suggestions in the order shown.
  let selected = $state<string[] | null>(null);
  let plan = $state<StudioPlan | null>(null);
  let planError = $state('');
  let busy = $state(false);
  let errorMsg = $state('');
  let progressLog = $state<string[]>([]);
  // The in-flight run's id, for cancel_extraction. Empty while idle.
  let requestId = '';
  let disposed = false;

  // The request this opening belongs to. Every await checks it is still the
  // one, so a late answer for a closed or reopened dialog is dropped.
  let openedFor: Request | null = null;
  let planSeq = 0;
  let planTimer: ReturnType<typeof setTimeout> | undefined;
  let lastPlanKey = '';

  // Suggestions from other folders: asked once the first plan has answered
  // (so the folder list is never slowed), then again when Focus changes.
  let related = $state<StudioRelated | null>(null);
  // Ticked suggestions a newer answer no longer lists: still shown, so a
  // choice is never lost silently.
  let keptRelated = $state<RelatedNote[]>([]);
  let relatedPending = $state(false);
  let relatedStarted = false;
  let relatedSeq = 0;
  let relatedTimer: ReturnType<typeof setTimeout> | undefined;
  let lastRelatedKey = '';

  function resetForm() {
    clearTimeout(planTimer);
    planSeq++;
    output = 'briefing';
    focus = '';
    selected = null;
    plan = null;
    planError = '';
    errorMsg = '';
    progressLog = [];
    lastPlanKey = '';
    clearTimeout(relatedTimer);
    relatedSeq++;
    related = null;
    keptRelated = [];
    relatedPending = false;
    relatedStarted = false;
    lastRelatedKey = '';
  }

  function planKey(req: Request): string {
    return JSON.stringify([req.accountId, req.label, output, focus, selected]);
  }

  async function loadPlan(req: Request) {
    const seq = ++planSeq;
    lastPlanKey = planKey(req);
    const args = { accountId: req.accountId, label: req.label, output, focus, selected: selected ? [...selected] : null };
    try {
      const res = await invoke<StudioPlan>('studio_plan', args);
      if (disposed || seq !== planSeq || openedFor !== req) return;
      plan = res;
      planError = '';
      if (!relatedStarted) {
        relatedStarted = true;
        void loadRelated(req);
      }
    } catch (e) {
      if (disposed || seq !== planSeq || openedFor !== req) return;
      planError = String(e);
    }
  }

  function relatedKey(req: Request): string {
    return JSON.stringify([req.accountId, req.label, focus]);
  }

  async function loadRelated(req: Request) {
    const seq = ++relatedSeq;
    lastRelatedKey = relatedKey(req);
    relatedPending = true;
    try {
      const res = await invoke<StudioRelated>('studio_related', { accountId: req.accountId, label: req.label, focus });
      if (disposed || seq !== relatedSeq || openedFor !== req) return;
      // Rows and kept rows change together, in this one turn (gotcha #28).
      const listed = new Set(res.notes.map((n) => n.uuid));
      keptRelated = relatedRows.filter((n) => ticked.has(n.uuid) && !listed.has(n.uuid));
      related = res;
      relatedPending = false;
    } catch (e) {
      if (disposed || seq !== relatedSeq || openedFor !== req) return;
      // Suggestions are optional: without them the dialog works as stage 1.
      console.warn('studio_related failed', e);
      relatedPending = false;
    }
  }

  // A new request opens the dialog fresh and plans at once; null closes it.
  $effect(() => {
    const req = $studioRequest;
    untrack(() => {
      if (req === openedFor) return;
      void cancelInFlight();
      openedFor = req;
      resetForm();
      if (req) void loadPlan(req);
    });
  });

  // A change to the output, Focus or ticks re-plans, debounced; the latest
  // call wins through planSeq.
  $effect(() => {
    void [output, focus, selected];
    untrack(() => {
      const req = openedFor;
      if (!req || planKey(req) === lastPlanKey) return;
      clearTimeout(planTimer);
      planTimer = setTimeout(() => {
        if (openedFor === req) void loadPlan(req);
      }, PLAN_DEBOUNCE_MS);
    });
  });

  // A Focus change re-asks for suggestions, debounced like the plan. Ticks
  // and the output do not: they change nothing the finder reads.
  $effect(() => {
    void focus;
    untrack(() => {
      const req = openedFor;
      if (!req || !relatedStarted) return;
      // Back to what was last asked for: a timer armed by the edit in between
      // would only ask again for the same answer.
      clearTimeout(relatedTimer);
      if (relatedKey(req) === lastRelatedKey) return;
      relatedTimer = setTimeout(() => {
        if (openedFor === req) void loadRelated(req);
      }, PLAN_DEBOUNCE_MS);
    });
  });

  onDestroy(() => {
    disposed = true;
    clearTimeout(planTimer);
    clearTimeout(relatedTimer);
    void cancelInFlight();
  });

  const ticked = $derived(new Set(selected ?? plan?.selected ?? []));
  const tickedNotes = $derived((plan?.notes ?? []).filter((n) => ticked.has(n.uuid)));
  const allTicked = $derived(!!plan && plan.notes.length > 0 && tickedNotes.length === plan.notes.length);
  const relatedRows = $derived([...(related?.notes ?? []), ...keptRelated]);
  const tickedRelated = $derived(relatedRows.filter((n) => ticked.has(n.uuid)));

  // Grouped by subfolder, in the plan's order; the folder itself first when it has notes.
  const groups = $derived.by(() => {
    const req = $studioRequest;
    const out: { label: string; heading: string; notes: PlanNote[] }[] = [];
    for (const n of plan?.notes ?? []) {
      let g = out.find((x) => x.label === n.label);
      if (!g) {
        g = { label: n.label, heading: groupHeading(req, n.label), notes: [] };
        out.push(g);
      }
      g.notes.push(n);
    }
    const own = out.findIndex((g) => g.label === req?.label);
    if (own > 0) out.unshift(...out.splice(own, 1));
    return out;
  });

  function groupHeading(req: Request | null, label: string): string {
    if (!req || label === req.label || !label.startsWith(`${req.label}/`)) return viewName(null, label, '');
    return label.slice(req.label.length + 1).split('/').map(stripWorkflowMarkers).join(' / ');
  }

  // Folder notes in the plan's order, then suggestions in the order shown,
  // so what is sent reads like the list. The first tick turns the backend's
  // default (`selected === null`) into the list it stood for.
  function ordered(on: Set<string>): string[] {
    return [...(plan?.notes ?? []), ...relatedRows].map((n) => n.uuid).filter((u) => on.has(u));
  }

  function toggle(uuid: string, on: boolean) {
    const next = new Set(ticked);
    if (on) next.add(uuid);
    else next.delete(uuid);
    selected = ordered(next);
  }

  // Folder notes only: a suggestion is ticked one at a time, never in bulk.
  function toggleAll() {
    const keep = tickedRelated.map((n) => n.uuid);
    selected = allTicked ? keep : [...(plan?.notes ?? []).map((n) => n.uuid), ...keep];
  }

  const estimateText = $derived(planError || (plan ? estimateLine(plan) : ''));
  const canGenerate = $derived(
    !busy && !!plan && !planError && plan.notes.length > 0 && !plan.estimate.refusal && tickedNotes.length > 0,
  );

  function newRequestId(): string {
    return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
  }

  async function generate() {
    const req = openedFor;
    if (!req || !canGenerate) return;
    await run(req, ordered(ticked), false);
  }

  async function run(req: Request, uuids: string[], retried: boolean) {
    busy = true;
    errorMsg = '';
    progressLog = [];
    const id = newRequestId();
    requestId = id;
    const onProgress = new Channel<StudioProgress>();
    onProgress.onmessage = (p) => {
      if (requestId === id) progressLog = [...progressLog, progressLine(p)];
    };
    let created: ExtractedNote;
    try {
      created = await invoke<ExtractedNote>('generate_studio_note', {
        accountId: req.accountId,
        label: req.label,
        output,
        focus,
        uuids,
        requestId: id,
        onProgress,
      });
    } catch (e) {
      if (requestId === id) {
        busy = false;
        requestId = '';
        progressLog = [];
      }
      if (disposed || openedFor !== req) return;
      const msg = String(e);
      // Cancel wrote nothing and needs no message.
      if (msg === 'cancelled' || msg.endsWith(': cancelled')) return;
      if (!retried && isConsentRefusal(msg)) {
        // The prompt was shown and answered; on Allow the second run goes
        // through, otherwise it is refused again and that refusal shows.
        if (await offerAiConsent(msg, req.accountId)) {
          if (!disposed && openedFor === req) await run(req, uuids, true);
          return;
        }
      }
      errorMsg = msg;
      return;
    }
    busy = false;
    requestId = '';
    progressLog = [];
    void refreshUnreviewed([req.accountId]); // R15: an AI write makes a note unreviewed with no remote-changed behind it.
    closeIntoResult(created.label);
    // A Studio note gets no post-extract suggestions (spec §3): filing would
    // move it out of the research folder, autolink would add unchecked links.
    await openCreatedNote(req.accountId, created);
  }

  async function cancelInFlight() {
    if (!busy || !requestId) return;
    try {
      await invoke('cancel_extraction', { requestId });
    } catch (e) {
      console.warn('cancel_extraction invoke failed', e);
    }
  }

  // Every close path asks this first: while a run is in flight a close
  // attempt cancels it and keeps the dialog; the next attempt closes.
  function beforeClose(): boolean {
    if (!busy) return true;
    void cancelInFlight();
    return false;
  }

  let modal: { closeInto(pane: Pane, title?: string): boolean } | undefined = $state();

  function onModalClose() {
    studioRequest.set(null);
  }

  // Closes INTO the new note's destination (as Extract does): the list of
  // the folder it was filed in, or the note itself when that folder is shown.
  function closeIntoResult(label: string) {
    const pane: Pane = get(selectedFolder) === label ? 'note' : 'list';
    if (!modal?.closeInto(pane, pane === 'list' ? viewName(null, label, '') : undefined)) studioRequest.set(null);
  }
</script>

{#if $studioRequest}
  <Modal
    bind:this={modal}
    title={`Studio — ${viewName(null, $studioRequest.label, '')}`}
    size="lg"
    onbeforeclose={beforeClose}
    onclose={onModalClose}
    primary={{ label: busy ? 'Generating…' : 'Generate', onclick: () => void generate(), disabled: !canGenerate, busy }}
    footer={busy ? cancelRun : undefined}
  >
    <fieldset class="studio-outputs" disabled={busy}>
      <legend>Output</legend>
      {#each OUTPUT_GROUPS as grp (grp.value)}
        <div class="studio-output-group" role="group" aria-labelledby="studio-group-{grp.value}">
          <div class="studio-output-group-label" id="studio-group-{grp.value}">{grp.label}</div>
          {#each OUTPUTS.filter((o) => o.group === grp.value) as opt (opt.value)}
            <label class="studio-output">
              <input type="radio" name="studio-output" value={opt.value} bind:group={output} />
              <span class="studio-output-text">
                <span class="studio-output-label">{opt.label}</span>
                <span class="studio-output-desc">{opt.description}</span>
              </span>
            </label>
          {/each}
        </div>
      {/each}
    </fieldset>

    <label class="studio-focus-label">
      Focus
      <input class="studio-focus" type="text" maxlength="200" bind:value={focus} disabled={busy} placeholder="Optional — what to focus on" />
    </label>

    {#if plan && plan.notes.length > 0}
      <section class="studio-notes" aria-label="Notes">
        <div class="studio-notes-head">
          <span class="studio-count">{tickedNotes.length} of {plan.notes.length} notes</span>
          <button type="button" class="studio-toggle-all" disabled={busy} onclick={toggleAll}>
            {allTicked ? 'Select none' : 'Select all'}
          </button>
        </div>
        <div class="studio-note-list">
          {#each groups as g (g.label)}
            <h3 class="studio-group">{g.heading}</h3>
            <ul>
              {#each g.notes as n (n.uuid)}
                <li>
                  <label class="studio-note">
                    <input
                      type="checkbox"
                      value={n.uuid}
                      checked={ticked.has(n.uuid)}
                      disabled={busy}
                      onchange={(e) => toggle(n.uuid, (e.currentTarget as HTMLInputElement).checked)}
                    />
                    <span class="studio-note-title">{displayTitle(n.title) || 'Untitled'}</span>
                    {#if n.is_studio}<span class="studio-chip">Studio</span>{/if}
                  </label>
                </li>
              {/each}
            </ul>
          {/each}
        </div>
        {#if plan.not_shown > 0}
          <p class="studio-not-shown">{plan.not_shown} older notes not shown.</p>
        {/if}
      </section>
    {/if}

    {#if relatedRows.length > 0 || relatedPending}
      <section class="studio-notes" aria-label="From other folders">
        <h3 class="studio-group">From other folders{relatedRows.length > 0 ? ` (${relatedRows.length})` : ''}</h3>
        {#if relatedRows.length > 0}
          <div class="studio-note-list">
            <ul>
              {#each relatedRows as n (n.uuid)}
                <li>
                  <label class="studio-note">
                    <input
                      type="checkbox"
                      value={n.uuid}
                      checked={ticked.has(n.uuid)}
                      disabled={busy}
                      onchange={(e) => toggle(n.uuid, (e.currentTarget as HTMLInputElement).checked)}
                    />
                    <span class="studio-related-text">
                      <span class="studio-note-title">{displayTitle(n.title) || 'Untitled'}</span>
                      <span class="studio-related-why">
                        <span class="studio-related-folder">{viewName(null, n.label, '')}</span>
                        {#each relatedReasons(n) as why (why)}<span class="studio-chip">{why}</span>{/each}
                      </span>
                    </span>
                  </label>
                </li>
              {/each}
            </ul>
          </div>
        {:else}
          <p class="studio-not-shown">Looking for related notes…</p>
        {/if}
        {#if related?.capped}
          <p class="studio-not-shown">Showing the 20 closest.</p>
        {/if}
      </section>
    {/if}

    {#if estimateText}
      <p class="studio-estimate" class:refused={!!planError || !!plan?.estimate.refusal || plan?.notes.length === 0}>{estimateText}</p>
    {/if}

    {#if progressLog.length > 0}
      <ol class="ingest-progress" aria-live="polite">
        {#each progressLog as line, i (i)}
          <li>{line}</li>
        {/each}
      </ol>
    {/if}

    {#if errorMsg}
      <div class="error">{errorMsg}</div>
    {/if}
  </Modal>
{/if}

<!-- While a run is in flight: cancel_extraction(requestId) stops it; nothing
     is written. Idle, the Modal's own close control closes. -->
{#snippet cancelRun()}
  <button type="button" class="cancel-run" onclick={() => void cancelInFlight()}>Cancel</button>
{/snippet}

<style>
  .studio-outputs { border: none; margin: 0 0 12px; padding: 0; min-width: 0; }
  .studio-outputs legend { padding: 0; margin-bottom: 6px; font-size: var(--size); color: var(--text-secondary); }
  .studio-output-group + .studio-output-group { margin-top: 10px; }
  .studio-output-group-label { font-size: var(--size-sm); color: var(--text-muted); text-transform: uppercase; letter-spacing: 0.04em; margin-bottom: 2px; }
  .studio-output { display: flex; align-items: flex-start; gap: 8px; margin: 6px 0; cursor: pointer; }
  .studio-output input, .studio-note input { flex: none; margin: 3px 0 0; padding: 0; }
  .studio-output-text { display: flex; flex-direction: column; min-width: 0; }
  .studio-output-label { font-size: var(--size); color: var(--text); font-weight: 600; }
  .studio-output-desc { font-size: var(--size-sm); color: var(--text-muted); overflow-wrap: anywhere; }
  .studio-focus-label { display: block; margin: 12px 0; font-size: var(--size); color: var(--text-secondary); }
  .studio-focus {
    display: block; width: 100%; padding: 8px; font: inherit; box-sizing: border-box; margin-top: 4px;
    /* Stated, never inherited: an unset background is the UA's white in dark mode too. */
    background: var(--surface-panel);
    color: var(--text);
  }
  .studio-notes { margin: 12px 0; min-width: 0; }
  .studio-notes-head { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .studio-count { font-size: var(--size); color: var(--text-secondary); }
  .studio-toggle-all {
    background: none; border: none; padding: 0; color: var(--accent-action); cursor: pointer;
    font: inherit; font-size: var(--size-sm);
  }
  .studio-note-list {
    max-height: 40vh; overflow-y: auto; overflow-x: hidden;
    border: 1px solid var(--border); border-radius: 4px; padding: 6px 8px;
  }
  .studio-group { margin: 8px 0 2px; font-size: var(--size-sm); font-weight: 600; color: var(--text-muted); overflow-wrap: anywhere; }
  .studio-group:first-child { margin-top: 0; }
  .studio-note-list ul { list-style: none; margin: 0; padding: 0; }
  .studio-note { display: flex; align-items: flex-start; gap: 8px; margin: 4px 0; font-size: var(--size-sm); color: var(--text); cursor: pointer; min-width: 0; }
  .studio-note-title { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .studio-chip {
    flex: none; font-size: var(--size-xs); color: var(--text-muted);
    border: 1px solid var(--border); border-radius: 999px; padding: 0 6px;
  }
  .studio-related-text { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
  .studio-related-why { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 6px; min-width: 0; }
  .studio-related-folder { font-size: var(--size-xs); color: var(--text-muted); overflow-wrap: anywhere; }
  .studio-related-why .studio-chip { overflow-wrap: anywhere; }
  .studio-not-shown { margin: 6px 0 0; font-size: var(--size-sm); color: var(--text-muted); }
  .studio-estimate { margin: 12px 0; font-size: var(--size); color: var(--text-secondary); }
  .studio-estimate.refused { color: var(--danger); }
  .error {
    color: var(--danger); padding: 8px; background: var(--danger-wash); border-radius: 4px;
    margin: 12px 0; white-space: pre-wrap;
  }
  .cancel-run {
    font: inherit; font-size: var(--size-sm); color: var(--text); cursor: pointer;
    padding: var(--space-1) var(--space-3); border-radius: var(--radius);
    border: 1px solid var(--border); background: var(--surface-panel);
  }
  .cancel-run:hover { background: var(--surface-hover); }
  .ingest-progress { margin: 8px 0 0; padding-left: 20px; font-size: var(--size-sm); color: var(--text-secondary); }
</style>
