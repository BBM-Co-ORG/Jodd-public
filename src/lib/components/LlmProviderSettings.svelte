<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { formFingerprint, hasUnsavedEdits } from './llmFormDirty';
  import { isAndroid } from '../stores/platform';

  // One form, two scopes. `account` edits accounts.json via the per-account
  // commands; `app` edits app_llm.json via the app-level ones. Everything
  // between — the preset dropdown, the PATH-detection hints, the probe — is
  // identical, and the app-level provider is the one behind Ask Jodd, so it
  // needs the hints and the probe at least as much as an account does.
  // showHeading: Account Settings drops this component in bare and relies on
  // its <h3>. App Settings renders it inside a <section> that already carries
  // its own heading and description, matching its sibling sections — so
  // without this the panel showed "LLM provider" immediately above
  // "LLM Provider".
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
  };

  type LlmConfig = {
    provider: 'none' | 'disabled' | 'claude_code' | 'http' | 'agent_cli';
    http_base_url: string | null;
    http_model: string | null;
    http_api_key_keychain: string | null;
    agent_preset: string | null;
    agent_custom: AgentCliSpec | null;
  };

  let cfg: LlmConfig = $state({
    provider: 'none',
    http_base_url: '',
    http_model: '',
    http_api_key_keychain: null,
    agent_preset: null,
    agent_custom: null,
  });
  type AppLlmConfig = { llm: LlmConfig; apply_to_accounts: boolean };

  let presets: PresetInfo[] = $state([]);
  let customArgsText = $state('');
  let apiKey = $state('');
  // App scope only: whether a key is already in the keychain, so the field can
  // say "leave blank to keep" honestly rather than guessing.
  let hasStoredKey = $state(false);
  let applyToAccounts = $state(false);
  type TestResult = { ok: boolean; elapsed_ms: number; error: string | null; raw_head: string };
  let testing = $state(false);
  let testResult: TestResult | null = $state(null);
  let saving = $state(false);
  let msg = $state('');

  onMount(load);

  async function load() {
    try {
      presets = await invoke<PresetInfo[]>('list_agent_cli_presets');
      if (isApp) {
        const res = await invoke<{ cfg: AppLlmConfig; has_api_key: boolean }>(
          'get_app_llm_config',
        );
        cfg = res.cfg.llm;
        applyToAccounts = res.cfg.apply_to_accounts;
        hasStoredKey = res.has_api_key;
      } else {
        cfg = await invoke<LlmConfig>('get_llm_settings', { accountId });
      }
      // Accounts saved before v0.19 say "claude_code"; show them as the
      // claude preset without rewriting anything until the user saves.
      if (cfg.provider === 'claude_code') {
        cfg.provider = 'agent_cli';
        cfg.agent_preset = 'claude';
      }
      // `disabled` is an account-scoped concept (opt out of the app default).
      // There is nothing above the app to opt out of, so show it as unset.
      if (isApp && cfg.provider === 'disabled') cfg.provider = 'none';
      apiKey = '';
      customArgsText = (cfg.agent_custom?.args ?? []).join('\n');
      // Baseline for `dirty`. Taken AFTER the legacy-provider rewrites above,
      // so showing a "claude_code" account as the claude preset does not read
      // as an edit the user made.
      savedFingerprint = formFingerprint(llmPayload(), applyToAccounts);
    } catch (e) {
      msg = `load: ${e}`;
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
      testResult = { ok: false, elapsed_ms: 0, error: String(e), raw_head: '' };
    } finally {
      testing = false;
    }
  }

  function llmPayload(): LlmConfig {
    return {
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
    };
  }

  async function save() {
    saving = true;
    msg = '';
    testResult = null;  // a stale OK next to edited fields reads as approval
    try {
      // Both backends share the api_key convention: null leaves the keychain
      // untouched, '' clears it, anything else writes it.
      const key = cfg.provider === 'http' && apiKey ? apiKey : null;
      if (isApp) {
        await invoke('set_app_llm_config', {
          cfg: { llm: llmPayload(), apply_to_accounts: applyToAccounts },
          apiKey: key,
        });
      } else {
        await invoke('update_llm_settings', { accountId, cfg: llmPayload(), apiKey: key });
      }
      msg = 'Saved.';
      apiKey = '';  // never keep the cleartext in memory
      // Re-baseline BEFORE apiKey is read again: the form now matches disk, so
      // Test is meaningful from here until the next edit.
      savedFingerprint = formFingerprint(llmPayload(), applyToAccounts);
      if (isApp && key) hasStoredKey = true;
    } catch (e) {
      msg = `save: ${e}`;
    } finally {
      saving = false;
    }
  }

  async function clearStoredKey() {
    saving = true;
    msg = '';
    try {
      await invoke('set_app_llm_config', {
        cfg: { llm: llmPayload(), apply_to_accounts: applyToAccounts },
        apiKey: '',
      });
      hasStoredKey = false;
      apiKey = '';
      msg = 'Stored key cleared.';
    } catch (e) {
      msg = `save: ${e}`;
    } finally {
      saving = false;
    }
  }
