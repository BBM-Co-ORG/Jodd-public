<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { accounts, activeAccounts } from '../stores/notes';
  import LlmProviderSettings from './LlmProviderSettings.svelte';

  // Account whose settings we're editing. Passed in by Sidebar when the
  // user clicks the gear icon on a row.
  export let accountId: string;
  export let accountEmail: string;
  export let backendKind: string | undefined = undefined;
  export let onClose: () => void;

  $: isLocalFs = backendKind === 'local_fs';

  // The two configurable labels (notes root + metadata root). Loaded
  // from the backend on mount via get_account_settings — the backend
  // returns the effective values (defaults resolved), so we always have
  // concrete strings to render. Edits go back via update_account_settings.
  let notesLabel = '';
  let metaLabel = '';
  let loaded = false;
  let saving = false;
  let error: string | null = null;

  // Vault name — only for LocalFs accounts. Pre-filled with the current
  // account email (which holds the vault display name). Saved via
  // rename_local_account and updates the accounts store optimistically.
  let vaultName = '';
  let rootDir: string | null = null;

  // The Rust constants — duplicated here as placeholder text so the user
  // can see "what does empty mean" before they save. Keep in sync with
  // accounts.rs DEFAULT_NOTES_LABEL / DEFAULT_META_LABEL.
  const DEFAULT_NOTES_LABEL = 'Notes';
  const DEFAULT_META_LABEL = 'Notes-Meta';

  onMount(async () => {
    try {
      if (isLocalFs) {
        // For LocalFs accounts, derive vaultName from the current account email
        // (which stores the vault display name). No Gmail label settings needed.
        const acctList = $activeAccounts;
        const a = acctList.find((x) => x.id === accountId);
        vaultName = a?.email ?? '';
        rootDir = a?.root_dir ?? null;
      } else {
        const cur = await invoke<{ notes_label: string; meta_label: string }>(
          'get_account_settings',
          { accountId },
        );
        notesLabel = cur.notes_label;
        metaLabel = cur.meta_label;
      }
    } catch (e) {
      console.error('get_account_settings failed', e);
      error = String(e);
    } finally {
      loaded = true;
    }
  });

  async function save() {
    saving = true;
    error = null;
    try {
      if (isLocalFs) {
        // Rename the vault: update the display name (email field) on the account.
        const updated = await invoke<{ id: string; email: string; added_at: string; backend_kind: string; root_dir: string | null }>(
          'rename_local_account',
          { accountId, name: vaultName.trim() || vaultName },
        );
        // Update the accounts store so the sidebar re-renders immediately.
        accounts.update((list) =>
          list.map((a) => a.id === accountId ? { ...a, email: updated.email } : a),
        );
        onClose();
      } else {
        // Pass exactly what's in the inputs. Empty string is meaningful on
        // the Rust side — it resets the field to the DEFAULT_ constant.
        const updated = await invoke<{ notes_label: string; meta_label: string }>(
          'update_account_settings',
          { accountId, notesLabel, metaLabel },
        );
        notesLabel = updated.notes_label;
        metaLabel = updated.meta_label;
        onClose();
      }
    } catch (e) {
      console.error('save settings failed', e);
      error = String(e);
    } finally {
      saving = false;
    }
  }

  // No confirmation: deactivating loses nothing (the account keeps flushing
  // its already-queued work in the background), so there is nothing to
  // confirm. Optimistic-first per the local-first doctrine: flip the
  // in-memory `accounts` store's status BEFORE the invoke resolves, so the
  // account drops out of $activeAccounts (and disappears from the sidebar)
  // at once rather than after a round trip. Roll back on failure.
  //
  // Shares the `saving` flag with save()/Cancel so all three buttons disable
  // together — without it, a double-click fires two set_account_status
  // invokes, and a click here during an in-flight save() interleaves the two.
  async function deactivate() {
    saving = true;
    error = null;
    const snapshot = get(accounts);
    accounts.update((list) =>
      list.map((a) => (a.id === accountId ? { ...a, status: 'draining' } : a)),
    );
    try {
      await invoke('set_account_status', { accountId, status: 'draining' });
      onClose();
    } catch (e) {
      accounts.set(snapshot);
      console.error('set_account_status failed', e);
      error = String(e);
    } finally {
      saving = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose();
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) save();
  }
</script>

<svelte:window on:keydown={onKey} />

<div
  class="settings-overlay"
  role="presentation"
  onclick={(e) => { if (e.target === e.currentTarget) onClose(); }}
