<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onDestroy, onMount } from 'svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import type { SttConfig, SttConfigStatus, SttPreset } from '../types';

  // App-level speech-to-text endpoint for video links (spec D3, D5, §5.4).
  // No Save button of its own: Settings › Speech to text's one Save calls
  // save() (lib/settings/sectionForm.ts), which still asks for the host.
  // The backend decides where audio may go: `confirmed_host` (an origin,
  // scheme://host[:port]) is written only when `set_stt_config` receives the
  // very origin its base URL has, and a base URL whose origin differs from it
  // is "not set up" until confirmed again — this form only asks, and sends
  // back exactly what it showed.

  const UNTESTED = 'not yet tested with Jodd';
  const PRESETS: Record<Exclude<SttPreset, 'custom'>, { label: string; base_url: string; model: string }> = {
    openai: { label: 'OpenAI', base_url: 'https://api.openai.com/v1', model: 'whisper-1' },
    groq: { label: 'Groq', base_url: 'https://api.groq.com/openai/v1', model: 'whisper-large-v3' },
  };

  let preset = $state<SttPreset>('custom');
  let baseUrl = $state('');
  let model = $state('');
  let language = $state('');
  let apiKey = $state('');
  let showKey = $state(false);
  // As last loaded or saved. Test runs THESE (the backend reads the saved
  // file), and `confirmed_host` here is what the backend will honour.
  let saved = $state<SttConfig | null>(null);
  let hasStoredKey = $state<boolean | null>(null);
  let confirmDeleteKey = $state(false);
  // What the host-confirm dialog showed: the origin to send back, the host[:port]
  // and scheme for the copy, and the config as it was when Save was pressed.
  let pending = $state<{ origin: string; host: string; secure: boolean; cfg: SttConfig } | null>(null);
  let saving = $state(false);
  let testing = $state(false);
  let testResult = $state<{ ok: boolean; text: string } | null>(null);
  let msg = $state('');

  onMount(load);

  async function load() {
    try {
      const res = await invoke<SttConfigStatus>('get_stt_config');
      applySaved(res.cfg);
      hasStoredKey = res.has_api_key ?? null;
    } catch (e) {
      msg = `load: ${e}`;
    }
  }

  function applySaved(cfg: SttConfig) {
    saved = cfg;
    preset = cfg.preset;
    baseUrl = cfg.base_url;
    model = cfg.model;
    language = cfg.language ?? '';
    apiKey = '';
  }

  /** `scheme://host[:port]` (default ports omitted, lowercase), `host[:port]`, and whether it is https; null for anything but http(s). */
  function parseOrigin(url: string): { origin: string; host: string; secure: boolean } | null {
    try {
      const u = new URL(url.trim());
      if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
      const host = u.host.toLowerCase();
      return { origin: `${u.protocol}//${host}`, host, secure: u.protocol === 'https:' };
    } catch {
      return null;
    }
  }

  function choosePreset(next: SttPreset) {
    preset = next;
    if (next !== 'custom') {
      baseUrl = PRESETS[next].base_url;
      model = PRESETS[next].model;
    }
  }

  function formConfig(): SttConfig {
    return {
      preset,
      base_url: baseUrl.trim(),
      model: model.trim(),
      language: language.trim() || null,
      max_upload_mb: saved?.max_upload_mb ?? 25,
      // Ignored by the backend; carried only so the shape round-trips.
      confirmed_host: saved?.confirmed_host ?? null,
    };
  }

  const dirty = $derived(
    saved !== null &&
      (preset !== saved.preset ||
        baseUrl.trim() !== saved.base_url ||
        model.trim() !== saved.model ||
        (language.trim() || null) !== (saved.language ?? null) ||
        apiKey !== ''),
  );

  const formOrigin = $derived(parseOrigin(baseUrl));
  const savedOrigin = $derived(saved ? parseOrigin(saved.base_url) : null);
  // Mirrors the backend's gate. The status line and Test follow the SAVED
  // settings, never the edits on screen.
  const confirmed = $derived(
    saved && savedOrigin && saved.confirmed_host?.toLowerCase() === savedOrigin.origin ? savedOrigin : null,
  );

  // Settles the host-confirm step save() is waiting on: true = Confirm.
  let decide: ((accepted: boolean) => void) | null = null;
  // The save in progress (confirm open or write running). A re-entrant save()
  // returns this same promise, so a double page-level Save cannot open two
  // questions or race two writes.
  let inFlight: Promise<void> | null = null;
  let askSeq = 0;
  onDestroy(() => decide?.(false));

  /**
   * Section-form contract (lib/settings/sectionForm.ts). An unchanged origin
   * that is already confirmed saves straight away; anything else asks first
   * and resolves only once the user accepts. Throws the message the section
   * shows when the write fails, and 'Speech-to-text host not confirmed.' if
   * the user cancels. Calling it again while one is in flight returns the
   * same promise.
   */
  export function save(): Promise<void> {
    if (inFlight) return inFlight;
    const run = doSave().finally(() => { if (inFlight === run) inFlight = null; });
    inFlight = run;
    return run;
  }

  async function doSave(): Promise<void> {
    msg = '';
    testResult = null;
    const target = formOrigin;
    if (baseUrl.trim() === '') {
      await commit(formConfig(), null);
    } else if (!target) {
      throw new Error('The base URL must start with http:// or https://');
    } else if (target.origin === confirmed?.origin) {
      await commit(formConfig(), null);
    } else {
      const asked = { ...target, cfg: formConfig() };
      const askId = ++askSeq; // `pending` is a state proxy, so identity can't mark ownership
      pending = asked;
      let mine: (accepted: boolean) => void = () => {};
      const accepted = await new Promise<boolean>((resolve) => { mine = resolve; decide = resolve; });
      // Only clear what still belongs to this call.
      if (decide === mine) decide = null;
      if (!accepted) {
        if (askSeq === askId) pending = null;
        throw new Error('Speech-to-text host not confirmed.');
      }
      await commit(asked.cfg, asked.origin);
    }
  }

  export function isDirty(): boolean {
    return dirty;
  }

  // A failed save is shown once, by the Settings page above this section
  // (settings/SaveError.svelte), so it is thrown and not also put in `msg`.
  async function commit(cfg: SttConfig, confirm: string | null) {
    // A key delete (removeKey) can be writing; two writes must not interleave.
    if (saving) throw new Error('Another save is in progress.');
    await persist(cfg, confirm);
  }

  /** `confirm` is the origin the dialog showed; the backend refuses it if the saved address is no longer that one. */
  async function persist(cfg: SttConfig, confirm: string | null): Promise<void> {
    pending = null;
    saving = true;
    try {
      await invoke('set_stt_config', { cfg, apiKey: apiKey ? apiKey : null, confirm });
      await load();
      showKey = false;
      msg = 'Saved.';
    } catch (e) {
      throw new Error(`save: ${e}`);
    } finally {
      saving = false;
    }
  }

  async function removeKey() {
    confirmDeleteKey = false;
    if (!saved) return;
    saving = true;
    msg = '';
    try {
      // The SAVED config, so unsaved edits are not written behind the user's
      // back; its host is unchanged, so the confirmation survives.
      await invoke('set_stt_config', { cfg: saved, apiKey: '', confirm: null });
      hasStoredKey = false;
      apiKey = '';
      showKey = false;
      testResult = null;
      msg = 'Stored key deleted. Other unsaved settings were not saved.';
    } catch (e) {
      msg = `Could not delete API key: ${e}`;
    } finally {
      saving = false;
    }
  }

  async function runTest() {
    testing = true;
    testResult = null;
    try {
      testResult = { ok: true, text: await invoke<string>('test_stt') };
    } catch (e) {
      testResult = { ok: false, text: String(e) };
    } finally {
      testing = false;
    }
  }
