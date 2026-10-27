<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { open as openFileDialog } from '@tauri-apps/plugin-dialog';
  import type { Account } from '../types';
  import { parseAddress, validateTarget } from '../sshAccountForm';
  import { isAndroid } from '../stores/platform';
  import Modal from './Modal.svelte';

  export let onAdded: (a: Account) => void;
  export let onClose: () => void;

  type Screen = 'connect' | 'trust' | 'progress';
  type Method = 'password' | 'keyfile';

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

  // One title per screen, one primary per screen. The connect/trust Cancel is
  // gone: Modal's header close is the one close control (ruling R10).
  $: title = screen === 'connect' ? 'Connect to your server'
    : screen === 'trust' ? 'Is this your server?'
    : 'Setting up your account';
  $: primary = screen === 'connect'
    ? { label: busy ? 'Connecting…' : 'Continue', onclick: fetchFingerprint, disabled: busy || !address.trim() }
    : screen === 'trust'
      ? { label: 'Trust and connect', onclick: trustAndConnect }
      : undefined;
</script>

<Modal
  {title}
  size="md"
  locked={screen === 'progress'}
  onclose={onClose}
  {primary}
  footer={screen === 'trust' ? backFooter : undefined}
>
  {#if screen === 'connect'}
    <p class="note">Enter what your hosting provider gave you. Jodd stores no password — it sets up its own key.</p>
    <label class="ssh-field">Server address
      <input data-autofocus data-testid="easy-ssh-host" bind:value={address} placeholder="box.example.com" autocomplete="off" spellcheck="false" />
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
          <button type="button" class="ssh-btn" onclick={pickKeyFile}>Choose file…</button>
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
  {:else if screen === 'trust'}
    <p class="note">
      First time connecting to <strong>{parsed().host}</strong>. Its identity is
      <code>{fingerprint?.fingerprint}</code> ({fingerprint?.algorithm}). If your
      hosting provider shows a fingerprint in their panel or email, check that it
      matches. Otherwise it's normal to continue.
    </p>
    <p class="note">
      Jodd's key will give this computer the same access to the server's shell as
      your password — not just to your notes.
    </p>
    {#if error}<div class="ssh-error" role="alert">{error}</div>{/if}
  {:else}
    <ul class="ssh-progress">
      {#each STEPS as label, i}
        <li class:done={step > i + 1 || step === 4} class:active={step === i + 1}>{label}</li>
      {/each}
    </ul>
    <!-- No error here: every failure sets screen = 'trust' in the same step. -->
  {/if}
</Modal>

{#snippet backFooter()}
  <button type="button" class="ssh-btn" onclick={() => (screen = 'connect')}>Back</button>
{/snippet}

<style>
  .note { font-size: var(--size); color: var(--text-secondary); line-height: var(--leading); margin: var(--space-1-5) 0; }
  .ssh-field { display: flex; flex-direction: column; gap: var(--space-1); margin-top: var(--space-3); font-size: var(--size); color: var(--text); }
  .ssh-field input { font: inherit; padding: var(--space-1) var(--space-2); border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); }
  .ssh-row { display: flex; gap: var(--space-3); align-items: center; margin-top: var(--space-2); }
  .ssh-row input { flex: 1; }
  .ssh-error { margin-top: var(--space-3); color: var(--danger); font-size: var(--size); white-space: pre-wrap; overflow-wrap: anywhere; }
  .ssh-btn { padding: var(--space-1) var(--space-3); font-size: var(--size-sm); border-radius: var(--radius); border: 1px solid var(--border); background: var(--surface-panel); color: var(--text); cursor: pointer; }
  .ssh-btn:hover { background: var(--surface-list); }
  .ssh-progress { list-style: none; margin: var(--space-3) 0; padding: 0; display: flex; flex-direction: column; gap: var(--space-2); }
  .ssh-progress li { color: var(--text-secondary); font-size: var(--size); }
  .ssh-progress li.active { color: var(--text); font-weight: 600; }
  .ssh-progress li.done { color: var(--text-secondary); text-decoration: line-through; }
</style>
