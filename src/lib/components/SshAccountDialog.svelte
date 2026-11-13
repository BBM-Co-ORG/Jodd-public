<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import type { Account } from '../types';
  import Modal from './Modal.svelte';
  import { addArgs, childPath, parentPath, validateTarget } from '../sshAccountForm';

  export let onAdded: (a: Account) => void;
  export let onClose: () => void;

  let target = '';
  let root = '';
  let name = '';
  let create = false;
  let busy = false;
  let error = '';
  let browsing: { path: string; dirs: string[] } | null = null;

  async function browse(path: string) {
    const bad = validateTarget(target);
    if (bad) { error = bad; return; }
    busy = true; error = '';
    try {
      browsing = await invoke<{ path: string; dirs: string[] }>('list_remote_dirs', { target: target.trim(), path });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function submit() {
    const bad = validateTarget(target);
    if (bad) { error = bad; return; }
    if (!root.trim()) { error = 'Choose a directory on the server.'; return; }
    busy = true; error = '';
    try {
      const account = await invoke<Account>('add_ssh_account', addArgs({ target, root, name, create }));
      onAdded(account);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<Modal
  title="Add SSH server"
  size="md"
  onclose={onClose}
  primary={{ label: busy ? 'Connecting…' : 'Add', onclick: submit, disabled: busy }}
>
  <p class="note">
    Jodd connects with your own <code>ssh</code> and its keys (ssh-agent or ~/.ssh/config). It stores no password.
    Notes are plain files on the server: anyone who can read that directory can read them.
  </p>
  <label class="ssh-field">Server
    <input data-autofocus bind:value={target} placeholder="me@host or an ~/.ssh/config alias" autocomplete="off" spellcheck="false" />
  </label>
  <label class="ssh-field">Folder on the server
    <span class="ssh-row">
      <input bind:value={root} placeholder="~/notes" autocomplete="off" spellcheck="false" />
      <button type="button" class="ssh-btn" disabled={busy} onclick={() => browse(root || '~')}>Browse…</button>
    </span>
  </label>
  {#if browsing}
    <div class="ssh-browser" role="listbox" aria-label="Folders on the server">
      <div class="ssh-row">
        <code class="ssh-path">{browsing.path}</code>
        <button type="button" class="ssh-btn" disabled={busy} onclick={() => browse(parentPath(browsing!.path))}>Up</button>
        <button type="button" class="ssh-btn" onclick={() => { root = browsing!.path; browsing = null; }}>Use this folder</button>
      </div>
      {#each browsing.dirs as dir}
        <button type="button" class="ssh-dir" role="option" aria-selected="false" disabled={busy} onclick={() => browse(childPath(browsing!.path, dir))}>{dir}/</button>
      {:else}
        <div class="note">No sub-folders.</div>
      {/each}
    </div>
  {/if}
  <label class="ssh-check"><input type="checkbox" bind:checked={create} /> Create this directory if it does not exist</label>
  <label class="ssh-field">Name in the sidebar (optional)
    <input bind:value={name} placeholder="defaults to server:folder" />
  </label>
  {#if error}<div class="ssh-error" role="alert">{error}</div>{/if}
</Modal>

<style>
  .note { margin: 0; font-size: var(--size); color: var(--text-secondary); line-height: var(--leading); }
  .ssh-field { display: flex; flex-direction: column; gap: var(--space-1); margin-top: var(--space-3); font-size: var(--size); color: var(--text); }
  .ssh-field input { font: inherit; padding: var(--space-1) var(--space-2); border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); }
  .ssh-row { display: flex; gap: var(--space-1-5); align-items: center; }
  .ssh-row input { flex: 1; }
  .ssh-btn { padding: var(--space-1) var(--space-3); font-size: var(--size-sm); border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); cursor: pointer; }
  .ssh-btn:hover:not(:disabled) { background: var(--surface-list); }
  .ssh-btn:disabled { opacity: 0.5; cursor: default; }
  .ssh-browser { margin-top: var(--space-2); max-height: 200px; overflow-y: auto; border: 1px solid var(--border); border-radius: var(--radius); padding: var(--space-1-5); }
  .ssh-path { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ssh-dir { display: block; width: 100%; text-align: left; background: none; border: 0; padding: var(--space-0-5) var(--space-1); color: var(--text); font: inherit; cursor: pointer; }
  .ssh-dir:hover { background: var(--surface-hover); }
  .ssh-check { display: flex; gap: var(--space-1-5); align-items: center; margin-top: var(--space-3); font-size: var(--size); color: var(--text-secondary); }
  .ssh-error { margin-top: var(--space-3); color: var(--danger); font-size: var(--size); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