>
  <div class="settings-modal" role="dialog" aria-modal="true" aria-labelledby="settings-title">
    <h2 id="settings-title">Account settings</h2>
    <div class="account-line">{accountEmail}</div>

    {#if !loaded}
      <div class="loading">Loading…</div>
    {:else if isLocalFs}
      <!-- LocalFs: only show vault name field (no Gmail labels) -->
      <label class="field">
        <span class="field-label">Vault name</span>
        <input
          type="text"
          bind:value={vaultName}
          placeholder="My Notes"
          spellcheck="false"
          autocomplete="off"
        />
        <span class="field-hint">
          Display name shown in the sidebar as <code>localfs:&lt;name&gt;</code>.
          Changing this only affects how Jodd labels this folder — the folder
          on disk is not renamed.
        </span>
      </label>

      {#if rootDir}
        <div class="field">
          <span class="field-label">Folder on disk</span>
          <div class="root-dir-display">{rootDir}</div>
        </div>
      {/if}

      <hr />
      <LlmProviderSettings accountId={accountId} />

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <div class="actions">
        <button class="cancel" onclick={onClose} disabled={saving}>Cancel</button>
        <button class="save" onclick={save} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    {:else}
      <!-- Gmail account: show full label config -->
      <label class="field">
        <span class="field-label">Notes label</span>
        <input
          type="text"
          bind:value={notesLabel}
          placeholder={DEFAULT_NOTES_LABEL}
          spellcheck="false"
          autocomplete="off"
        />
        <span class="field-hint">
          Gmail label this account's notes live under. Strongly recommend
          keeping <code>Notes</code> — Apple Notes only writes to this label,
          and changing it loses iPhone/Mac interop. Empty resets to default.
        </span>
      </label>

      <label class="field">
        <span class="field-label">Metadata label</span>
        <input
          type="text"
          bind:value={metaLabel}
          placeholder={DEFAULT_META_LABEL}
          spellcheck="false"
          autocomplete="off"
        />
        <span class="field-hint">
          Top-level Gmail label where Jodd stores per-note metadata
          (pin state, etc.) as small sidecar messages. Apple Notes ignores
          this label entirely. Multiple Jodd installs on the same Gmail
          account share pins through this label — use the same value on
          every Jodd install of this account. Empty resets to default.
        </span>
      </label>

      <hr />
      <LlmProviderSettings accountId={accountId} />

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <div class="actions">
        <button class="cancel" onclick={onClose} disabled={saving}>Cancel</button>
        <button class="save" onclick={save} disabled={saving}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    {/if}

    <hr />
    <button class="deactivate" onclick={deactivate} disabled={saving}>
      {saving ? 'Deactivating…' : 'Deactivate this account'}
    </button>
    <p class="hint">
      Hides it everywhere and stops background sync. Anything already queued still
      finishes sending first. Nothing is deleted — reactivate from the Inactive
      group in the account list.
    </p>
  </div>
</div>

<style>
  .settings-overlay {
    position: fixed;
    inset: 0;
    background: var(--scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1500;
  }
  .settings-modal {
    background: var(--surface-panel);
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
    padding: 22px 24px 18px;
    width: 460px;
    max-width: 92vw;
    max-height: 92vh;
    overflow-y: auto;
    font-size: var(--size);
    color: var(--text);
  }
  h2 {
    font-size: var(--size-lg);
    font-weight: 600;
    margin: 0 0 4px;
  }
  .account-line {
    color: var(--text-muted);
    font-size: var(--size-sm);
    line-height: var(--leading);
    margin-bottom: 16px;
  }
  .loading {
    color: var(--text-muted);
    padding: 24px 0;
    text-align: center;
  }
  .field {
    display: block;
    margin-bottom: 16px;
  }
  .field-label {
    display: block;
    font-weight: 600;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    margin-bottom: 6px;
  }
  input[type="text"] {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    border: 1px solid var(--border-strong);
    border-radius: 6px;
    font-size: var(--size);
    line-height: var(--leading);
    font-family: inherit;
    background: var(--surface-editor);
    color: var(--text);
  }
  input[type="text"]:focus {
    outline: none;
    border-color: var(--focus);
    background: var(--surface-panel);
  }
  .field-hint {
    display: block;
    margin-top: 6px;
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
  }
  .root-dir-display {
    font-family: var(--font-mono);
    font-size: var(--size-xs);
    color: var(--text-secondary);
    background: var(--surface-sunken);
    border: 1px solid var(--border-subtle);
    border-radius: 5px;
    padding: 6px 8px;
    word-break: break-all;
    line-height: var(--leading);
  }
  .field-hint code {
    background: var(--surface-hover);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: var(--size-xs);
  }
  .error {
    background: var(--danger-wash);
    color: var(--danger);
    padding: 8px 10px;
    border-radius: 6px;
    font-size: var(--size-sm);
    margin-bottom: 12px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  .cancel, .save {
    padding: 7px 14px;
    border-radius: 6px;
    border: 1px solid transparent;
    cursor: pointer;
    font-size: var(--size);
    font-family: inherit;
  }
  .cancel {
    background: none;
    color: var(--text-secondary);
    border-color: var(--border);
  }
  .cancel:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .save {
    background: var(--accent-action);
    color: var(--text-inverse);
  }
  .save:hover:not(:disabled) {
    background: var(--accent-hover);
  }
  .save:disabled, .cancel:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .deactivate {
    width: 100%;
    padding: 7px 14px;
    border-radius: 6px;
    border: 1px solid var(--border);
    background: none;
    color: var(--text-secondary);
    cursor: pointer;
    font-size: var(--size);
    font-family: inherit;
  }
  .deactivate:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .deactivate:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
  .hint {
    margin: 8px 0 0;
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
  }
</style>
