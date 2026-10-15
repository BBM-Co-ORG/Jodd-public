<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
  import type { Account } from '../types';
  import { parseAddress, validateTarget } from '../sshAccountForm';
  import { isAndroid } from '../stores/platform';

  export let onAdded: (a: Account) => void;
  export let onClose: () => void;

  type Screen = 'connect' | 'trust' | 'progress';
  type Method = 'password' | 'keyfile';

  let dialog: HTMLDialogElement;
  let screen: Screen = 'connect';
  let address = '';
  let username = '';
  let method: Method = 'password';
  let password = '';
  let keyPath = '';
  let keyPassphrase = '';
  // One key per account, so a second vault on the same server is a second
  // setup with a different folder here (docs/BACKEND-SSH.md §8).
  let root = '~/Jodd-Notes';
  let name = '';
  let busy = false;
  let error = '';
  let fingerprint: { algorithm: string; fingerprint: string; known_hosts_line: string } | null = null;
  let step = 0; // progress screen: 0..4
  const STEPS = ['Securing the connection', "Setting up Jodd's key", 'Signing in', 'Creating your notes folder'];

  onMount(() => { dialog.showModal(); return () => dialog.close(); });

  function parsed() {
    const { host, port, user } = parseAddress(address);
    return { host, port, user: username.trim() || user || '' };
  }

  async function pickKeyFile() {
    const p = await openFileDialog({ multiple: false, title: 'Choose your key file' });
    if (typeof p === 'string') keyPath = p;
  }

  // Only host/port matter to ssh_fingerprint — username and the credential
  // are validated at trustAndConnect, right before they're actually used, so
  // a partially-filled connect screen never blocks the (cheap, reversible)
  // fingerprint lookup.
  async function fetchFingerprint() {
    const { host, port } = parsed();
    const bad = validateTarget(host);
    if (bad) { error = bad; return; }
    busy = true; error = '';
    try {
      fingerprint = await invoke('ssh_fingerprint', { host, port });
      screen = 'trust';
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function trustAndConnect() {
    if (!fingerprint) return;
    const { host, port, user } = parsed();
    if (!user) { error = 'Enter a username.'; screen = 'connect'; return; }
    if (method === 'password' && !password) { error = 'Enter a password.'; screen = 'connect'; return; }
    if (method === 'keyfile' && !keyPath) { error = 'Choose a key file.'; screen = 'connect'; return; }
    screen = 'progress';
    error = '';
    step = 0;
    const credential = method === 'password'
      ? { kind: 'Password' as const, password }
      : { kind: 'KeyFile' as const, path: keyPath, passphrase: keyPassphrase || null };
    try {
      step = 1;
      const account = await invoke<Account>('ssh_setup_managed', {
        host, port, user, credential,
        knownHostsLine: fingerprint.known_hosts_line,
        root: root.trim() || null,
        name: name.trim() || null,
      });
      step = 4;
      onAdded(account);
    } catch (e) {
      error = String(e);
      screen = 'trust';
    }
  }

  function onKey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === 'Escape' && screen !== 'progress') { e.preventDefault(); onClose(); }
  }
</script>

