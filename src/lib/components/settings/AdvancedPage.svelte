<script lang="ts">
  // Settings › Advanced: Google OAuth credentials, the Microsoft OAuth client
  // (desktop only) and Diagnostics. Moved verbatim from the old
  // AppSettings.svelte. The two OAuth fields are this page's one form (page
  // Save); Diagnostics applies at once. "Remove credentials" replaces the old
  // footer Clear, behind ConfirmDialog — window.confirm() is dropped by
  // WKWebView.
  import { onMount, tick } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { isAndroid } from '../../stores/platform';
  import Icon from '../Icon.svelte';
  import ConfirmDialog from '../ConfirmDialog.svelte';
  import SaveError from './SaveError.svelte';
  import type { PageSaveError, SectionForm } from '../../settings/sectionForm';

  let { sub = null, saveError = null }: { sub?: string | null; saveError?: PageSaveError } = $props();

  const DOCS_URL = 'https://developers.google.com/identity/protocols/oauth2/native-app';

  let clientId = $state('');
  let clientSecret = $state('');
  // The client_id as last persisted. A secret belongs to exactly one client_id,
  // so "leave blank to keep the stored secret" is only valid while the id in the
  // field still matches this one.
  let savedClientId = $state('');
  let showSecret = $state(false);
  let saving = $state(false);
  let errorMsg = $state('');
  let confirmRemove = $state(false);
  let clientIdInput: HTMLInputElement | undefined = $state();

  const MS_DOCS_URL =
    'https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade';

  // Microsoft has no secret half — it is a public client, so there is only an
  // id to store and no keychain entry, no "leave blank to keep" rule, and no
  // equivalent of `secretRequired`.
  let msClientId = $state('');
  let msSavedClientId = $state('');

  let fileLoggingEnabled = $state(true);
  let logFilePath = $state('');
  let logFileSizeBytes = $state(0);
  let clearingLog = $state(false);
  let logSettingsError = $state('');

  onMount(() => {
    void loadConfig().then(async () => {
      // `advanced/oauth` (AuthScreen's "Configure first"): land on the field.
      if (sub === 'oauth') {
        await tick();
        clientIdInput?.focus();
      }
    });
    void loadLogSettings();
  });

  async function loadConfig() {
    errorMsg = '';
    clientSecret = '';
    try {
      const cfg = await invoke<{ client_id: string }>('get_oauth_settings');
      clientId = cfg.client_id;
      savedClientId = cfg.client_id;
      // Returns only what the user configured, never the resolved value — an
      // embedded or env-var id must leave this field empty, or the user cannot
      // tell "I set this" from "the build came with one".
      const ms = await invoke<{ client_id: string; credentials_available: boolean }>('get_ms_oauth_config');
      msClientId = ms.client_id;
      msSavedClientId = ms.client_id;
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
  // page Save) — a logging toggle has no "draft" state worth staging.
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
  // — this only turns a failed save into an explanation before it is sent.
  const secretRequired = $derived(
    clientId.trim().length > 0 &&
      clientSecret.trim().length === 0 &&
      !(savedClientId && clientId.trim() === savedClientId)
  );
  const secretWarning = $derived(
    savedClientId
      ? 'The stored secret no longer matches this Client ID. Paste the secret issued for this Client ID.'
      : 'Paste the secret issued for this Client ID — Google requires it even with PKCE.',
  );

  const oauthForm: SectionForm = {
    isDirty: () =>
      msClientId.trim() !== msSavedClientId ||
      clientId.trim() !== savedClientId ||
      clientSecret.trim() !== '',
    save: saveOauth,
  };

  /** The section-form contract (lib/settings/sectionForm.ts): one form, both OAuth clients. */
  export function forms(): SectionForm[] {
    return [oauthForm];
  }

  async function saveOauth(): Promise<void> {
    if (saving) throw new Error('Another save is in progress.');
    saving = true;
    errorMsg = '';
    try {
      // Microsoft first, and deliberately before the Google guard below, so
      // a Google form left mid-edit cannot swallow a Microsoft edit silently.
      // An empty field clears the stored id, dropping back to the embedded one.
      if (msClientId.trim() !== msSavedClientId) {
        await invoke('save_ms_oauth_config', { clientId: msClientId.trim() });
        msSavedClientId = msClientId.trim();
      }
      // The warning is already on screen beside the field.
      if (secretRequired) throw new Error(secretWarning);
      if (clientId.trim() !== savedClientId || clientSecret.trim()) {
        await invoke('save_oauth_config', { clientId: clientId.trim(), clientSecret });
      }
      savedClientId = clientId.trim();
      clientSecret = '';
    } catch (e) {
      // Shown once, by the page above this form (SaveError).
      throw e instanceof Error ? e : new Error(String(e));
    } finally {
      saving = false;
    }
  }

  async function removeCreds() {
    confirmRemove = false;
    saving = true;
    errorMsg = '';
    try {
      await invoke('clear_oauth_config');
      clientId = '';
      savedClientId = '';
      clientSecret = '';
    } catch (e) {
      errorMsg = String(e);
    } finally {
      saving = false;
    }
  }
</script>

{#if confirmRemove}
  <ConfirmDialog
    title="Remove saved credentials?"
    message="Gmail sync will stop working until you re-configure."
    confirmLabel="Remove credentials"
    destructive
    onConfirm={removeCreds}
    onCancel={() => { confirmRemove = false; }}
  />
{/if}

<SaveError error={saveError} index={0} />

<section class="section" id="settings-oauth">
  <h4>Google OAuth Credentials</h4>
  <!-- Shown on Android too. The client TYPE differs because the redirect
       does: Android's is an https App Links URL, which only a Web
       application client may declare, while a Desktop client is limited
       to http://localhost. Both are honored by auth::client_id(), which
       checks a stored override before it looks at the platform. -->
  <p class="help">
    Required for Gmail sync. Create a
    <strong>{$isAndroid ? 'Web application' : 'Desktop application'}</strong> OAuth client in
    <button class="link-btn" onclick={() => openUrl(DOCS_URL)}>Google Cloud Console</button>.
  </p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>Paste the Client ID and Secret below.</p>
  </details>
  {#if $isAndroid}
    <p class="help">
      On the Web client, authorize this exact redirect URI:
      <code>https://jodd.bbmedia.co.th/oauth2redirect</code>
    </p>
  {/if}

  <label>
    <span class="lbl">Client ID</span>
    <input
      bind:this={clientIdInput}
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
        placeholder={savedClientId && clientId.trim() === savedClientId
          ? '(leave blank to keep any existing secret)'
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

  <!-- Once a Save has been refused for it, the page's error line above says
       it; one surface per failure. -->
  {#if secretRequired && saveError?.message !== secretWarning}
    <p class="help warn">{secretWarning}</p>
  {/if}

  {#if errorMsg}
    <p class="error">{errorMsg}</p>
  {/if}

  <button class="btn-danger" onclick={() => { confirmRemove = true; }} disabled={saving}>Remove credentials</button>
</section>

<!-- Desktop only, as a developer setting. Microsoft sign-in itself works on
     Android (it redirects through the same App Links URL as Gmail, and
     `backend_kind_for_signin` no longer refuses it), and a configured id
     would be honoured there. But a bring-your-own registration would also
     have to declare that App Links redirect, and release builds embed a
     client that already does, so the field stays off the phone. -->
{#if !$isAndroid}
  <section class="section">
    <h4>Microsoft OAuth Client</h4>
    <p class="help">
      Optional. Release builds already include one; set this only to use your
      own app registration.
    </p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>
        Create it in
        <button class="link-btn" onclick={() => openUrl(MS_DOCS_URL)}>Azure app registrations</button>.
        There is no secret: Microsoft sign-in uses a public client.
      </p>
      <p>
        One client is used for <strong>every</strong> Microsoft account here. A
        client registered inside a single organisation's tenant will not work
        for a personal <code>@outlook.com</code> account.
      </p>
    </details>

    <label>
      <span class="lbl">Client ID</span>
      <input
        class="field"
        type="text"
        bind:value={msClientId}
        placeholder="(using the built-in client)"
        autocomplete="off"
      />
    </label>
  </section>
{/if}

<section class="section">
  <h4>Diagnostics</h4>
  <label class="toggle-row">
    <input type="checkbox" checked={fileLoggingEnabled} onchange={toggleFileLogging} />
    <span>Save app logs to a file (recommended)</span>
  </label>
  <p class="help">Helps diagnose sync issues after the fact.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      The app window itself doesn't show its own diagnostic log.
      Automatically trimmed once it passes 20 MB.
    </p>
  </details>
  {#if logFilePath}
    <p class="log-file">
      <code class="log-path">{logFilePath}</code>
      <span class="log-size">({fmtBytes(logFileSizeBytes)})</span>
    </p>
  {/if}
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

<style>
  .section { margin-bottom: var(--space-4); }
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: 600; }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
  .help.warn { color: var(--danger); margin-top: var(--space-1); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .link-btn.danger { color: var(--danger); }
  label { display: block; margin-bottom: var(--space-2); }
  .lbl { display: block; font-size: var(--size-sm); color: var(--text-secondary); margin-bottom: var(--space-0-5); }
  .field {
    width: 100%; padding: var(--space-1-5) var(--space-2); border: 1px solid var(--border-strong);
    border-radius: var(--radius); font-size: var(--size); box-sizing: border-box;
    background: var(--surface-panel); color: var(--text);
  }
  .secret-row { display: flex; gap: var(--space-1); }
  .secret-row .field { flex: 1; }
  .toggle-btn {
    display: inline-flex; align-items: center; justify-content: center;
    border: 1px solid var(--border-strong); border-radius: var(--radius);
    background: var(--surface-sidebar); cursor: pointer; padding: 0 var(--space-2); font-size: var(--size);
  }
  /* Destructive, so never the primary: outlined in danger, not filled. */
  .btn-danger {
    margin-top: var(--space-1);
    padding: var(--space-1-5) var(--space-3);
    border: 1px solid var(--danger); border-radius: var(--radius);
    background: var(--surface-panel); color: var(--danger);
    font: inherit; font-size: var(--size-sm); cursor: pointer;
  }
  .btn-danger:disabled { opacity: 0.6; cursor: not-allowed; }
  .error { color: var(--danger); font-size: var(--size-sm); margin: var(--space-1) 0 0; }
  .toggle-row {
    display: flex; align-items: center; gap: var(--space-2);
    font-size: var(--size); color: var(--text); margin-bottom: var(--space-1-5); cursor: pointer;
  }
  .toggle-row input { margin: 0; }
  .log-path {
    display: inline-block; margin-top: var(--space-1);
    font-size: var(--size-xs); color: var(--text-muted); word-break: break-all;
  }
  .log-file { margin: 0 0 var(--space-3); line-height: var(--leading); }
  .log-size { font-size: var(--size-xs); color: var(--text-muted); }
  .log-actions { display: flex; gap: var(--space-3); margin-top: var(--space-0-5); }
</style>
