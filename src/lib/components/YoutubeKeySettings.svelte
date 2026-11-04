<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { onMount } from 'svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';

  // Settings › Links: the optional YouTube Data API key that lets ingest read
  // a video's public comments (spec 2026-10-28 §6.2). No Save button of its
  // own: the page's one Save calls save() (lib/settings/sectionForm.ts).
  // Whether a key is saved comes from a flag file, never the keychain
  // (gotcha #15); the key itself is only ever written, never shown.

  let hasKey = $state<boolean | null>(null);
  let key = $state('');
  let showKey = $state(false);
  let confirmDelete = $state(false);
  let busy = $state(false);
  let msg = $state('');

  onMount(async () => {
    try {
      hasKey = await invoke<boolean | null>('get_youtube_key_status');
    } catch (e) {
      msg = `load: ${e}`;
    }
  });

  /** Section-form contract. */
  export function isDirty(): boolean {
    return key.trim() !== '';
  }

  export async function save(): Promise<void> {
    if (!isDirty()) return;
    busy = true;
    msg = '';
    try {
      await invoke('set_youtube_key', { key: key.trim() });
      key = '';
      hasKey = true;
    } catch (e) {
      msg = String(e);
      throw new Error(msg);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    confirmDelete = false;
    busy = true;
    msg = '';
    try {
      await invoke('clear_youtube_key');
      hasKey = false;
    } catch (e) {
      msg = String(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if confirmDelete}
  <ConfirmDialog
    title="Delete YouTube key?"
    message="Delete the saved YouTube Data API key from the credential store? YouTube links will be read without their comments."
    confirmLabel="Delete key"
    destructive
    onConfirm={remove}
    onCancel={() => { confirmDelete = false; }}
  />
{/if}

<div class="yt-key">
  <p role="status" data-testid="youtube-key-status">
    YouTube comments: {hasKey === null ? 'not set up' : hasKey ? 'a key is saved' : 'no key saved'}.
  </p>
  <label>
    YouTube Data API key
    <input
      type={showKey ? 'text' : 'password'}
      bind:value={key}
      autocomplete="new-password"
      placeholder={hasKey ? 'Leave blank to keep the saved key' : 'AIza…'}
      disabled={busy}
    />
  </label>
  <button onclick={() => { showKey = !showKey; }} aria-pressed={showKey}>{showKey ? 'Hide key' : 'Show key'}</button>
  {#if hasKey}
    <button class="link-btn" onclick={() => { confirmDelete = true; }} disabled={busy}>Delete key</button>
  {/if}
  <p class="help">Saving or deleting a key may ask for permission to access your credential store on macOS.</p>
  {#if msg}<p class="msg" role="alert">{msg}</p>{/if}
</div>

<style>
  .yt-key { padding: 12px 0; }
  label { display: block; margin: 8px 0; font-size: var(--size); }
  input { width: 100%; padding: 6px; font: inherit; box-sizing: border-box; margin-top: 4px; background: var(--surface-panel); color: var(--text); }
  button { padding: 8px 16px; font: inherit; }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 4px 0 0; }
  .link-btn {
    background: none; border: none; padding: 0; margin-left: 8px;
    color: var(--accent-action); font: inherit; font-size: var(--size-sm);
    text-decoration: underline; cursor: pointer;
  }
  .link-btn:disabled { opacity: 0.5; cursor: not-allowed; text-decoration: none; }
  .msg { font-size: var(--size-sm); color: var(--danger); }
</style>
