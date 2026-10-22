<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { untrack } from 'svelte';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import Icon from './Icon.svelte';
  import { hostLabel } from '../cliPrivacy';
  import { validateTarget } from '../sshAccountForm';

  // The user's own server, where the agent CLIs run (claude / codex). Jodd
  // installs a small wrapper and its own key there; the CLIs' logins stay the
  // user's business and never pass through Jodd. This is an explicit,
  // user-triggered remote action, so awaiting IPC is fine — but the only state
  // written after an await is that call's own result.
  type AgentHost = { target: string; port: number | null; key_dir: string };
  type PresetStatus = { id: string; path: string | null };
  type HostStatus = { version: number; current: boolean; presets: PresetStatus[] };
  type Fingerprint = { algorithm: string; fingerprint: string; known_hosts_line: string };

  let {
    host,
    onchange,
    onremoved,
    removedNote = 'Jodd will have no AI provider until you choose one.',
  }: {
    host: AgentHost | null;
    onchange: (h: AgentHost | null, presets: PresetStatus[]) => void;
    // Called once the server's key is gone. Remove takes effect immediately
    // (the key is deleted on the spot), so the parent must not keep a saved
    // config that still points at it.
    onremoved?: (removed: AgentHost) => void;
    // What happens to AI here once the server is gone. Scope-dependent: an
    // account falls back to the app's provider, the app has none.
    removedNote?: string;
  } = $props();

  const LABELS: Record<string, string> = { claude: 'Claude Code', codex: 'Codex CLI' };

  // Local copy so the panel shows the result of its own setup even before the
  // parent has handed the new host back down.
  let current: AgentHost | null = $state(null);
  $effect(() => { current = host; });

  let status: HostStatus | null = $state(null);
  let loadedFor = '';
  $effect(() => {
    const h = current;
    if (!h) { status = null; loadedFor = ''; return; }
    if (h.key_dir === loadedFor) return;
    loadedFor = h.key_dir;
    untrack(() => { void refresh(h); });
  });

  let server = $state('');
  let port = $state('');
  let username = $state('');
  let password = $state('');
  let fingerprint: Fingerprint | null = $state(null);
  let step: 'form' | 'trust' = $state('form');
  let busy = $state(false);
  let error = $state('');
  let updating = $state(false);
  let confirmRemove = $state(false);

  function parsedPort(): number | null | 'bad' {
    const t = port.trim();
    if (!t) return null;
    const n = Number(t);
    return Number.isInteger(n) && n > 0 && n < 65536 ? n : 'bad';
  }

  async function refresh(h: AgentHost) {
    busy = true;
    error = '';
    try {
      const s = await invoke<HostStatus>('ai_host_status', { host: h });
      if (current?.key_dir === h.key_dir) {
        status = s;
        // The settings form gates its preset list on what is installed here.
        onchange(h, s.presets);
      }
    } catch (e) {
      if (current?.key_dir === h.key_dir) error = String(e);
    } finally {
      busy = false;
    }
  }

  async function fetchFingerprint() {
    // The target Jodd builds is `<username>@<server>`; an `@` here would make
    // it `me@me@box`.
    if (server.includes('@')) {
      error = 'Enter the server name only, without “@”. Put the login name in Username.';
      return;
    }
    const bad = validateTarget(server);
    if (bad) { error = bad; return; }
    const p = parsedPort();
    if (p === 'bad') { error = 'Port must be a number between 1 and 65535.'; return; }
    busy = true;
    error = '';
    try {
      fingerprint = await invoke<Fingerprint>('ssh_fingerprint', { host: server.trim(), port: p });
      step = 'trust';
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function trustAndSetUp() {
    if (!fingerprint) return;
    const user = username.trim();
    if (!user) { error = 'Enter a username.'; step = 'form'; return; }
    if (!password) { error = 'Enter the password.'; return; }
    const p = parsedPort();
    busy = true;
    error = '';
    try {
      const r = await invoke<{ host: AgentHost; status: HostStatus }>('ai_host_setup', {
        host: server.trim(),
        port: p === 'bad' ? null : p,
        user,
        password,
        knownHostsLine: fingerprint.known_hosts_line,
      });
      loadedFor = r.host.key_dir;
      current = r.host;
      status = r.status;
      step = 'form';
      fingerprint = null;
      onchange(r.host, r.status.presets);
    } catch (e) {
      error = String(e);
    } finally {
      password = '';
      busy = false;
    }
  }

  async function update() {
    const h = current;
    if (!h) return;
    if (!password) { error = 'Enter the password.'; return; }
    busy = true;
    error = '';
    try {
      const s = await invoke<HostStatus>('ai_host_update', { host: h, password });
      if (current?.key_dir === h.key_dir) {
        status = s;
        updating = false;
        onchange(h, s.presets);
      }
    } catch (e) {
      error = String(e);
    } finally {
      password = '';
      busy = false;
    }
  }

  async function remove() {
    confirmRemove = false;
    const h = current;
    if (!h) return;
    busy = true;
    error = '';
    try {
      await invoke('ai_host_forget', { host: h });
      current = null;
      status = null;
      updating = false;
      onchange(null, []);
      onremoved?.(h);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if confirmRemove && current}
  <ConfirmDialog
    title="Remove this server?"
    message={`This takes effect immediately, not when you press Save: Jodd deletes its key on ${hostLabel(current)} and forgets the server. ${removedNote} The CLIs installed there are not touched. If the server cannot be reached, Jodd only forgets it locally.`}
    confirmLabel="Remove server"
    destructive
    onConfirm={remove}
    onCancel={() => { confirmRemove = false; }}
  />
{/if}

<div class="ai-host">
  {#if current}
    <p class="host-name">Server: <strong>{hostLabel(current)}</strong></p>
    {#if status}
      <ul class="cli-list">
        {#each status.presets as p (p.id)}
          <li>
            <span class="mark" class:ok={p.path != null}><Icon name={p.path != null ? 'check' : 'close'} size={14} inherit /></span>
            <span>{LABELS[p.id] ?? p.id}</span>
            <span class="detail">{p.path ?? 'not installed'}</span>
          </li>
        {/each}
      </ul>
      {#if !status.current}
        <p class="help warn">Jodd's helper on this server needs an update.</p>
      {/if}
    {:else if busy}
      <p class="help">Checking the server…</p>
    {/if}

    {#if updating}
      <label class="field">Password
        <input type="password" bind:value={password} autocomplete="off" disabled={busy} />
      </label>
      <p class="help">Used once to reinstall the helper; Jodd does not keep it.</p>
      <div class="row">
        <button type="button" onclick={update} disabled={busy}>{busy ? 'Updating…' : 'Update now'}</button>
        <button type="button" onclick={() => { updating = false; password = ''; error = ''; }} disabled={busy}>Cancel</button>
      </div>
    {:else}
      <div class="row">
        <button type="button" onclick={() => { updating = true; error = ''; }} disabled={busy}>Update</button>
        <button type="button" onclick={() => { confirmRemove = true; }} disabled={busy}>Remove</button>
      </div>
    {/if}
  {:else if step === 'form'}
    <p class="help">Jodd runs the CLI on your own server over SSH.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>It needs your server's address and a login it can use once to set itself up.</p>
    </details>
    <label class="field">Server
      <input type="text" bind:value={server} placeholder="box.example.com" autocomplete="off" spellcheck="false" />
    </label>
    <label class="field">Port
      <input type="text" inputmode="numeric" bind:value={port} placeholder="22" autocomplete="off" />
    </label>
    <label class="field">Username
      <input type="text" bind:value={username} placeholder="me" autocomplete="off" spellcheck="false" />
    </label>
    <div class="row">
      <button type="button" onclick={fetchFingerprint} disabled={busy || !server.trim()}>{busy ? 'Connecting…' : 'Continue'}</button>
    </div>
  {:else}
    <!-- Both are decisions the user must make here, so they stay visible. -->
    <p class="help">
      First time connecting to <strong>{server.trim()}</strong>. Its identity is
      <code>{fingerprint?.fingerprint}</code> ({fingerprint?.algorithm}). If your hosting
      provider shows a fingerprint, check it matches.
    </p>
    <p class="help warn">Codex can read any file this account can. A dedicated server account is safer.</p>
    <details class="learn-more">
      <summary>Learn more</summary>
      <p>Jodd's key can only run Claude Code or Codex on this server &mdash; but its answers come back to Jodd.</p>
    </details>
    <label class="field">Password
      <input type="password" bind:value={password} autocomplete="off" disabled={busy} />
    </label>
    <div class="row">
      <button type="button" onclick={() => { step = 'form'; password = ''; error = ''; }} disabled={busy}>Back</button>
      <button type="button" onclick={trustAndSetUp} disabled={busy}>{busy ? 'Setting up…' : 'Trust and set up'}</button>
    </div>
  {/if}

  {#if error}<p class="err" role="alert">{error}</p>{/if}

  <p class="help">Sign the CLIs in on the server yourself first.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>Run <code>claude</code>, or <code>codex login --device-auth</code>, over SSH. Jodd never sees those logins.</p>
  </details>
  <p class="help"><button type="button" class="link-btn" onclick={() => void openUrl('https://jodd.bbmedia.co.th/ai-host.html').catch((e) => console.warn('open set-up guide', e))}>How to set up a server</button></p>
</div>

<style>
  .ai-host { margin: 8px 0; }
  .field { display: flex; flex-direction: column; gap: 4px; margin: 8px 0; font-size: var(--size); }
  .field input {
    font: inherit; padding: 6px; box-sizing: border-box; width: 100%;
    background: var(--surface-panel); color: var(--text);
    border: 1px solid var(--border); border-radius: 4px;
  }
  .row { display: flex; gap: 8px; margin-top: 8px; }
  button { padding: 6px 14px; font: inherit; }
  .host-name { margin: 4px 0; font-size: var(--size); }
  .cli-list { list-style: none; margin: 6px 0; padding: 0; display: grid; gap: 4px; font-size: var(--size); }
  .cli-list li { display: flex; gap: 8px; align-items: center; }
  .mark { display: inline-flex; color: var(--danger); }
  .mark.ok { color: var(--success); }
  .detail { color: var(--text-muted); font-family: var(--font-mono); font-size: var(--size-sm); overflow-wrap: anywhere; }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 6px 0 0; line-height: var(--leading); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .help.warn { background: var(--warn-wash); border-left: 3px solid var(--accent); padding: 6px 8px; border-radius: 3px; }
  .err { color: var(--danger); margin: 8px 0 0; font-size: var(--size-sm); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
