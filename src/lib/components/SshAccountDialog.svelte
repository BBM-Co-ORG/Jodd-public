<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import type { Account } from '../types';
  import { addArgs, childPath, parentPath, validateTarget } from '../sshAccountForm';

  export let onAdded: (a: Account) => void;
  export let onClose: () => void;

  let dialog: HTMLDialogElement;
  let target = '';
  let root = '';
  let name = '';
  let create = false;
  let busy = false;
  let error = '';
  let browsing: { path: string; dirs: string[] } | null = null;

  onMount(() => {
    dialog.showModal();
    return () => dialog.close();
  });

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

  function onKey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape') { e.preventDefault(); onClose(); }
  }
</script>

<dialog
  bind:this={dialog}
  class="prompt-overlay"
  aria-modal="true"
  aria-label="Add SSH server"
  oncancel={(e) => { e.preventDefault(); onClose(); }}
  onkeydown={onKey}
>
  <div class="prompt-dialog ssh-dialog">
    <div class="prompt-title">Add SSH server</div>
    <p class="confirm-message">
      Jodd connects with your own <code>ssh</code> and its keys (ssh-agent or ~/.ssh/config). It stores no password.
      Notes are plain files on the server: anyone who can read that directory can read them.
    </p>
    <label class="ssh-field">Server
      <input bind:value={target} placeholder="me@host or an ~/.ssh/config alias" autocomplete="off" spellcheck="false" />
    </label>
    <label class="ssh-field">Folder on the server
      <span class="ssh-row">
        <input bind:value={root} placeholder="~/notes" autocomplete="off" spellcheck="false" />
        <button type="button" class="prompt-btn" disabled={busy} onclick={() => browse(root || '~')}>Browse…</button>
      </span>
    </label>
    {#if browsing}
      <div class="ssh-browser" role="listbox" aria-label="Folders on the server">
        <div class="ssh-row">
          <code class="ssh-path">{browsing.path}</code>
          <button type="button" class="prompt-btn" disabled={busy} onclick={() => browse(parentPath(browsing!.path))}>Up</button>
          <button type="button" class="prompt-btn primary" onclick={() => { root = browsing!.path; browsing = null; }}>Use this folder</button>
        </div>
        {#each browsing.dirs as dir}
          <button type="button" class="ssh-dir" role="option" aria-selected="false" disabled={busy} onclick={() => browse(childPath(browsing!.path, dir))}>{dir}/</button>
        {:else}
          <div class="confirm-message">No sub-folders.</div>
        {/each}
      </div>
    {/if}
    <label class="ssh-check"><input type="checkbox" bind:checked={create} /> Create this directory if it does not exist</label>
    <label class="ssh-field">Name in the sidebar (optional)
      <input bind:value={name} placeholder="defaults to server:folder" />
    </label>
    {#if error}<div class="ssh-error" role="alert">{error}</div>{/if}
    <div class="prompt-actions">
      <button type="button" class="prompt-btn" onclick={onClose}>Cancel</button>
      <button type="button" class="prompt-btn primary" disabled={busy} onclick={submit}>{busy ? 'Connecting…' : 'Add'}</button>
    </div>
  </div>
</dialog>

<style>
  .prompt-overlay {
    position: fixed;
    inset: 0;
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    box-sizing: border-box;
    background: var(--scrim);
  }

  .prompt-overlay::backdrop {
    background: transparent; /* The full-viewport overlay already draws the scrim. */
  }

  .prompt-overlay[open] {
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 2000;
  }

  .prompt-dialog {
    background: var(--surface-panel);
    min-width: min(320px, 80vw);
    box-sizing: border-box;
    max-width: 80vw;
    /* designGuards.test.ts "modal scrolling": every role="dialog" component
       needs a bound + a scroller somewhere in its own stylesheet. Message
       text here is normally one or two lines, but nothing enforces that at
       the call site (a future caller could pass something long), and the
       overlay's fixed/inset:0 centering means an overgrown panel would
       overflow at both ends with no way to reach the buttons. */
    max-height: 80vh;
    overflow-y: auto;
    padding: 18px 20px 14px;
    border-radius: 10px;
    box-shadow: var(--shadow-modal);
  }

  .prompt-title {
    font-size: var(--size);
    font-weight: 600;
    color: var(--text);
    line-height: var(--leading);
    margin-bottom: 10px;
  }

  .confirm-message {
    font-size: var(--size);
    color: var(--text-secondary);
    line-height: var(--leading);
  }

  .prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }

  .prompt-btn {
    padding: 5px 14px;
    font-size: var(--size-sm);
    border-radius: 5px;
    border: 1px solid var(--border);
    background: var(--surface-panel);
    color: var(--text);
    cursor: pointer;
  }

  .prompt-btn:hover {
    background: var(--surface-list);
  }

  .prompt-btn.primary {
    background: var(--accent-action);
    color: var(--text-inverse);
    border-color: var(--accent-action);
  }

  .prompt-btn.primary:hover {
    background: var(--accent-hover);
  }

  .prompt-btn.danger {
    background: var(--danger);
    color: var(--text-inverse);
    border-color: var(--danger);
  }

  .prompt-btn.danger:hover {
    filter: brightness(1.1);
  }

  .ssh-dialog { min-width: min(460px, 90vw); }
  .ssh-field { display: flex; flex-direction: column; gap: 4px; margin-top: 10px; font-size: var(--size); color: var(--text); }
  .ssh-field input { font: inherit; padding: 5px 7px; border-radius: 6px; border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); }
  .ssh-row { display: flex; gap: 6px; align-items: center; }
  .ssh-row input { flex: 1; }
  .ssh-browser { margin-top: 8px; max-height: 200px; overflow-y: auto; border: 1px solid var(--border); border-radius: 6px; padding: 6px; }
  .ssh-path { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ssh-dir { display: block; width: 100%; text-align: left; background: none; border: 0; padding: 3px 4px; color: var(--text); font: inherit; cursor: pointer; }
  .ssh-dir:hover { background: var(--surface-hover); }
  .ssh-check { display: flex; gap: 6px; align-items: center; margin-top: 10px; font-size: var(--size); color: var(--text-secondary); }
  .ssh-error { margin-top: 10px; color: var(--danger); font-size: var(--size); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
