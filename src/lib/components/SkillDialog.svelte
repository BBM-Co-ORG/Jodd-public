<script lang="ts">
  // Publish a folder as a Claude Code skill (spec 2026-11-05 §6.1), opened
  // from the folder menu through `skillRequest`. Desktop only. Saving records
  // the folder, then runs one sync so the result shows here at once.
  import { invoke } from '@tauri-apps/api/core';
  import Modal from './Modal.svelte';
  import { pickFolder } from '../backup/pickFolder';
  import type { SyncOutcome } from '../agentFiles';
  import {
    skillRequest, skillForFolder, skillStateLine, suggestSkillName, validSkillName,
    SKILL_PREFIX, type SkillRow, type SkillsView,
  } from '../agentSkills';

  let name = $state('');
  let assetsDir = $state('');
  let onlyReviewed = $state(false);
  let existing = $state<SkillRow | null>(null);
  let busy = $state(false);
  let error = $state('');
  let result = $state<SkillRow | null>(null);
  let loadedFor = '';

  // Load once per request: the folder's current entry, else a suggestion.
  $effect(() => {
    const req = $skillRequest;
    if (!req) { loadedFor = ''; return; }
    const key = `${req.accountId}\n${req.folder}`;
    if (key === loadedFor) return;
    loadedFor = key;
    existing = null; result = null; error = '';
    name = suggestSkillName(req.folder);
    assetsDir = ''; onlyReviewed = false;
    invoke<SkillsView>('agent_skills_view')
      .then(view => {
        if (loadedFor !== key) return;
        const row = skillForFolder(view, req.accountId, req.folder);
        if (row) {
          existing = row;
          name = row.name;
          assetsDir = row.assets_dir ?? '';
          onlyReviewed = row.only_reviewed;
        }
      })
      .catch(() => {});
  });

  let leaf = $derived(($skillRequest?.folder ?? '').split('/').pop() ?? '');
  let nameOk = $derived(validSkillName(name));

  async function chooseAssets() {
    const picked = await pickFolder('Choose the folder of brand files (fonts, logos, templates)', () => !$skillRequest);
    if (picked) assetsDir = picked;
  }

  async function publish() {
    const req = $skillRequest;
    if (!req || busy || !nameOk) return;
    busy = true; error = ''; result = null;
    try {
      await invoke('set_agent_skill', {
        name, accountId: req.accountId, folder: req.folder,
        assetsDir: assetsDir.trim() || null, onlyReviewed,
      });
      await invoke<SyncOutcome>('sync_agent_files_now', { allowLargeDelete: false });
      const view = await invoke<SkillsView>('agent_skills_view');
      if ($skillRequest !== req) return;
      result = skillForFolder(view, req.accountId, req.folder);
      existing = result;
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function stop() {
    const req = $skillRequest;
    if (!req || !existing || busy) return;
    busy = true; error = '';
    try {
      await invoke('remove_agent_skill', { name: existing.name });
      skillRequest.set(null);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if $skillRequest}
  <Modal
    title="Publish as Claude Code skill"
    size="md"
    onclose={() => skillRequest.set(null)}
    primary={result
      ? { label: 'Done', onclick: () => skillRequest.set(null) }
      : { label: busy ? 'Publishing…' : existing ? 'Save' : 'Publish', onclick: () => void publish(), disabled: !nameOk || busy, busy }}
  >
    <div class="skill-dialog">
      <p class="help">
        Claude Code will follow the notes in <strong>{$skillRequest.folder}</strong> when a task matches.
        The note titled “{leaf}” becomes the skill's main page: its first line under the title should say
        when to use it, for example “Use when making slides, images or video for BBMedia.”
      </p>

      <label class="field">
        <span class="label">Skill name</span>
        <div class="name"><span class="prefix">{SKILL_PREFIX}</span><input type="text" bind:value={name} disabled={busy} aria-label="Skill name" /></div>
        {#if !nameOk}<span class="hint">Lowercase letters, digits and single hyphens.</span>{/if}
      </label>

      <div class="field">
        <span class="label">Brand files on disk (optional)</span>
        <div class="path">
          <input type="text" bind:value={assetsDir} disabled={busy} placeholder="Fonts, logos, .pptx templates" aria-label="Brand files folder" />
          <button onclick={chooseAssets} disabled={busy}>Choose…</button>
        </div>
      </div>

      <label class="row">
        <input type="checkbox" bind:checked={onlyReviewed} disabled={busy} />
        Only publish notes I marked reviewed (edits from my iPhone wait too)
      </label>

      <details class="learn-more">
        <summary>What makes a good brand guide for an agent</summary>
        <ul>
          <li>Exact values: colours as hex, font names, sizes, aspect ratios.</li>
          <li>Logo rules that name the files in the skill's <code>assets/</code>.</li>
          <li>Image style written as a prompt, with what to avoid.</li>
          <li>Do and don't examples, and a checklist to run before handing work over.</li>
          <li>Never ask an image or video model to draw the logo; place the file from <code>assets/</code> on top.</li>
        </ul>
        <p class="help">Notes an agent or Jodd's AI changed wait for Mark reviewed before they reach the skill.</p>
      </details>

      {#if result}
        <p class="status" role="status">{skillStateLine(result)}</p>
        {#each result.status?.problems ?? [] as p, i (i)}<p class="warn">{p}</p>{/each}
      {:else if existing}
        <p class="help">{skillStateLine(existing)}</p>
      {/if}
      {#if error}<p class="error" role="alert">{error}</p>{/if}

      {#if existing && !result}
        <button class="danger" onclick={stop} disabled={busy}>Stop publishing</button>
      {/if}
    </div>
  </Modal>
{/if}

<style>
  .skill-dialog { display: flex; flex-direction: column; gap: 10px; }
  .help, .hint { font-size: var(--size-sm); color: var(--text-muted); line-height: var(--leading); }
  .field { display: flex; flex-direction: column; gap: 4px; }
  .label { font-size: var(--size-sm); font-weight: var(--weight-semibold); }
  .name, .path, .row { display: flex; align-items: center; gap: 6px; }
  .prefix { font-family: var(--font-mono); color: var(--text-muted); }
  .name input, .path input { flex: 1; min-width: 0; padding: 6px; font: inherit; background: var(--surface-panel); color: var(--text); }
  .error, .danger { color: var(--danger); }
  .warn { font-size: var(--size-sm); color: var(--warning, var(--danger)); }
  .status { font-weight: var(--weight-semibold); }
  ul { margin: 4px 0; padding-left: 20px; font-size: var(--size-sm); line-height: var(--leading); }
</style>
