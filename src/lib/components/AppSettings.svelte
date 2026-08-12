<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { appSettingsOpen } from '../stores/ui';
  import { isAndroid } from '../stores/platform';
  import Icon from './Icon.svelte';
  import LlmProviderSettings from './LlmProviderSettings.svelte';
  import { getThemePref, setThemePref, type ThemePref } from '../theme';

  let theme = $state<ThemePref>(getThemePref());

  function chooseTheme(next: ThemePref) {
    theme = next;
    setThemePref(next);
  }

  const DOCS_URL = 'https://developers.google.com/identity/protocols/oauth2/native-app';

  let clientId = $state('');
  let clientSecret = $state('');
  let hasSecret = $state(false);
  // The client_id as last persisted. A secret belongs to exactly one client_id,
  // so "leave blank to keep the stored secret" is only valid while the id in the
  // field still matches this one.
  let savedClientId = $state('');
  let showSecret = $state(false);
  let saving = $state(false);
  let errorMsg = $state('');

  let fileLoggingEnabled = $state(true);
  let logFilePath = $state('');
  let logFileSizeBytes = $state(0);
  let clearingLog = $state(false);
  let logSettingsError = $state('');

  $effect(() => {
    if ($appSettingsOpen) {
      loadConfig();
      loadLogSettings();
    }
  });

  async function loadConfig() {
    errorMsg = '';
    clientSecret = '';
    try {
      const cfg = await invoke<{ client_id: string; has_secret: boolean; credentials_available: boolean }>('get_oauth_config');
      clientId = cfg.client_id;
      savedClientId = cfg.client_id;
      hasSecret = cfg.has_secret;
    } catch (e) {
      errorMsg = String(e);
    }
  }

  async function loadLogSettings() {
    logSettingsError = '';
    try {
      const cfg = await invoke<{
        file_logging_enabled: boolean;
        log_file_path: string;
        log_file_size_bytes: number;
      }>('get_log_settings');
      fileLoggingEnabled = cfg.file_logging_enabled;
      logFilePath = cfg.log_file_path;
      logFileSizeBytes = cfg.log_file_size_bytes;
    } catch (e) {
      logSettingsError = String(e);
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  }

  async function clearLog() {
    clearingLog = true;
    logSettingsError = '';
    try {
      await invoke('clear_log_file');
      logFileSizeBytes = 0;
    } catch (e) {
      logSettingsError = String(e);
    } finally {
      clearingLog = false;
    }
  }

  // Applies immediately (unlike the OAuth fields above, which wait for the
  // Save button) — a logging toggle has no "draft" state worth staging.
  async function toggleFileLogging() {
    const next = !fileLoggingEnabled;
    fileLoggingEnabled = next; // optimistic
    try {
      await invoke('set_file_logging_enabled', { enabled: next });
    } catch (e) {
      fileLoggingEnabled = !next; // rollback
      logSettingsError = String(e);
    }
  }

  // revealItemInDir opens a file manager to the file's location — meaningful
  // on desktop, but @tauri-apps/plugin-opener has no Android implementation
  // (there's no OS-level "reveal in file manager" surface for a sandboxed
  // app's private storage there anyway), so it always throws "API not
  // supported on the current platform" on Android. Copying the path is the
  // Android-appropriate equivalent: the path is also shown as selectable
  // text above (<code class="log-path">), but a tap-to-copy affordance is
  // friendlier than requiring a manual long-press-select on a monospace path.
  async function revealLogFile() {
    if (!logFilePath) return;
    try {
      if ($isAndroid) {
        await navigator.clipboard.writeText(logFilePath);
      } else {
        await revealItemInDir(logFilePath);
      }
    } catch (e) {
      logSettingsError = String(e);
    }
  }

  // Mirrors plan_cred_write in lib.rs. The backend rejects this case regardless
  // — this only turns a failed save into a disabled button and an explanation.
  const secretRequired = $derived(
    clientId.trim().length > 0 &&
      clientSecret.trim().length === 0 &&
      !(hasSecret && clientId.trim() === savedClientId)
  );

  async function save() {
    if (secretRequired) return;
    saving = true;
    errorMsg = '';
    try {
      await invoke('save_oauth_config', { clientId: clientId.trim(), clientSecret });
      savedClientId = clientId.trim();
      hasSecret = savedClientId.length > 0 && (clientSecret.trim().length > 0 || hasSecret);
      clientSecret = '';
      appSettingsOpen.set(false);
    } catch (e) {
      errorMsg = String(e);
    } finally {
      saving = false;
    }
  }

  async function clearCreds() {
    if (!confirm('Remove saved credentials? Gmail sync will stop working until you re-configure.')) return;
    saving = true;
    errorMsg = '';
    try {
      await invoke('clear_oauth_config');
      clientId = '';
      savedClientId = '';
      clientSecret = '';
      hasSecret = false;
    } catch (e) {
      errorMsg = String(e);
    } finally {
      saving = false;
    }
  }

  function close() {
    appSettingsOpen.set(false);
  }
</script>

<svelte:window onkeydown={(e) => { if ($appSettingsOpen && e.key === 'Escape') close(); }} />

{#if $appSettingsOpen}
  <div class="overlay" role="presentation" onclick={close}>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="app-settings-title"
      onclick={(e) => e.stopPropagation()}
    >
      <h2 id="app-settings-title">App Settings</h2>

      <section class="section">
        <h3 id="appearance-heading">Appearance</h3>
        <div class="theme-picker" role="group" aria-labelledby="appearance-heading">
          {#each [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']] as [value, label] (value)}
            <button
              type="button"
              aria-pressed={theme === value}
              class="theme-option"
              class:active={theme === value}
              onclick={() => chooseTheme(value as ThemePref)}
            >{label}</button>
          {/each}
        </div>
        <p class="hint">System follows your operating system setting.</p>
      </section>

      <section class="section">
        <h3>Google OAuth Credentials</h3>
        <!-- Shown on Android too. The client TYPE differs because the redirect
             does: Android's is an https App Links URL, which only a Web
             application client may declare, while a Desktop client is limited
             to http://localhost. Both are honored by auth::client_id(), which
             checks a stored override before it looks at the platform. -->
        <p class="hint">
          Required for Gmail sync. Create a
          <strong>{$isAndroid ? 'Web application' : 'Desktop application'}</strong> OAuth client in
          <button class="link-btn" onclick={() => openUrl(DOCS_URL)}>Google Cloud Console</button>
          and paste the Client ID and Secret below.
        </p>
        {#if $isAndroid}
          <p class="hint">
            On the Web client, authorize this exact redirect URI:
            <code>https://jodd.bbmedia.co.th/oauth2redirect</code>
          </p>
        {/if}

          <label>
            <span class="lbl">Client ID</span>
            <input
              class="field"
              type="text"
              bind:value={clientId}
              placeholder="123456-abc.apps.googleusercontent.com"
              autocomplete="off"
            />
          </label>

          <label>
            <span class="lbl">Client Secret</span>
            <div class="secret-row">
              <input
                class="field"
                type={showSecret ? 'text' : 'password'}
                bind:value={clientSecret}
                placeholder={hasSecret && clientId.trim() === savedClientId
                  ? '(already saved — leave blank to keep)'
                  : ''}
                autocomplete="new-password"
              />
              <button
                class="toggle-btn"
                onclick={() => (showSecret = !showSecret)}
                title={showSecret ? 'Hide' : 'Show'}
                aria-label={showSecret ? 'Hide client secret' : 'Show client secret'}
              ><Icon name={showSecret ? 'eye-off' : 'eye'} size={14} /></button>
            </div>
          </label>

          {#if secretRequired}
            <p class="hint warn">
              {hasSecret && savedClientId
                ? 'This Client ID differs from the saved one, so the stored secret no longer matches it. Paste the secret issued for this Client ID.'
                : 'Paste the secret issued for this Client ID — Google requires it even with PKCE.'}
            </p>
          {/if}

          {#if errorMsg}
            <p class="error">{errorMsg}</p>
          {/if}
      </section>

      <section class="section">
        <h3>LLM provider</h3>
        <p class="hint">
          Used by Ask Jodd. Accounts without their own provider can adopt it for
          Extract and auto-link.
        </p>
        <!-- Same component as Account Settings, in `app` scope. The app-level
             provider is the ONLY provider behind Ask Jodd, so it needs the
             PATH-detected preset list and the probe at least as much as an
             account does; a free-text preset field made `cluade` unfalsifiable
             until the user asked a question and got a red turn. -->
        <LlmProviderSettings scope="app" showHeading={false} />
      </section>

      <section class="section">
        <h3>Diagnostics</h3>
        <label class="toggle-row">
          <input type="checkbox" checked={fileLoggingEnabled} onchange={toggleFileLogging} />
          <span>Save app logs to a file (recommended)</span>
        </label>
        <p class="hint">
          Helps diagnose sync issues after the fact — the app window itself
          doesn't show its own diagnostic log. Automatically trimmed once it
          passes 20 MB.
          {#if logFilePath}
            <br /><code class="log-path">{logFilePath}</code>
            <span class="log-size">({fmtBytes(logFileSizeBytes)})</span>
          {/if}
        </p>
        {#if logFilePath}
          <div class="log-actions">
            <button class="link-btn" onclick={revealLogFile}>{$isAndroid ? 'Copy log path' : 'Reveal log file'}</button>
            <button
              class="link-btn danger"
              onclick={clearLog}
              disabled={clearingLog || logFileSizeBytes === 0}
            >{clearingLog ? 'Clearing…' : 'Clear log'}</button>
          </div>
        {/if}
        {#if logSettingsError}
          <p class="error">{logSettingsError}</p>
        {/if}
      </section>

      <!-- Clear and Save act on the OAuth credential fields above, so they are
           shown wherever that section is — which is now everywhere. Hiding them
           on Android while the fields were also hidden was consistent; leaving
           them hidden now would render an editable form with no way to save. -->
      <div class="actions">
        <button class="btn-clear" onclick={clearCreds} disabled={saving}>Clear</button>
        <div class="spacer"></div>
        <button class="btn-cancel" onclick={close} disabled={saving}>Cancel</button>
        <button
          class="btn-save"
          onclick={save}
          disabled={saving || secretRequired}
          title={secretRequired ? 'Enter the client secret for this Client ID' : undefined}
        >
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed; inset: 0;
    background: var(--scrim);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: var(--surface-editor);
    border-radius: 10px;
    padding: 20px 24px;
    width: min(440px, 90vw);
    box-shadow: var(--shadow-modal);
    /* The overlay centres this box in a fixed, full-viewport flex container,
       so without a bound a tall panel overflows the window at the top AND the
       bottom at once, with no ancestor able to scroll it. Same 85vh the
       Extract modal uses. */
    max-height: 85vh;
    overflow-y: auto;
  }
  h2 { margin: 0 0 16px; font-size: var(--size-lg); }
  h3 { margin: 0 0 6px; font-size: var(--size-md); font-weight: 600; }
  .hint { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 12px; line-height: 1.5; }
  .hint.warn { color: var(--danger); margin-top: 4px; }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .link-btn.danger { color: var(--danger); }
  label { display: block; margin-bottom: 10px; }
  .lbl { display: block; font-size: var(--size-sm); color: var(--text-secondary); margin-bottom: 3px; }
  .field {
    width: 100%; padding: 6px 8px; border: 1px solid var(--border-strong);
    border-radius: 6px; font-size: var(--size); box-sizing: border-box; background: var(--surface-panel);
  }
  .secret-row { display: flex; gap: 4px; }
  .secret-row .field { flex: 1; }
  .toggle-btn {
    display: inline-flex; align-items: center; justify-content: center;
    border: 1px solid var(--border-strong); border-radius: 6px;
    background: var(--surface-sidebar); cursor: pointer; padding: 0 8px; font-size: var(--size);
  }
  .error { color: var(--danger); font-size: var(--size-sm); margin: 4px 0 0; }
  .actions { display: flex; gap: 8px; align-items: center; margin-top: 16px; }
  .spacer { flex: 1; }
  .btn-save, .btn-cancel, .btn-clear {
    padding: 6px 14px; border: 1px solid var(--border-strong); border-radius: 6px;
    background: var(--surface-sidebar); cursor: pointer; font-size: var(--size);
  }
  .btn-save { background: var(--accent-action); color: var(--text-inverse); border-color: var(--accent-hover); }
  .btn-save:disabled, .btn-cancel:disabled, .btn-clear:disabled {
    opacity: 0.6; cursor: not-allowed;
  }
  .section { margin-bottom: 16px; }
  .theme-picker {
    display: flex;
    gap: 6px;
    margin-bottom: 8px;
  }
  .theme-option {
    flex: 1;
    padding: 7px 10px;
    font-family: inherit;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
  }
  .theme-option:hover { background: var(--surface-hover); }
  .theme-option:focus-visible { box-shadow: var(--ring-focus); }
  .theme-option.active {
    color: var(--accent-on-tint);
    background: var(--accent-tint);
    border-color: var(--accent-border);
    font-weight: 600;
  }
  .toggle-row {
    display: flex; align-items: center; gap: 8px;
    font-size: var(--size); color: var(--text); margin-bottom: 6px; cursor: pointer;
  }
  .toggle-row input { margin: 0; }
  .log-path {
    display: inline-block; margin-top: 4px;
    font-size: var(--size-xs); color: var(--text-muted); word-break: break-all;
  }
  .log-size { font-size: var(--size-xs); color: var(--text-muted); }
  .log-actions { display: flex; gap: 14px; margin-top: 2px; }
</style>