</script>

<div class="llm-settings">
  {#if showHeading}<h3>LLM Provider</h3>{/if}

  {#if !$isAndroid}
    <label>
      <input type="radio" bind:group={cfg.provider} value="agent_cli" />
      Agent CLI (headless) — uses an AI agent you already have installed
    </label>

    {#if cfg.provider === 'agent_cli'}
      <div class="agent-fields">
        <select bind:value={cfg.agent_preset}>
          <option value={null} disabled>Choose a CLI…</option>
          {#each presets as p (p.id)}
            {@const usable = p.installed && p.available}
            <!-- documented exemption: the ✓ below is built by JS string
                 concatenation inside an <option> label, not markup — an
                 <Icon> component cannot be rendered inside <option> text. -->
            <option
              value={p.id}
              disabled={!usable}
              title={usable ? null : (p.unavailable_reason ?? 'Not installed on this machine.')}
            >
              {p.label}{usable
                ? ' ✓' + (p.fidelity === 'heuristic' ? ' · no JSON mode' : '')
                : ' — TBA'}
            </option>
          {/each}
          <option value="custom">Custom…</option>
        </select>

        {#if cfg.agent_preset && cfg.agent_preset !== 'custom'}
          {@const sel = presets.find((p) => p.id === cfg.agent_preset)}
          {#if sel && !sel.installed}
            <p class="hint">Not found on PATH — install it, or extraction will fail.</p>
          {:else if sel && !sel.available}
            <p class="hint warn">
              <strong>Not available yet.</strong> {sel.unavailable_reason}
            </p>
          {/if}
          {#if sel && sel.available && sel.fidelity === 'heuristic'}
            <p class="hint">This CLI has no JSON output mode; Jodd digs the result
              out of its plain output and may need to retry.</p>
          {/if}
        {/if}

        {#if cfg.agent_preset === 'custom' && cfg.agent_custom}
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
      <label>
        API key
        <input type="password" bind:value={apiKey} placeholder="(leave blank to keep existing)" />
      </label>
      {#if isApp && hasStoredKey}
        <button class="link-btn" onclick={clearStoredKey} disabled={saving}>
          Clear stored key
        </button>
      {/if}
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
      Use app default
    </label>

    <label>
      <input type="radio" bind:group={cfg.provider} value="disabled" />
      Disabled (never use an LLM here)
    </label>
  {/if}

  {#if isApp}
    <label class="inline">
      <input type="checkbox" bind:checked={applyToAccounts} />
      Also use for Extract and auto-link on accounts with no provider of their own
    </label>
  {/if}

  <div class="btn-row">
    <button onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save'}</button>
    <button
      onclick={testConnection}
      disabled={testing || !canTest || dirty}
      title={dirty ? 'Save first — Test runs the saved configuration' : undefined}
    >
      {testing ? 'Testing…' : 'Test connection'}
    </button>
  </div>
  {#if dirty}
    <p class="hint warn">Unsaved changes — press Save to test them. Test runs the
      <em>saved</em> configuration, so it would report on the previous one.</p>
  {:else}
    <p class="hint">Test runs the <em>saved</em> settings through a one-line prompt.</p>
  {/if}

  {#if testResult}
    <div class="test-result" class:bad={!testResult.ok}>
      <strong>{testResult.ok ? 'OK' : 'Failed'}</strong> · {testResult.elapsed_ms} ms
      {#if testResult.error}<p class="err">{testResult.error}</p>{/if}
      {#if testResult.raw_head}<pre>{testResult.raw_head}</pre>{/if}
    </div>
  {/if}

  {#if msg}<p class="msg">{msg}</p>{/if}
</div>

<style>
  .llm-settings { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: var(--size); }
  .agent-fields { margin-left: 24px; padding: 8px; background: var(--surface-list); border-radius: 4px; }
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
  .hint { font-size: var(--size-sm); color: var(--accent-action); margin: 4px 0 0; }
  .hint.warn { background: var(--warn-wash); border-left: 3px solid var(--accent); padding: 6px 8px; border-radius: 3px; line-height: var(--leading); }
  .http-fields { margin-left: 24px; padding: 8px; background: var(--surface-list); border-radius: 4px; }
  input[type="text"], input[type="password"], input[type="number"] {
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