<dialog bind:this={dialog} class="prompt-overlay" aria-modal="true" aria-label="Connect to your server" oncancel={(e) => { e.preventDefault(); if (screen !== 'progress') onClose(); }} onkeydown={onKey}>
  <div class="prompt-dialog ssh-dialog">
    {#if screen === 'connect'}
      <div class="prompt-title">Connect to your server</div>
      <p class="confirm-message">Enter what your hosting provider gave you. Jodd stores no password — it sets up its own key.</p>
      <label class="ssh-field">Server address
        <input data-testid="easy-ssh-host" bind:value={address} placeholder="box.example.com" autocomplete="off" spellcheck="false" />
      </label>
      <label class="ssh-field">Username
        <input data-testid="easy-ssh-user" bind:value={username} placeholder="me" autocomplete="off" spellcheck="false" />
      </label>
      {#if !$isAndroid}
        <div class="ssh-row" role="radiogroup" aria-label="How you sign in">
          <label><input type="radio" bind:group={method} value="password" /> Password</label>
          <label><input type="radio" bind:group={method} value="keyfile" /> Key file</label>
        </div>
      {/if}
      {#if method === 'password'}
        <label class="ssh-field">Password
          <input data-testid="easy-ssh-password" type="password" bind:value={password} autocomplete="off" />
        </label>
      {:else}
        <label class="ssh-field">Key file
          <span class="ssh-row">
            <input readonly value={keyPath} placeholder="No file chosen" />
            <button type="button" class="prompt-btn" onclick={pickKeyFile}>Choose file…</button>
          </span>
        </label>
        <label class="ssh-field">Passphrase (only if the key needs one)
          <input type="password" bind:value={keyPassphrase} autocomplete="off" />
        </label>
      {/if}
      <label class="ssh-field">Notes folder on the server
        <input data-testid="easy-ssh-root" bind:value={root} placeholder="~/Jodd-Notes" autocomplete="off" spellcheck="false" autocapitalize="off" />
      </label>
      <label class="ssh-field">Name in the sidebar (optional)
        <input bind:value={name} placeholder="defaults to the server's name" />
      </label>
      {#if error}<div class="ssh-error" role="alert">{error}</div>{/if}
      <div class="prompt-actions">
        <button type="button" class="prompt-btn" onclick={onClose}>Cancel</button>
        <button data-testid="easy-ssh-continue" type="button" class="prompt-btn primary" disabled={busy || !address.trim()} onclick={fetchFingerprint}>{busy ? 'Connecting…' : 'Continue'}</button>
      </div>
    {:else if screen === 'trust'}
      <div class="prompt-title">Is this your server?</div>
      <p class="confirm-message">
        First time connecting to <strong>{parsed().host}</strong>. Its identity is
        <code>{fingerprint?.fingerprint}</code> ({fingerprint?.algorithm}). If your
        hosting provider shows a fingerprint in their panel or email, check that it
        matches. Otherwise it's normal to continue.
      </p>
      <p class="confirm-message">
        Jodd's key will give this computer the same access to the server's shell as
        your password — not just to your notes.
      </p>
      {#if error}<div class="ssh-error" role="alert">{error}</div>{/if}
      <div class="prompt-actions">
        <button type="button" class="prompt-btn" onclick={() => (screen = 'connect')}>Back</button>
        <button data-testid="easy-ssh-trust" type="button" class="prompt-btn primary" onclick={trustAndConnect}>Trust and connect</button>
      </div>
    {:else}
      <div class="prompt-title">Setting up your account</div>
      <ul class="ssh-progress">
        {#each STEPS as label, i}
          <li class:done={step > i + 1 || step === 4} class:active={step === i + 1}>{label}</li>
        {/each}
      </ul>
      {#if error}
        <div class="ssh-error" role="alert">{error}</div>
        <div class="prompt-actions">
          <button type="button" class="prompt-btn" onclick={() => (screen = 'trust')}>Back</button>
        </div>
      {/if}
    {/if}
  </div>
</dialog>

<style>
  .prompt-overlay { position: fixed; inset: 0; width: 100%; height: 100%; max-width: none; max-height: none; margin: 0; padding: 0; border: 0; box-sizing: border-box; background: var(--scrim); }
  .prompt-overlay::backdrop { background: transparent; }
  .prompt-overlay[open] { display: flex; align-items: center; justify-content: center; z-index: 2000; }
  .prompt-dialog { background: var(--surface-panel); box-sizing: border-box; max-width: 80vw; max-height: 80vh; overflow-y: auto; padding: 18px 20px 14px; border-radius: 10px; box-shadow: var(--shadow-modal); }
  .ssh-dialog { min-width: min(460px, 90vw); }
  .prompt-title { font-size: var(--size); font-weight: 600; color: var(--text); line-height: var(--leading); margin-bottom: 10px; }
  .confirm-message { font-size: var(--size); color: var(--text-secondary); line-height: var(--leading); margin: 6px 0; }
  .ssh-field { display: flex; flex-direction: column; gap: 4px; margin-top: 10px; font-size: var(--size); color: var(--text); }
  .ssh-field input { font: inherit; padding: 5px 7px; border-radius: 6px; border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); }
  .ssh-row { display: flex; gap: 12px; align-items: center; margin-top: 8px; }
  .ssh-row input { flex: 1; }
  .ssh-error { margin-top: 10px; color: var(--danger); font-size: var(--size); white-space: pre-wrap; overflow-wrap: anywhere; }
  .prompt-actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 12px; }
  .prompt-btn { padding: 5px 14px; font-size: var(--size-sm); border-radius: 5px; border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); cursor: pointer; }
  .prompt-btn:hover { background: var(--surface-list); }
  .prompt-btn.primary { background: var(--accent-action); color: var(--text-inverse); border-color: var(--accent-action); }
  .prompt-btn.primary:hover { background: var(--accent-hover); }
  .prompt-btn:disabled { opacity: 0.5; cursor: default; }
  .ssh-progress { list-style: none; margin: 12px 0; padding: 0; display: flex; flex-direction: column; gap: 8px; }
  .ssh-progress li { color: var(--text-secondary); font-size: var(--size); }
  .ssh-progress li.active { color: var(--text); font-weight: 600; }
  .ssh-progress li.done { color: var(--text-secondary); text-decoration: line-through; }
</style>
