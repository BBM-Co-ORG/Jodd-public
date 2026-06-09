<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';

  // Account whose settings we're editing. Passed in by Sidebar when the
  // user clicks the gear icon on a row.
  export let accountId: string;
  export let accountEmail: string;
  export let onClose: () => void;

  // The two configurable labels (notes root + metadata root). Loaded
  // from the backend on mount via get_account_settings — the backend
  // returns the effective values (defaults resolved), so we always have
  // concrete strings to render. Edits go back via update_account_settings.
  let notesLabel = '';
  let metaLabel = '';
  let loaded = false;
  let saving = false;
  let error: string | null = null;

  // The Rust constants — duplicated here as placeholder text so the user
  // can see "what does empty mean" before they save. Keep in sync with
  // accounts.rs DEFAULT_NOTES_LABEL / DEFAULT_META_LABEL.
  const DEFAULT_NOTES_LABEL = 'Notes';
  const DEFAULT_META_LABEL = 'Notes-Meta';

  onMount(async () => {
    try {
      const cur = await invoke<{ notes_label: string; meta_label: string }>(
        'get_account_settings',
        { accountId },
      );
      notesLabel = cur.notes_label;
      metaLabel = cur.meta_label;
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
      // Pass exactly what's in the inputs. Empty string is meaningful on
      // the Rust side — it resets the field to the DEFAULT_ constant.
      const updated = await invoke<{ notes_label: string; meta_label: string }>(
        'update_account_settings',
        { accountId, notesLabel, metaLabel },
      );
      notesLabel = updated.notes_label;
      metaLabel = updated.meta_label;
      onClose();
    } catch (e) {
      console.error('update_account_settings failed', e);
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
    {:else}
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
  </div>
</div>

<style>
  .settings-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1500;
  }
  .settings-modal {
    background: white;
    border-radius: 10px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.28);
    padding: 22px 24px 18px;
    width: 460px;
    max-width: 92vw;
    max-height: 92vh;
    overflow-y: auto;
    font-size: 13px;
    color: #222;
  }
  h2 {
    font-size: 16px;
    font-weight: 600;
    margin: 0 0 4px;
  }
  .account-line {
    color: #888;
    font-size: 12px;
    margin-bottom: 16px;
  }
  .loading {
    color: #888;
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
    font-size: 12px;
    color: #444;
    margin-bottom: 6px;
  }
  input[type="text"] {
    width: 100%;
    box-sizing: border-box;
    padding: 8px 10px;
    border: 1px solid #d4cdb9;
    border-radius: 6px;
    font-size: 13px;
    font-family: inherit;
    background: #fdfbf5;
    color: #222;
  }
  input[type="text"]:focus {
    outline: none;
    border-color: #c97c1f;
    background: white;
  }
  .field-hint {
    display: block;
    margin-top: 6px;
    font-size: 11px;
    color: #888;
    line-height: 1.45;
  }
  .field-hint code {
    background: rgba(0, 0, 0, 0.06);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 11px;
  }
  .error {
    background: rgba(192, 57, 43, 0.08);
    color: #c0392b;
    padding: 8px 10px;
    border-radius: 6px;
    font-size: 12px;
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
    font-size: 13px;
    font-family: inherit;
  }
  .cancel {
    background: none;
    color: #555;
    border-color: rgba(0, 0, 0, 0.12);
  }
  .cancel:hover:not(:disabled) {
    background: rgba(0, 0, 0, 0.05);
  }
  .save {
    background: #c97c1f;
    color: white;
  }
  .save:hover:not(:disabled) {
    background: #b06a17;
  }
  .save:disabled, .cancel:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }
</style>
