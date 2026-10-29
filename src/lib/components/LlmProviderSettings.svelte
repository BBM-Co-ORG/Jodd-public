<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { formFingerprint, hasUnsavedEdits } from './llmFormDirty';
  import { isAndroid } from '../stores/platform';
  import { isConsentRefusal, DATA_ALLOWED_LABEL } from '../aiConsent';
  import { cliPrivacyNotes, hostLabel, splitNote } from '../cliPrivacy';
  import AiHostPanel from './AiHostPanel.svelte';
  import { inheritedProvider } from '../llmInherit';

  // One form, two scopes. `account` edits accounts.json via the per-account
  // commands; `app` edits app_llm.json via the app-level ones. Everything
  // between — the preset dropdown, the PATH-detection hints, the probe — is
  // identical, and the app-level provider is the one behind Ask Jodd, so it
  // needs the hints and the probe at least as much as an account does.
  // showHeading: an account's Settings page drops this component in bare and
  // relies on its <h3>. Settings › AI renders it inside a <section> that
  // already carries its own heading and description, matching its sibling
  // sections — so without this the panel showed "LLM provider" immediately
  // above "LLM Provider".
  //
  // No Save button of its own: the Settings page's one Save calls save()
  // through the section-form contract (lib/settings/sectionForm.ts).
  let {
    scope = 'account',
    accountId,
    showHeading = true,
  }: { scope?: 'account' | 'app'; accountId?: string; showHeading?: boolean } = $props();

  // $derived, not a plain const: props are reactive state in Svelte 5, so a
  // const would freeze the value this component mounted with.
  const isApp = $derived(scope === 'app');

  // The keychain entry the HTTP key lands in. Mirrors Rust: per-account keys
  // are `llm_api_key::{account_id}`, and the app's `__app__` sentinel can
  // never collide because an account id is an email or `localfs:<uuid>`.
  const keychainKey = $derived(
    isApp ? 'llm_api_key::__app__' : `llm_api_key::${accountId}`,
  );

  type AgentCliSpec = {
    binary: string;
    args: string[];
    prompt_delivery: 'stdin_all' | 'stdin_payload_system_arg' | 'argv';
    output: 'stdout' | 'last_message_file';
    unwrap: string | null;
    fidelity: 'structured' | 'heuristic';
    timeout_secs: number;
  };

  type PresetInfo = {
    id: string;
    label: string;
    fidelity: 'structured' | 'heuristic';
    installed: boolean;
    resolved_path: string | null;
    available: boolean;
    unavailable_reason: string | null;
    file_access: 'disabled' | 'enabled' | 'unknown';
    argv_prompt: boolean;
  };

  type AgentHost = { target: string; port: number | null; key_dir: string };

  type LlmConfig = {
    data_allowed?: boolean | null;
    provider: 'none' | 'disabled' | 'claude_code' | 'http' | 'agent_cli';
    http_base_url: string | null;
    http_model: string | null;
    http_api_key_keychain: string | null;
    agent_preset: string | null;
    agent_custom: AgentCliSpec | null;
    // Set when the agent CLI runs on the user's server over SSH; older
    // configs omit it.
    agent_host?: AgentHost | null;
    disable_thinking: boolean;
  };

  let cfg: LlmConfig = $state({
    provider: 'none',
    http_base_url: '',
    http_model: '',
    http_api_key_keychain: null,
    agent_preset: null,
    agent_custom: null,
    agent_host: null,
    disable_thinking: false,
  });
  // Where the agent CLI runs. Android has no child processes, so it can only
  // be a server there.
  let runsOn = $state<'local' | 'server'>('local');
  // What the server reported installed; gates the preset list in server mode.
  let remotePresets: { id: string; path: string | null }[] = $state([]);
  const REMOTE_CAPABLE = ['claude', 'codex'];
  $effect(() => {
    if ($isAndroid) runsOn = 'server';
  });
  // A preset the server cannot run (Custom, or anything but claude/codex) is
  // refused by the backend, so do not leave it selected.
  $effect(() => {
    if (runsOn === 'server' && cfg.agent_preset && !REMOTE_CAPABLE.includes(cfg.agent_preset)) {
      cfg.agent_preset = null;
    }
  });
  // The server panel reports the host and what is installed on it. With no
  // CLI chosen yet — always the case on a first Android setup, which has no
  // local preset to carry over — pick one the server can run (claude first),
  // or Test stays disabled with nothing to say why. Never replaces a choice.
  function hostChanged(h: AgentHost | null, p: { id: string; path: string | null }[]) {
    cfg.agent_host = h;
    remotePresets = p;
    if (!cfg.agent_preset) {
      cfg.agent_preset = REMOTE_CAPABLE.find((id) => p.some((r) => r.id === id && r.path != null)) ?? null;
    }
  }
  type AppLlmConfig = { llm: LlmConfig; apply_to_accounts: boolean };

  let presets: PresetInfo[] = $state([]);
  let customArgsText = $state('');
  let apiKey = $state('');
  // App scope only: last-known metadata, never a credential-store read on open.
  // null covers older installs and missing metadata; errors preserve this state.
  let hasStoredKey = $state<boolean | null>(null);
  let editingKey = $state(false);
  let showKey = $state(false);
  let checkingKey = $state(false);
  let confirmDeleteKey = $state(false);
  let applyToAccounts = $state(false);
  // Account scope only: the app config, so "Use app default" can say what it
  // resolves to. Null until read, and stays null if the read fails.
  let appCfg = $state<AppLlmConfig | null>(null);
  const inherited = $derived(isApp ? null : inheritedProvider(appCfg, presets));
  type TestResult = {
    ok: boolean;
    elapsed_ms: number;
    error: string | null;
    raw_head: string;
    cause: string | null;
    action: string | null;
  };
  let testing = $state(false);
  let testResult: TestResult | null = $state(null);
  let saving = $state(false);
  let msg = $state('');

  onMount(load);

  async function load() {
    try {
      presets = await invoke<PresetInfo[]>('list_agent_cli_presets');
      if (isApp) {
        const res = await invoke<{ cfg: AppLlmConfig; has_api_key: boolean | null }>(
          'get_app_llm_config',
        );
        cfg = res.cfg.llm;
        applyToAccounts = res.cfg.apply_to_accounts;
        hasStoredKey = res.has_api_key ?? null;
      } else {
        cfg = await invoke<LlmConfig>('get_llm_settings', { accountId });
      }
      // Accounts saved before v0.19 say "claude_code"; show them as the
      // claude preset without rewriting anything until the user saves.
      cfg.data_allowed = cfg.provider === 'disabled' ? false : (cfg.data_allowed ?? false);
      if (cfg.provider === 'claude_code') {
        cfg.provider = 'agent_cli';
        cfg.agent_preset = 'claude';
      }
      // `disabled` is an account-scoped concept (opt out of the app default).
      // There is nothing above the app to opt out of, so show it as unset.
      if (isApp && cfg.provider === 'disabled') cfg.provider = 'none';
      apiKey = '';
      cfg.agent_host = cfg.agent_host ?? null;
      runsOn = $isAndroid || cfg.agent_host ? 'server' : 'local';
      customArgsText = (cfg.agent_custom?.args ?? []).join('\n');
      // Baseline for `dirty`. Taken AFTER the legacy-provider rewrites above,
      // so showing a "claude_code" account as the claude preset does not read
      // as an edit the user made.
      rebaseline(llmPayload(), applyToAccounts);
    } catch (e) {
      msg = `load: ${e}`;
    }
    if (!isApp) {
      // Separate from the form's own load: failing to read the app config
      // must not stop this account's settings from loading. Metadata only —
      // `get_app_llm_config` never reads the credential store (gotcha #15).
      try {
        appCfg = (await invoke<{ cfg: AppLlmConfig }>('get_app_llm_config')).cfg;
      } catch (e) {
        console.warn('get_app_llm_config failed; not describing the app default', e);
      }
    }
  }

  // A Custom spec defaults to `heuristic`: Jodd cannot know whether an
  // unknown CLI has a reliable JSON mode, and heuristic only ever costs one
  // extra retry, while wrongly claiming structured would suppress a retry
  // that could have succeeded.
  $effect(() => {
    if (cfg.agent_preset === 'custom' && !cfg.agent_custom) {
      cfg.agent_custom = {
        binary: '',
        args: [],
        prompt_delivery: 'stdin_all',
        output: 'stdout',
        unwrap: null,
        fidelity: 'heuristic',
        timeout_secs: 120,
      };
    }
  });

  // The probe goes through the same resolver the real workflows use, so it is
  // meaningful for every provider kind — not just agent CLIs. An HTTP base URL
  // with a typo in it fails exactly as informatively as a misspelled binary.
  // An ACCOUNT set to "use app default" is testable too, and usefully so: the
  // probe runs the §4.2 cascade, so it answers "what will Extract actually use
  // here?" — either the inherited app provider or a clear NotConfigured. The
  // app itself has nothing above it to inherit, so there `none` is untestable.
  const canTest = $derived(
    cfg.provider === 'http' ||
      (cfg.provider === 'agent_cli' && !!cfg.agent_preset) ||
      (!isApp && cfg.provider === 'none'),
  );

  // What Save last wrote; null until the initial load lands.
  let savedFingerprint: string | null = $state(null);
  // What is on disk, as last loaded or saved. Remove persists its change on
  // top of THIS, never on top of the form, so an unsaved edit elsewhere in
  // the form (the AI-data consent above all) is not saved behind the user's back.
  let savedLlm: LlmConfig | null = null;
  let savedApply = false;

  // Testing an edited-but-unsaved form probes the PREVIOUS configuration and
  // reports on that, which reads as a verdict on what is currently on screen.
  // The dropdown makes this worse, not better: its ✓ means "this binary is on
  // PATH", so a freshly picked provider looks ready while the probe correctly
  // answers "provider not configured" about something else entirely.
  const dirty = $derived(
    hasUnsavedEdits(savedFingerprint, formFingerprint(llmPayload(), applyToAccounts), apiKey),
  );

  // The probe resolves the SAVED config, exactly as a real Extract or Ask
  // Jodd turn would — so it can only tell the truth about what is on disk,
  // never about unsaved edits in this form. Hence the Save-first hint below.
  async function testConnection() {
    testing = true;
    testResult = null;
    try {
      // `null` is meaningful, not an omission: it selects the app-level
      // provider arm of the command rather than an account's cascade.
      testResult = await invoke<TestResult>('test_llm_provider', {
        accountId: isApp ? null : accountId,
      });
    } catch (e) {
      testResult = {
        ok: false,
        elapsed_ms: 0,
        error: String(e),
        raw_head: '',
        cause: null,
        action: null,
      };
    } finally {
      testing = false;
    }
  }

  function llmPayload(): LlmConfig {
    return {
      // Only accounts have a data permission; the app scope stores none (C5).
      data_allowed: isApp ? null : (cfg.provider === 'disabled' ? false : (cfg.data_allowed ?? false)),
      provider: cfg.provider,
      http_base_url: cfg.provider === 'http' ? cfg.http_base_url : null,
      http_model: cfg.provider === 'http' ? cfg.http_model : null,
      http_api_key_keychain: cfg.provider === 'http' ? keychainKey : null,
      agent_preset: cfg.provider === 'agent_cli' ? cfg.agent_preset : null,
      agent_custom:
        cfg.provider === 'agent_cli' && cfg.agent_preset === 'custom'
          ? {
              ...(cfg.agent_custom as AgentCliSpec),
              // One argument per line: splitting on spaces would
              // reintroduce the quoting problem this design avoids.
              args: customArgsText.split('\n').map((x) => x.trim()).filter(Boolean),
            }
          : null,
      // Only a server-run agent CLI carries a host; anything else clears it so
      // switching back to "This computer" really runs locally.
      agent_host: cfg.provider === 'agent_cli' && runsOn === 'server' ? (cfg.agent_host ?? null) : null,
      // Only the HTTP provider reads this, so it is cleared when the user
      // switches away rather than lingering in the file as dead state.
      disable_thinking: cfg.provider === 'http' ? cfg.disable_thinking : false,
    };
  }

  // The section-form contract (lib/settings/sectionForm.ts).
  export function isDirty(): boolean {
    return dirty;
  }

  // Throws a user-readable message when the write fails. The Settings page
  // shows it once, above this section (settings/SaveError.svelte), so it is
  // not also put in `msg`. A re-entrant call returns the save already running. Refuses what the old
  // per-section Save button was disabled for: an unusable key or a
  // write/check/test in progress throws instead of being skipped silently.
  let inFlight: Promise<void> | null = null;
  export function save(): Promise<void> {
    if (inFlight) return inFlight;
    if (editingKey && apiKey.length > 0 && !apiKey.trim()) return Promise.reject(new Error('Enter a valid API key.'));
    if (saving) return Promise.reject(new Error('Another save is in progress.'));
    if (checkingKey) return Promise.reject(new Error('Wait for the API key check to finish, then save.'));
    if (testing) return Promise.reject(new Error('Wait for the connection test to finish, then save.'));
    const run = saveForm().finally(() => { if (inFlight === run) inFlight = null; });
    inFlight = run;
    return run;
  }

  // Saving "A server" with no server would silently fall back to running the
  // CLI on this computer.
  const needsHost = $derived(cfg.provider === 'agent_cli' && runsOn === 'server' && !cfg.agent_host);

  // Remove deletes the server's key at once, so the saved config must stop
  // pointing at it now rather than at the next Save (which "A server" with no
  // host would refuse). Not "This computer": that would silently move where
  // the data goes. No provider at all (account scope: inherit the app's).
  async function serverRemoved(removed: AgentHost) {
    cfg.agent_host = null;
    remotePresets = [];
    // A server set up in this form but never saved: disk never pointed at it,
    // so there is nothing to persist — and the saved provider must survive.
    if (savedLlm?.agent_host?.key_dir !== removed.key_dir) return;
    const llm: LlmConfig = { ...savedLlm, provider: 'none', agent_preset: null, agent_custom: null, agent_host: null };
    // The server is gone either way, so the form reflects that now; on a
    // failed write it stays dirty and Save retries it.
    cfg.provider = 'none';
    cfg.agent_preset = null;
    testResult = null;  // an OK from the removed server is no longer true
    saving = true;
    try {
      await persist(llm, savedApply, null);
    } catch (e) {
      msg = `The server was removed, but saving the change failed: ${e}. Press Save to try again.`;
      return;
    } finally {
      saving = false;
    }
    rebaseline(llm, savedApply);
    msg = isApp
      ? 'Server removed. Choose an AI provider.'
      : "Server removed. This account now follows the app's AI provider, if it shares one with accounts.";
  }

  // Change server deletes the old server's key once the new one works, so,
  // as with Remove, a saved config pointing at the old key moves to the new
  // host now. Built on top of what is on disk, never the form: other unsaved
  // edits stay unsaved.
  async function serverReplaced(old: AgentHost, next: AgentHost) {
    testResult = null;  // an OK from the old server says nothing about the new one
    if (savedLlm?.agent_host?.key_dir !== old.key_dir) return;
    const llm: LlmConfig = { ...savedLlm, agent_host: next };
    saving = true;
    try {
      await persist(llm, savedApply, null);
    } catch (e) {
      msg = `The server was changed, but saving the change failed: ${e}. Press Save to try again.`;
      return;
    } finally {
      saving = false;
    }
    rebaseline(llm, savedApply);
    msg = `Server changed to ${hostLabel(next)}.`;
  }

  function rebaseline(llm: LlmConfig, apply: boolean) {
    savedLlm = $state.snapshot(llm);
    savedApply = apply;
    savedFingerprint = formFingerprint(llm, apply);
  }

  // Both backends share the api_key convention: null leaves the keychain
  // untouched, '' clears it, anything else writes it.
  async function persist(llm: LlmConfig, apply: boolean, key: string | null) {
    if (isApp) {
      await invoke('set_app_llm_config', { cfg: { llm, apply_to_accounts: apply }, apiKey: key });
    } else {
      await invoke('update_llm_settings', { accountId, cfg: llm, apiKey: key });
    }
  }

  async function saveForm(): Promise<void> {
    if (needsHost) throw new Error('save: set up your server first, or choose This computer.');
    saving = true;
    msg = '';
    testResult = null;  // a stale OK next to edited fields reads as approval
    try {
      const key = cfg.provider === 'http' && apiKey ? apiKey : null;
      await persist(llmPayload(), applyToAccounts, key);
      msg = 'Saved.';
      apiKey = '';  // never keep the cleartext in memory
      editingKey = false;
      showKey = false;
      // Re-baseline BEFORE apiKey is read again: the form now matches disk, so
      // Test is meaningful from here until the next edit.
      rebaseline(llmPayload(), applyToAccounts);
      if (isApp && key) hasStoredKey = true;
    } catch (e) {
      throw new Error(`save: ${e}`);
    } finally {
      saving = false;
    }
  }

  async function checkStoredKey() {
    checkingKey = true;
    msg = '';
    try {
      hasStoredKey = await invoke<boolean>('check_app_llm_key');
    } catch (e) {
      // A cancelled/denied read must preserve the previous status.
      msg = `Could not check API key: ${e}`;
    } finally {
      checkingKey = false;
    }
  }

  async function clearStoredKey() {
    confirmDeleteKey = false;
    saving = true;
    msg = '';
    try {
      await invoke('delete_app_llm_key');
      hasStoredKey = false;
      apiKey = '';
      editingKey = false;
      showKey = false;
      testResult = null;
      msg = 'Stored key deleted. Other unsaved settings were not saved.';
    } catch (e) {
      msg = `Could not delete API key: ${e}`;
    } finally {
      saving = false;
    }
  }

