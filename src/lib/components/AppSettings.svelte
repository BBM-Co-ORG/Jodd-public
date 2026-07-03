<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { appSettingsOpen } from '../stores/ui';

  const DOCS_URL = 'https://developers.google.com/identity/protocols/oauth2/native-app';

  let clientId = $state('');
  let clientSecret = $state('');
  let hasSecret = $state(false);
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

  async function revealLogFile() {
    if (!logFilePath) return;
    try {
      await revealItemInDir(logFilePath);
    } catch (e) {
      logSettingsError = String(e);
    }
  }

  async function save() {
    saving = true;
    errorMsg = '';
    try {
      await invoke('save_oauth_config', { clientId: clientId.trim(), clientSecret });
      if (clientSecret.length > 0) hasSecret = true;
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
        <h3>Google OAuth Credentials</h3>
        <p class="hint">
          Required for Gmail sync. Create a <strong>Desktop application</strong> OAuth client in
          <button class="link-btn" onclick={() => openUrl(DOCS_URL)}>Google Cloud Console</button>
          and paste the Client ID and Secret below.
        </p>

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
              placeholder={hasSecret ? '(already saved — leave blank to keep)' : ''}
              autocomplete="new-password"
            />
            <button
              class="toggle-btn"
              onclick={() => (showSecret = !showSecret)}
              title={showSecret ? 'Hide' : 'Show'}
            >{showSecret ? '🙈' : '👁'}</button>
          </div>
        </label>

        {#if errorMsg}
          <p class="error">{errorMsg}</p>
        {/if}
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
            <button class="link-btn" onclick={revealLogFile}>Reveal log file</button>
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

      <div class="actions">
        <button class="btn-clear" onclick={clearCreds} disabled={saving}>Clear</button>
        <div class="spacer"></div>
        <button class="btn-cancel" onclick={close} disabled={saving}>Cancel</button>
        <button class="btn-save" onclick={save} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed; inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex; align-items: center; justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: #fdfcf7;
    border-radius: 10px;
    padding: 20px 24px;
    width: min(440px, 90vw);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
  }
  h2 { margin: 0 0 16px; font-size: 18px; }
  h3 { margin: 0 0 6px; font-size: 14px; font-weight: 600; }
  .hint { font-size: 12px; color: #666; margin: 0 0 12px; line-height: 1.5; }
  .link-btn {
    background: none; border: none; color: #4a90d9; cursor: pointer;
    padding: 0; font-size: 12px; text-decoration: underline;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .link-btn.danger { color: #c0392b; }
  label { display: block; margin-bottom: 10px; }
  .lbl { display: block; font-size: 12px; color: #5a5a52; margin-bottom: 3px; }
  .field {
    width: 100%; padding: 6px 8px; border: 1px solid #cfcabb;
    border-radius: 6px; font-size: 13px; box-sizing: border-box; background: #fff;
  }
  .secret-row { display: flex; gap: 4px; }
  .secret-row .field { flex: 1; }
  .toggle-btn {
    border: 1px solid #cfcabb; border-radius: 6px;
    background: #efece2; cursor: pointer; padding: 0 8px; font-size: 13px;
  }
  .error { color: #c0392b; font-size: 12px; margin: 4px 0 0; }
  .actions { display: flex; gap: 8px; align-items: center; margin-top: 16px; }
  .spacer { flex: 1; }
  .btn-save, .btn-cancel, .btn-clear {
    padding: 6px 14px; border: 1px solid #cfcabb; border-radius: 6px;
    background: #efece2; cursor: pointer; font-size: 13px;
  }
  .btn-save { background: #4a6fa5; color: white; border-color: #3d5c8c; }
  .btn-save:disabled, .btn-cancel:disabled, .btn-clear:disabled {
    opacity: 0.6; cursor: not-allowed;
  }
  .section { margin-bottom: 16px; }
  .toggle-row {
    display: flex; align-items: center; gap: 8px;
    font-size: 13px; color: #333; margin-bottom: 6px; cursor: pointer;
  }
  .toggle-row input { margin: 0; }
  .log-path {
    display: inline-block; margin-top: 4px;
    font-size: 11px; color: #777; word-break: break-all;
  }
  .log-size { font-size: 11px; color: #999; }
  .log-actions { display: flex; gap: 14px; margin-top: 2px; }
</style>