</script>

<!-- The host question is a dialog, not an inline panel: save() can be
     called from the Settings page's discard guard, which is itself a modal
     alert and makes everything beneath it inert. A dialog opened later stacks
     above the guard, so Confirm stays reachable on every save path. -->
{#if pending}
  <ConfirmDialog
    title="Confirm where audio is sent"
    message={`Audio from video links will be sent to ${pending.host}${pending.secure ? '' : ' (not encrypted)'} to turn speech into text.`}
    confirmLabel="Confirm"
    onConfirm={() => decide?.(true)}
    onCancel={() => { if (decide) decide(false); else pending = null; }}
  />
{/if}

{#if confirmDeleteKey}
  <ConfirmDialog title="Delete API key?" message="Delete the saved speech-to-text API key from the credential store? Other unsaved settings will be kept in this form." confirmLabel="Delete API key" destructive onConfirm={removeKey} onCancel={() => { confirmDeleteKey = false; }} />
{/if}

<div class="stt-settings">
  {#if confirmed}
    <p class="help" role="status">Audio from video links is sent to <strong>{confirmed.host}</strong>{confirmed.secure ? '' : ' (not encrypted)'}.</p>
  {:else}
    <p class="help" role="status">Not set up. No audio leaves this device; video links use the platform's own transcript or caption.</p>
  {/if}

  <label>
    Service
    <select value={preset} disabled={!!pending} onchange={(e) => choosePreset(e.currentTarget.value as SttPreset)}>
      <option value="openai">{PRESETS.openai.label} — {UNTESTED}</option>
      <option value="groq">{PRESETS.groq.label} — {UNTESTED}</option>
      <option value="custom">My server</option>
    </select>
  </label>
  {#if preset !== 'custom'}
    <p class="help warn untested">{PRESETS[preset].label} is {UNTESTED}.</p>
  {/if}

  <label>
    Base URL
    <input type="text" bind:value={baseUrl} placeholder="https://api.openai.com/v1" autocomplete="off" disabled={!!pending} />
  </label>
  <label>
    Model
    <input type="text" bind:value={model} placeholder="whisper-1" autocomplete="off" disabled={!!pending} />
  </label>
  <label>
    Language (optional)
    <input type="text" bind:value={language} placeholder="Leave blank for Thai and English mixed" autocomplete="off" disabled={!!pending} />
  </label>

  <p role="status">API key: {hasStoredKey === null ? 'Not checked yet' : hasStoredKey ? 'Previously saved' : 'Last known to be absent'}.</p>
  <label>
    New API key
    <input class="api-key-input" type={showKey ? 'text' : 'password'} bind:value={apiKey} autocomplete="new-password" placeholder="Leave blank to keep any existing key" disabled={saving || testing || !!pending} />
  </label>
  <button onclick={() => { showKey = !showKey; }} aria-pressed={showKey} disabled={!!pending}>
    {showKey ? 'Hide new key' : 'Show new key'}
  </button>
  {#if hasStoredKey !== false}
    <button class="link-btn" onclick={() => { confirmDeleteKey = true; }} disabled={saving || testing || !!pending}>Delete API key</button>
  {/if}
  <p class="help">Saving a new key or deleting one may ask for permission to access your credential store on macOS.</p>

  <div class="btn-row">
    <button
      onclick={runTest}
      disabled={testing || saving || dirty || !confirmed || !!pending}
      title={dirty ? 'Save first — Test runs the saved settings' : undefined}
    >
      {testing ? 'Testing…' : 'Test'}
    </button>
  </div>
  {#if dirty}
    <p class="help warn">Unsaved changes — press Save to test them.</p>
  {:else if !confirmed}
    <p class="help">Save and confirm the host to enable Test.</p>
  {:else}
    <p class="help">Test sends a 3-second Thai and English clip bundled with Jodd to {confirmed.host}{confirmed.secure ? '' : ' (not encrypted)'}.</p>
  {/if}

  {#if testResult}
    <div class="test-result" class:bad={!testResult.ok}>
      <strong>{testResult.ok ? 'OK' : 'Failed'}</strong>
      {#if testResult.ok}<p class="transcript">{testResult.text}</p>{:else}<p class="err">{testResult.text}</p>{/if}
    </div>
  {/if}

  {#if msg}<p class="msg">{msg}</p>{/if}
</div>

<style>
  .stt-settings { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: var(--size); }
  select { width: 100%; padding: 6px; font: inherit; background: var(--surface-panel); color: var(--text); }
  .api-key-input, input[type="text"], input[type="password"] {
    width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px;
    background: var(--surface-panel); color: var(--text);
  }
  button { padding: 8px 16px; font: inherit; }
  .btn-row { display: flex; gap: 8px; margin-top: 12px; }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 4px 0 0; }
  .help.warn { background: var(--warn-wash); border-left: 3px solid var(--accent); padding: 6px 8px; border-radius: 3px; line-height: var(--leading); }
  .test-result { margin-top: 8px; padding: 8px; border-radius: 4px; background: var(--success-wash); font-size: var(--size-sm); }
  .test-result.bad { background: var(--danger-wash); }
  .transcript { margin: 4px 0 0; }
  .err { color: var(--danger); margin: 4px 0 0; }
  .link-btn {
    background: none; border: none; padding: 0; margin-top: 6px;
    color: var(--accent-action); font: inherit; font-size: var(--size-sm);
    text-decoration: underline; cursor: pointer;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .msg { font-size: var(--size-sm); color: var(--text-muted); }
</style>