</script>

{#if confirmDeleteKey}
  <ConfirmDialog title="Delete API key?" message="Delete the saved API key from the credential store? Connections that use it may stop working. Other unsaved settings will be kept in this form." confirmLabel="Delete API key" destructive onConfirm={clearStoredKey} onCancel={() => { confirmDeleteKey = false; }} />
{/if}

<!-- A privacy note changes what the user should do, so its first sentence
     stays visible; the rest (verbatim) is under Learn more. -->
{#snippet privacyNote(note: string)}
  {@const part = splitNote(note)}
  <p class="help warn">{part.lead}</p>
  {#if part.more}
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>{part.more}</p>
    </details>
  {/if}
{/snippet}

<div class="llm-settings">
  {#if !isApp}
    <label class="radio">
      <input type="checkbox" bind:checked={cfg.data_allowed} disabled={cfg.provider === 'disabled'} />
      {DATA_ALLOWED_LABEL}
    </label>
    <p class="help">Covers Ask Jodd, notes made with AI, and suggestions. Data already sent cannot be recalled.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>It includes titles, tags and folders. Provider choice is separate; legacy Disabled also denies access.
        External MCP clients have separate access rules.</p>
    </details>
  {/if}
  {#if showHeading}<h3>AI provider</h3>{/if}
  {#if !isApp}
    <p class="help">Follow Settings › AI, or choose a provider for this account only. Ask Jodd always uses Settings › AI.</p>
  {/if}

  <label>
    <input type="radio" bind:group={cfg.provider} value="agent_cli" />
    Agent CLI (headless) — uses an AI agent you already have installed
  </label>

  {#if cfg.provider === 'agent_cli'}
    {@const onServer = runsOn === 'server'}
    {@const remoteHost = onServer && cfg.agent_host ? hostLabel(cfg.agent_host) : null}
    <div class="agent-fields">
      <div class="runs-on" role="radiogroup" aria-label="Runs on">
        <span class="runs-on-title">Runs on</span>
        {#if !$isAndroid}
          <label class="inline">
            <input type="radio" bind:group={runsOn} value="local" />
            This computer
          </label>
        {/if}
        <label class="inline">
          <input type="radio" bind:group={runsOn} value="server" />
          A server
        </label>
      </div>

      {#if onServer}
        <AiHostPanel
          host={cfg.agent_host ?? null}
          onchange={hostChanged}
          onremoved={serverRemoved}
          onreplaced={serverReplaced}
          removedNote={isApp
            ? 'Jodd will have no AI provider until you choose one.'
            : "This account then follows the app's AI provider if the app shares one with accounts; otherwise AI stays off for it until you choose a provider."}
        />
      {/if}

      <select bind:value={cfg.agent_preset}>
        <option value={null} disabled>Choose a CLI…</option>
        {#each presets.filter((p) => !onServer || REMOTE_CAPABLE.includes(p.id)) as p (p.id)}
          {@const usable = onServer
            ? remotePresets.find((r) => r.id === p.id)?.path != null
            : p.installed && p.available}
          <!-- documented exemption: the ✓ below is built by JS string
               concatenation inside an <option> label, not markup — an
               <Icon> component cannot be rendered inside <option> text. -->
          <option
            value={p.id}
            disabled={!usable}
            title={usable ? null : (onServer ? 'Not installed on the server.' : (p.unavailable_reason ?? 'Not installed on this machine.'))}
          >
            {p.label}{usable
              ? ' ✓' + (p.fidelity === 'heuristic' ? ' · no JSON mode' : '')
              : ' — TBA'}
          </option>
        {/each}
        {#if !onServer}<option value="custom">Custom…</option>{/if}
      </select>

      {#if cfg.agent_preset && cfg.agent_preset !== 'custom'}
        {@const sel = presets.find((p) => p.id === cfg.agent_preset)}
        {#if sel && onServer}
          {#if cfg.agent_host && remotePresets.find((r) => r.id === sel.id)?.path == null}
            <p class="help">Not installed on the server — install it there, or AI actions will fail.</p>
          {/if}
        {:else if sel && !sel.installed}
          <p class="help">Not found on PATH — install it, or AI actions will fail.</p>
        {/if}
        {#if sel && !sel.available && (onServer || sel.installed)}
          <p class="help warn">
            <strong>Not available yet.</strong> {sel.unavailable_reason}
          </p>
        {/if}
        {#if sel && sel.available && sel.fidelity === 'heuristic'}
          <p class="help">This CLI has no JSON output mode, so Jodd may need to retry.</p>
          <details class="learn-more">
            <summary>Learn more</summary>
            <p>Jodd digs the result out of its plain output.</p>
          </details>
        {/if}
        {#if sel}
          {#each cliPrivacyNotes(sel.file_access, sel.argv_prompt, remoteHost) as note}
            {@render privacyNote(note)}
          {/each}
        {/if}
      {/if}

      {#if needsHost}
        <p class="help warn">Set up your server above before saving.</p>
      {/if}

      {#if cfg.agent_preset === 'custom' && cfg.agent_custom && !onServer}
        {#each cliPrivacyNotes('unknown', cfg.agent_custom.prompt_delivery === 'argv') as note}
          {@render privacyNote(note)}
        {/each}
        <div class="custom-fields">
          <label>Binary
            <input type="text" bind:value={cfg.agent_custom.binary} placeholder="my-agent" /></label>
          <label>Arguments (one per line)
            <textarea rows="5" bind:value={customArgsText} placeholder={'-p\n{system}'}></textarea></label>
          <label>Prompt delivery
            <select bind:value={cfg.agent_custom.prompt_delivery}>
              <option value="stdin_all">Everything on stdin</option>
              <option value="stdin_payload_system_arg">System prompt in args, text on stdin</option>
              <option value="argv">Everything in args (risky for long text)</option>
            </select></label>
          <label>Output source
            <select bind:value={cfg.agent_custom.output}>
              <option value="stdout">stdout</option>
              <option value="last_message_file">file passed as {'{out_file}'}</option>
            </select></label>
          <label>JSON field to unwrap (blank if the output is plain text)
            <input type="text" bind:value={cfg.agent_custom.unwrap} placeholder="result" /></label>
          <label>Timeout (seconds)
            <input type="number" bind:value={cfg.agent_custom.timeout_secs} /></label>
        </div>
      {/if}
    </div>
  {/if}

  <label>
    <input type="radio" bind:group={cfg.provider} value="http" />
    Custom endpoint (OpenAI-compatible)
  </label>

  {#if cfg.provider === 'http'}
    <div class="http-fields">
      <label>
        Base URL
        <input type="text" bind:value={cfg.http_base_url} placeholder="https://api.openai.com/v1" />
      </label>
      <label>
        Model
        <input type="text" bind:value={cfg.http_model} placeholder="gpt-4o-mini" />
      </label>
      {#if isApp}
        <p role="status">API key: {hasStoredKey === null ? 'Not checked yet' : hasStoredKey ? 'Previously saved' : 'Last known to be absent'}.</p>
        <p class="help">Status does not check the credential store automatically.</p>
        <details class="learn-more">
          <summary>Learn more</summary>
          <p>Changes made outside Jodd may not be reflected here.</p>
        </details>
        <button onclick={checkStoredKey} disabled={saving || checkingKey || testing}>
          {checkingKey ? 'Checking…' : 'Check API key'}
        </button>
        <button onclick={() => { editingKey = true; }} disabled={saving || checkingKey || testing}>
          Change API key
        </button>
        {#if hasStoredKey !== false}
          <button class="link-btn" onclick={() => { confirmDeleteKey = true; }} disabled={saving || checkingKey || testing}>
            Delete API key
          </button>
        {/if}
      {/if}
      {#if !isApp || editingKey}
        <label>
          New API key
          <input class="api-key-input" type={showKey ? 'text' : 'password'} bind:value={apiKey} autocomplete="new-password" placeholder="Leave blank to keep any existing key" disabled={saving || checkingKey || testing} />
        </label>
        <button onclick={() => { showKey = !showKey; }} aria-pressed={showKey}>
          {showKey ? 'Hide new key' : 'Show new key'}
        </button>
        {#if isApp}
          <button onclick={() => { editingKey = false; apiKey = ''; showKey = false; }} disabled={saving}>Cancel key change</button>
        {/if}
      {/if}
      <p class="help">macOS may ask for permission to use your credential store.</p>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>Checking, saving a new key, deleting a key, or testing a connection may ask for it. Leaving the key blank keeps it unchanged.</p>
      </details>
      <label class="inline">
        <input type="checkbox" bind:checked={cfg.disable_thinking} />
        Disable thinking (local llama.cpp / Qwen3)
      </label>
      <p class="help">Turn this on for a local llama.cpp server running a reasoning model such as Qwen3.</p>
      <details class="learn-more">
        <summary>Learn more</summary>
        <p>
          It skips the model's “thinking” step, which otherwise burns hundreds
          of tokens before the answer and can time out on a consumer GPU. Leave it off
          for hosted providers (Anthropic, OpenAI, Kilo) — they ignore the setting.
        </p>
      </details>
    </div>
  {/if}

  {#if isApp}
    <label>
      <input type="radio" bind:group={cfg.provider} value="none" />
      Not configured
    </label>
  {:else}
    <label>
      <input type="radio" bind:group={cfg.provider} value="none" />
      <!-- The separator is an expression: Svelte trims a space at the start
           of the span's text, which rendered "Use app default· Codex". -->
      Use app default{#if inherited?.name}<span class="inherit-name">{` · ${inherited.name}`}</span>{/if}
    </label>
    {#if cfg.provider === 'none' && inherited}
      {#if inherited.noAi}
        <p class="help warn inherit">
          {inherited.reason === 'not_configured'
            ? 'Settings › AI has no provider set up, so this account uses no AI.'
            : 'Settings › AI doesn’t share its provider with accounts, so this account uses no AI.'}
        </p>
        <details class="learn-more">
          <summary>Learn more</summary>
          <p>
            {inherited.reason === 'not_configured'
              ? 'Set up a provider in Settings › AI and tick its “Also use for accounts…” box, or choose a provider above for this account only.'
              : 'In Settings › AI, tick the “Also use for accounts…” box under the provider, or choose a provider above for this account only.'}
          </p>
        </details>
      {:else if !cfg.data_allowed}
        <!-- The provider is inherited, but the backend refuses every AI call
             for an account whose data is not allowed
             (llm::policy::require_account, before any provider is built), so
             "From Settings › AI" alone would read as if it were in use. -->
        <p class="help warn inherit">
          “{DATA_ALLOWED_LABEL}” is off, so nothing from this account is sent. Tick it above to use {inherited.name}.
        </p>
      {:else}
        <p class="help inherit">From Settings › AI. Change it there, or choose a provider above for this account only.</p>
      {/if}
    {/if}

    <label>
      <input type="radio" bind:group={cfg.provider} value="disabled" />
      Disabled (never use AI here)
    </label>
  {/if}

  {#if isApp}
    <label class="inline">
      <input type="checkbox" bind:checked={applyToAccounts} />
      Also use for accounts set to “Use app default”
    </label>
    <p class="help">Each account’s own choice in Settings › Accounts still wins. Ask Jodd always uses this provider.</p>
  {/if}

  <div class="btn-row">
    <button
      onclick={testConnection}
      disabled={testing || saving || checkingKey || !canTest || dirty}
      title={dirty ? 'Save first — Test runs the saved configuration' : undefined}
    >
      {testing ? 'Testing…' : 'Test connection'}
    </button>
  </div>
  {#if dirty}
    <p class="help warn">Unsaved changes — press Save to test them.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>Test runs the <em>saved</em> configuration, so it would report on the previous one.</p>
    </details>
  {:else if cfg.provider === 'agent_cli' && !cfg.agent_preset}
    <p class="help warn">Choose a CLI above and Save to test it.</p>
  {:else}
    <p class="help">Test runs the <em>saved</em> settings through one real AI request and uses its quota.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>It typically takes about 10&ndash;20 seconds, longer if your AI tool stalls.</p>
    </details>
  {/if}

  {#if testResult}
    <div class="test-result" class:bad={!testResult.ok}>
      <strong>{testResult.ok ? 'OK' : 'Failed'}</strong> · {testResult.elapsed_ms} ms
      {#if !isApp && isConsentRefusal(testResult.error)}
        <p class="action">Tick “{DATA_ALLOWED_LABEL}” above and Save, then test again.</p>
      {/if}
      {#if testResult.cause}
        <p class="cause">{testResult.cause}</p>
        {#if testResult.action}<p class="action">→ {testResult.action}</p>{/if}
        <details>
          <summary>Show details</summary>
          {#if testResult.error}<p class="err">{testResult.error}</p>{/if}
          {#if testResult.raw_head}<pre>{testResult.raw_head}</pre>{/if}
        </details>
      {:else}
        <!-- Nothing recognised the failure, so the evidence is all the user
             has. Open by default: hiding an unexplained error behind a click
             is what made this class of bug undiagnosable in the first place. -->
        {#if testResult.error}<p class="err">{testResult.error}</p>{/if}
        {#if testResult.raw_head}<pre>{testResult.raw_head}</pre>{/if}
      {/if}
    </div>
  {/if}

  {#if msg}<p class="msg">{msg}</p>{/if}
</div>

<style>
  .llm-settings { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: var(--size); }
  .agent-fields { margin-left: 24px; padding: 8px; background: var(--surface-list); border-radius: 4px; }
  .runs-on { display: flex; flex-wrap: wrap; gap: 4px 16px; align-items: center; }
  .runs-on-title { font-size: var(--size); font-weight: 600; }
  .custom-fields { margin-top: 8px; display: grid; gap: 6px; }
  /* Every control below states its own background. The UA stylesheet beats
     inheritance for form controls, so leaving it unset is the UA's white in
     both themes — measured 1.25:1 against --text in dark mode, i.e. invisible.
     The enclosing .agent-fields / .http-fields background does not help: a
     control does not inherit its parent's. */
  .custom-fields textarea {
    width: 100%; font-family: var(--font-mono); font-size: var(--size-sm);
    line-height: var(--leading); box-sizing: border-box;
    background: var(--surface-panel); color: var(--text);
  }
  select { width: 100%; padding: 6px; font: inherit; background: var(--surface-panel); color: var(--text); }
  .test-result { margin-top: 8px; padding: 8px; border-radius: 4px; background: var(--success-wash); font-size: var(--size-sm); }
  .test-result.bad { background: var(--danger-wash); }
  .test-result pre { white-space: pre-wrap; word-break: break-all; margin: 6px 0 0; font-family: var(--font-mono); font-size: var(--size-xs); line-height: var(--leading); }
  .err { color: var(--danger); margin: 4px 0 0; }
  .cause { margin: 4px 0 0; font-weight: 600; }
  .action { margin: 2px 0 0; }
  .test-result details { margin-top: 6px; }
  .test-result summary { cursor: pointer; font-size: var(--size-xs); }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 4px 0 0; }
  .inherit-name { color: var(--text-muted); }
  .help.warn { background: var(--warn-wash); border-left: 3px solid var(--accent); padding: 6px 8px; border-radius: 3px; line-height: var(--leading); }
  .http-fields { margin-left: 24px; padding: 8px; background: var(--surface-list); border-radius: 4px; }
  .api-key-input, input[type="text"], input[type="password"], input[type="number"] {
    width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px;
    background: var(--surface-panel); color: var(--text);
  }
  button { padding: 8px 16px; font: inherit; }
  .btn-row { display: flex; gap: 8px; margin-top: 12px; }
  /* The label-level `display: block` above would put the checkbox on its own
     line, away from the text it labels. */
  .inline { display: flex; align-items: flex-start; gap: 8px; line-height: var(--leading); }
  .inline input { margin: 0; flex: none; }
  .link-btn {
    background: none; border: none; padding: 0; margin-top: 6px;
    color: var(--accent-action); font: inherit; font-size: var(--size-sm);
    text-decoration: underline; cursor: pointer;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .msg { font-size: var(--size-sm); color: var(--text-muted); }
</style>
