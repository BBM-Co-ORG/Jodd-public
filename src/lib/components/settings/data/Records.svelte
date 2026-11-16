<script lang="ts">
  // Settings › Privacy & Data › Records on this device (spec 2026-11-11 §4): what
  // Jodd keeps about your AI use and your app, and the controls over it.
  // Ask Jodd history moved from General and the app log from Advanced ›
  // Diagnostics, logic unchanged; what each keeps is a visible line (E10, E11).
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { revealItemInDir } from '@tauri-apps/plugin-opener';
  import { isAndroid } from '../../../stores/platform';
  import { copyText } from '../../../clipboard';
  import AiReceipts from '../../AiReceipts.svelte';
  import { ASK_HISTORY_KEEPS, LOGS_DEFAULT_ON, LOGS_HOLD, LOGS_TRIMMED } from '../../../settings/governanceCopy';

  // ── Ask Jodd history (from GeneralPage) ──────────────────────────────
  let askHistoryEnabled = $state(true);
  let askHistoryPrompt = $state(false);
  let askHistoryError = $state('');

  async function loadAskHistory() {
    // A prompt or error left from the last time the page was open describes
    // a switch the user may since have flipped elsewhere.
    askHistoryPrompt = false;
    askHistoryError = '';
    try { askHistoryEnabled = (await invoke<boolean | null>('get_ask_history_enabled')) !== false; }
    catch (e) { askHistoryError = String(e); }
  }

  async function toggleAskHistory() {
    const next = !askHistoryEnabled;
    askHistoryEnabled = next; // optimistic
    askHistoryError = '';
    try {
      await invoke('set_ask_history_enabled', { enabled: next });
      askHistoryPrompt = !next;
    } catch (e) {
      askHistoryEnabled = !next; // rollback
      askHistoryError = String(e);
    }
  }

  async function clearAskHistory() {
    askHistoryPrompt = false;
    try { await invoke('clear_ask_history'); }
    catch (e) { askHistoryError = `Could not delete Ask history: ${e}`; }
  }

  // ── App log (from AdvancedPage › Diagnostics) ────────────────────────
  let fileLoggingEnabled = $state(true);
  let logFilePath = $state('');
  let logFileSizeBytes = $state(0);
  let clearingLog = $state(false);
  let logSettingsError = $state('');

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

  // @tauri-apps/plugin-opener has no Android implementation of
  // revealItemInDir, so Android copies the path instead.
  async function revealLogFile() {
    if (!logFilePath) return;
    try {
      if ($isAndroid) {
        if (await copyText(logFilePath)) logSettingsError = '';
        else logSettingsError = 'Could not copy the log path — select it above and copy it by hand.';
      } else {
        await revealItemInDir(logFilePath);
      }
    } catch (e) {
      logSettingsError = String(e);
    }
  }

  onMount(() => {
    void loadAskHistory();
    void loadLogSettings();
  });
</script>

<div class="record">
  <h5>AI receipts</h5>
  <AiReceipts history />
</div>

<div class="record">
  <h5>Ask Jodd history</h5>
  <label class="toggle-row">
    <input type="checkbox" checked={askHistoryEnabled} onchange={toggleAskHistory} />
    <span>Keep Ask Jodd history</span>
  </label>
  <p class="help">{ASK_HISTORY_KEEPS}</p>
  <!-- `!askHistoryEnabled` too: re-checking the box while the switch-off is
       still saving must take the Delete offer away at once, not when the
       second save lands. -->
  {#if askHistoryPrompt && !askHistoryEnabled}
    <div class="actions">
      <span>Also delete existing history?</span>
      <button class="link-btn" onclick={() => (askHistoryPrompt = false)}>Keep history</button>
      <button class="link-btn danger" onclick={clearAskHistory}>Delete history</button>
    </div>
  {/if}
  {#if askHistoryError}<p class="error">{askHistoryError}</p>{/if}
</div>

<div class="record">
  <h5>App logs</h5>
  <label class="toggle-row">
    <input type="checkbox" checked={fileLoggingEnabled} onchange={toggleFileLogging} />
    <span>Save app logs to a file (recommended)</span>
  </label>
  <p class="help">{LOGS_DEFAULT_ON} Helps diagnose sync issues after the fact.</p>
  <p class="help">{LOGS_HOLD}</p>
  <p class="help">{LOGS_TRIMMED}</p>
  {#if logFilePath}
    <p class="log-file">
      <code class="log-path">{logFilePath}</code>
      <span class="log-size">({fmtBytes(logFileSizeBytes)})</span>
    </p>
    <div class="actions">
      <button class="link-btn" onclick={revealLogFile}>{$isAndroid ? 'Copy log path' : 'Reveal log file'}</button>
      <button
        class="link-btn danger"
        onclick={clearLog}
        disabled={clearingLog || logFileSizeBytes === 0}
      >{clearingLog ? 'Clearing…' : 'Clear log'}</button>
    </div>
  {/if}
  {#if logSettingsError}<p class="error">{logSettingsError}</p>{/if}
</div>

<style>
  .record { display: flex; flex-direction: column; gap: var(--space-1); padding: var(--space-2) 0; border-top: 1px solid var(--border); }
  .record:first-child { border-top: 0; padding-top: 0; }
  h5 { margin: 0; font-size: var(--size); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0; line-height: var(--leading); }
  .toggle-row { display: flex; align-items: center; gap: var(--space-2); font-size: var(--size); color: var(--text); cursor: pointer; }
  .toggle-row input { margin: 0; }
  .actions { display: flex; flex-wrap: wrap; gap: var(--space-3); margin-top: var(--space-0-5); font-size: var(--size-sm); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font: inherit; font-size: var(--size-sm); text-decoration: underline;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .link-btn.danger { color: var(--danger); }
  .error { color: var(--danger); font-size: var(--size-sm); margin: var(--space-1) 0 0; }
  .log-file { margin: 0; line-height: var(--leading); }
  .log-path { display: inline-block; font-size: var(--size-xs); color: var(--text-muted); word-break: break-all; }
  .log-size { font-size: var(--size-xs); color: var(--text-muted); }
</style>
