<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { untrack } from 'svelte';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import Icon from './Icon.svelte';
  import { hostLabel } from '../cliPrivacy';
  import { codeSegments } from '../text/codeSegments';
  import { validateTarget } from '../sshAccountForm';
  import { copyText } from '../clipboard';

  // The user's own server, where the agent CLIs run (claude / codex). Jodd
  // installs a small wrapper and its own key there; the CLIs' logins stay the
  // user's business and never pass through Jodd. This is an explicit,
  // user-triggered remote action, so awaiting IPC is fine — but the only state
  // written after an await is that call's own result.
  type AgentHost = { target: string; port: number | null; key_dir: string };
  // `reason` is why a turned-off harness is off, in the host's own words, so
  // the AI settings can say it beside the disabled option.
  type PresetStatus = { id: string; path: string | null; reason?: string };
  // Mirrors `AiHostStatus` / `AiHostNotice` in ai_host_commands.rs.
  type NoticeKind = 'upgrade_host' | 'ask_admin' | 'update_jodd';
  type Notice = { feature: string; kind: NoticeKind; text: string };
  type McpStatus = { protocol: 'mcp'; version: string; harnesses: string[]; notices: Notice[] };
  type LegacyStatus = {
    protocol: 'legacy'; version: number; current: boolean; presets: PresetStatus[]; notices: Notice[];
  };
  type HostStatus = McpStatus | LegacyStatus;
  type Fingerprint = { algorithm: string; fingerprint: string; known_hosts_line: string };
  // The two lines the user runs on the host, byte-exact as the backend made them.
  type Commands = { sudo: string; docker: string };

  let {
    host,
    onchange,
    onremoved,
    onreplaced,
    removedNote = 'Jodd will have no AI provider until you choose one.',
  }: {
    host: AgentHost | null;
    onchange: (h: AgentHost | null, presets: PresetStatus[]) => void;
    // Called once the server's key is gone. Remove takes effect immediately
    // (the key is deleted on the spot), so the parent must not keep a saved
    // config that still points at it.
    onremoved?: (removed: AgentHost) => void;
    // Called after Change server set up the new host and deleted the old
    // key. Like Remove, the old key is gone at once, so a saved config that
    // points at it must move to the new host now, not at the next Save.
    onreplaced?: (old: AgentHost, next: AgentHost) => void;
    // What happens to AI here once the server is gone. Scope-dependent: an
    // account falls back to the app's provider, the app has none.
    removedNote?: string;
  } = $props();

  const LABELS: Record<string, string> = { claude: 'Claude Code', codex: 'Codex CLI' };
  // The harnesses Jodd has a preset for, in display order.
  const JODD_HARNESSES = ['claude', 'codex'];

  function reasonFor(s: McpStatus, id: string): string {
    return s.notices.find((n) => n.feature === id)?.text ?? 'Turned off on this host.';
  }
  // What the settings form gates its preset list on: a harness the host has
  // enabled is usable, one it has not is not, and says why.
  function presetsOf(s: HostStatus): PresetStatus[] {
    if (s.protocol === 'legacy') return s.presets;
    return JODD_HARNESSES.map((id) =>
      s.harnesses.includes(id) ? { id, path: id } : { id, path: null, reason: reasonFor(s, id) },
    );
  }

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
  let step: 'form' | 'trust' | 'command' = $state('form');
  // How Jodd's key reaches the host: the password once (unlock), or a command
  // the user runs on the host themselves.
  let method: 'password' | 'command' = $state('password');
  // Command path: the host prepared by `ai_host_prepare_command` (its key
  // exists locally, and maybe not yet on the host) and the lines to run.
  let prepared: AgentHost | null = $state(null);
  let commands: Commands | null = $state(null);
  // A finish in flight. While set, Back/Cancel are disabled: a cancel landing
  // mid-finish would delete the key of the host being saved.
  let finishing = $state(false);
  // Reinstall by command: the lines to run, until Check succeeds.
  let reinstallCommands: Commands | null = $state(null);
  // Which Copy button just worked, and which one failed.
  let copied: 'sudo' | 'docker' | '' = $state('');
  let copyFailed: 'sudo' | 'docker' | '' = $state('');
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  let busy = $state(false);
  // A background refresh, apart from `busy` so it cannot release another
  // operation's lock when it finishes.
  let refreshing = $state(false);
  let error = $state('');
  let updating = $state(false);
  let confirmRemove = $state(false);
  // Change server: the setup form, prefilled, while the current host stays
  // in use. A new address, port or login needs a new fingerprint check and a
  // new key, so this is a fresh setup; the old key is dropped only after the
  // new one works, so AI never goes dark halfway through.
  let changing = $state(false);
  // Set after a successful Setup: the one step Jodd cannot do for the user.
  let showLockTip = $state(false);

  function startChange() {
    const h = current;
    if (!h) return;
    // `target` is `<user>@<server>`; a server name never contains `@`.
    const at = h.target.lastIndexOf('@');
    username = at > 0 ? h.target.slice(0, at) : '';
    server = at > 0 ? h.target.slice(at + 1) : h.target;
    port = h.port != null ? String(h.port) : '';
    password = '';
    method = 'password';
    fingerprint = null;
    error = '';
    step = 'form';
    updating = false;
    showLockTip = false;
    changing = true;
  }

  function cancelChange() {
    changing = false;
    method = 'password';
    step = 'form';
    fingerprint = null;
    password = '';
    error = '';
  }

  function parsedPort(): number | null | 'bad' {
    const t = port.trim();
    if (!t) return null;
    const n = Number(t);
    return Number.isInteger(n) && n > 0 && n < 65536 ? n : 'bad';
  }

  // `quiet` is a refresh nobody is waiting on (after Test): it must not take
  // `busy` or clear `error` from a Setup, Remove or Reinstall in progress.
  async function refresh(h: AgentHost, quiet = false) {
    if (quiet) {
      refreshing = true;
    } else {
      busy = true;
      error = '';
    }
    try {
      const s = await invoke<HostStatus>('ai_host_status', { host: h });
      if (current?.key_dir === h.key_dir) {
        status = s;
        // The settings form gates its preset list on what is installed here.
        onchange(h, presetsOf(s));
      }
    } catch (e) {
      if (current?.key_dir === h.key_dir && !(quiet && busy)) error = String(e);
    } finally {
      if (quiet) refreshing = false;
      else busy = false;
    }
  }

  // Ask the host again, e.g. after Test connection: a harness the administrator
  // has just turned on (or off) shows up without reopening Settings.
  export async function refreshStatus() {
    const h = current;
    if (h) await refresh(h, true);
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

  // What both setup paths do once the new host works: forget the host being
  // replaced (Change server) only now, then show the new one.
  async function finishSetup(r: { host: AgentHost; status: HostStatus }, old: AgentHost | null, lockTip: boolean) {
    if (old) {
      // The new host works, so the old key can go. Best effort, like
      // Remove: an unreachable old server is only forgotten locally, and a
      // failure here must not undo the new setup.
      try {
        await invoke('ai_host_forget', { host: old });
      } catch (e) {
        console.warn('ai_host_forget (old server)', e);
      }
    }
    loadedFor = r.host.key_dir;
    current = r.host;
    status = r.status;
    step = 'form';
    fingerprint = null;
    changing = false;
    // Only the password path needs the host's password login locked again;
    // the command path never unlocked it, so its text has no lock step.
    showLockTip = lockTip;
    prepared = null;
    commands = null;
    method = 'password';
    onchange(r.host, presetsOf(r.status));
    if (old) onreplaced?.(old, r.host);
  }

  async function trustAndSetUp() {
    if (!fingerprint) return;
    const user = username.trim();
    if (!user) { error = 'Enter a username.'; step = 'form'; return; }
    if (method === 'password' && !password) { error = 'Enter the password.'; return; }
    const p = parsedPort();
    // The host being replaced, if this is Change server.
    const old = changing ? current : null;
    busy = true;
    error = '';
    try {
      if (method === 'command') {
        const r = await invoke<{ host: AgentHost; commands: Commands }>('ai_host_prepare_command', {
          host: server.trim(),
          port: p === 'bad' ? null : p,
          user,
          knownHostsLine: fingerprint.known_hosts_line,
        });
        prepared = r.host;
        commands = r.commands;
        step = 'command';
        return;
      }
      const r = await invoke<{ host: AgentHost; status: HostStatus }>('ai_host_setup', {
        host: server.trim(),
        port: p === 'bad' ? null : p,
        user,
        password,
        knownHostsLine: fingerprint.known_hosts_line,
      });
      await finishSetup(r, old, true);
    } catch (e) {
      error = String(e);
    } finally {
      password = '';
      busy = false;
    }
  }

  // "I've run it": the host now accepts the key, or says it does not yet. A
  // failure keeps the prepared host so a retry reuses its key.
  async function finishCommand() {
    const h = prepared;
    if (!h || finishing) return;
    const old = changing ? current : null;
    finishing = true;
    error = '';
    try {
      const r = await invoke<{ host: AgentHost; status: HostStatus }>('ai_host_finish_command', { host: h });
      await finishSetup(r, old, false);
    } catch (e) {
      error = String(e);
    } finally {
      finishing = false;
    }
  }

  // Back or Cancel on the command screen: the prepared key goes (locally, and
  // remotely if it was added), then the host is forgotten here.
  function leaveCommand(to: 'trust' | 'cancel') {
    if (finishing) return;
    const h = prepared;
    if (h) {
      void invoke('ai_host_cancel_command', { host: h }).catch((e) => console.warn('ai_host_cancel_command', e));
    }
    prepared = null;
    commands = null;
    error = '';
    if (to === 'trust') step = 'trust';
    else if (changing) cancelChange();
    else { step = 'form'; fingerprint = null; }
  }

  async function copy(which: 'sudo' | 'docker') {
    const text = commands?.[which] ?? reinstallCommands?.[which];
    if (text == null) return;
    const ok = await copyText(text);
    clearTimeout(copiedTimer);
    copied = ok ? which : '';
    copyFailed = ok ? '' : which;
    if (ok) copiedTimer = setTimeout(() => { copied = ''; }, 1500);
  }

  async function showReinstallCommands() {
    const h = current;
    if (!h) return;
    busy = true;
    error = '';
    try {
      reinstallCommands = await invoke<Commands>('ai_host_reinstall_commands', { host: h });
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  function closeReinstall() {
    updating = false;
    reinstallCommands = null;
    method = 'password';
    password = '';
    error = '';
  }

  // Check after the user ran the reinstall command: the same status refresh
  // `update()` ends with. `ai_host_status` falls back to the legacy status, so
  // only an MCP answer means the command ran; anything else keeps the commands
  // on screen.
  async function checkReinstall() {
    const h = current;
    if (!h) return;
    busy = true;
    error = '';
    try {
      const s = await invoke<HostStatus>('ai_host_status', { host: h });
      if (current?.key_dir === h.key_dir) {
        status = s;
        onchange(h, presetsOf(s));
        if (s.protocol === 'mcp') closeReinstall();
        else error = 'The host still runs the older helper. Run the command (needs AI host kit 1.1.0 or later), then press Check again.';
      }
    } catch (e) {
      error = String(e);
    } finally {
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
        onchange(h, presetsOf(s));
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
      showLockTip = false;
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

<!-- Host text is untrusted: backticks make <code>, nothing else is markup. -->
{#snippet rich(text: string)}{#each codeSegments(text) as seg}{#if seg.code}<code>{seg.text}</code>{:else}{seg.text}{/if}{/each}{/snippet}

{#snippet methodChoice()}
  <div class="methods" role="radiogroup" aria-label="How to authorize Jodd's key">
    <label class="choice"><input type="radio" name="ai-host-method" value="password" bind:group={method} disabled={busy} /><span>Password</span></label>
    <label class="choice"><input type="radio" name="ai-host-method" value="command" bind:group={method} disabled={busy} /><span>Run a command on the host</span></label>
  </div>
{/snippet}

{#snippet commandBlocks(cmds: Commands)}
  {#each [['sudo', 'On the host'], ['docker', 'Docker']] as [id, label] (id)}
    {@const which = id as 'sudo' | 'docker'}
    <div class="cmd-block">
      <div class="cmd-head">
        <span class="cmd-label">{label}</span>
        <button type="button" onclick={() => void copy(which)}>Copy</button>
        {#if copied === which}<span class="copied" role="status">Copied</span>{/if}
      </div>
      <code class="cmd">{cmds[which]}</code>
      {#if copyFailed === which}<p class="help warn">Could not copy. Select the command and copy it.</p>{/if}
    </div>
  {/each}
  <p class="help">Needs AI host kit 1.1.0 or later.</p>
{/snippet}

<div class="ai-host">
  {#if current && !changing}
    {#if status?.protocol === 'mcp'}
      <p class="host-name">AI host {status.version} on <strong>{hostLabel(current)}</strong></p>
      <ul class="cli-list">
        {#each JODD_HARNESSES as id (id)}
          {@const on = status.harnesses.includes(id)}
          <li>
            <span class="mark" class:ok={on}><Icon name={on ? 'check' : 'close'} size={14} inherit /></span>
            <span>{LABELS[id] ?? id}</span>
            {#if !on}<span class="reason">{@render rich(reasonFor(status, id))}</span>{/if}
          </li>
        {/each}
      </ul>
      {#each status.notices.filter((n) => !JODD_HARNESSES.includes(n.feature)) as n (n.feature + n.kind)}
        <p class="help warn">{@render rich(n.text)}</p>
      {/each}
    {:else if status}
      <p class="host-name">Server: <strong>{hostLabel(current)}</strong></p>
      <ul class="cli-list">
        {#each status.presets as p (p.id)}
          <li>
            <span class="mark" class:ok={p.path != null}><Icon name={p.path != null ? 'check' : 'close'} size={14} inherit /></span>
            <span>{LABELS[p.id] ?? p.id}</span>
            <span class="detail">{p.path ?? 'not installed'}</span>
          </li>
        {/each}
      </ul>
      {#each status.notices as n (n.feature + n.kind)}
        <p class="help warn">{@render rich(n.text)}</p>
      {/each}
      {#if !status.current}
        <p class="help warn">Jodd's helper on this server needs an update. Press Reinstall helper.</p>
      {/if}
    {:else if busy}
      <p class="help">Checking the server…</p>
    {/if}

    {#if showLockTip}
      <p class="help">Next, on the host: run <code>sudo jodd-ai-host lock</code> so the AI account can no longer log in with its password.</p>
    {/if}

    {#if updating && status?.protocol !== 'mcp'}
      {#if reinstallCommands}
        <p class="help">Run this on the host, then press Check.</p>
        {@render commandBlocks(reinstallCommands)}
        <div class="row">
          <button type="button" onclick={checkReinstall} disabled={busy}>{busy ? 'Checking…' : 'Check'}</button>
          <button type="button" onclick={closeReinstall} disabled={busy}>Cancel</button>
        </div>
      {:else}
        {@render methodChoice()}
        {#if method === 'password'}
          <label class="field">Password
            <input type="password" bind:value={password} autocomplete="off" disabled={busy} />
          </label>
          <p class="help">Used once to reinstall Jodd's helper on this server; Jodd does not keep it.</p>
        {:else}
          <p class="help">Jodd shows a command to run on the host. No password is used.</p>
        {/if}
        <div class="row">
          {#if method === 'password'}
            <button type="button" onclick={update} disabled={busy}>{busy ? 'Reinstalling…' : 'Reinstall now'}</button>
          {:else}
            <button type="button" onclick={showReinstallCommands} disabled={busy}>Show command</button>
          {/if}
          <button type="button" onclick={closeReinstall} disabled={busy}>Cancel</button>
        </div>
      {/if}
    {:else}
      <div class="row">
        {#if status?.protocol !== 'mcp'}
          <button type="button" onclick={() => { updating = true; method = 'password'; error = ''; }} disabled={busy}>Reinstall helper</button>
        {/if}
        <button type="button" onclick={startChange} disabled={busy}>Change server</button>
        <button type="button" onclick={() => { confirmRemove = true; }} disabled={busy}>Remove</button>
      </div>
    {/if}
  {:else if step === 'form'}
    {#if changing && current}
      <p class="help">Changing from <strong>{hostLabel(current)}</strong>. It keeps working until the new one is set up.</p>
    {/if}
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
      {#if changing}
        <button type="button" onclick={cancelChange} disabled={busy}>Cancel</button>
      {/if}
      <button type="button" onclick={fetchFingerprint} disabled={busy || !server.trim()}>{busy ? 'Connecting…' : 'Continue'}</button>
    </div>
  {:else if step === 'command' && commands}
    <p class="help">Run one of these on <strong>{server.trim()}</strong>, then press I've run it.</p>
    {@render commandBlocks(commands)}
    <div class="row">
      <button type="button" onclick={() => leaveCommand('trust')} disabled={finishing}>Back</button>
      <button type="button" onclick={() => leaveCommand('cancel')} disabled={finishing}>Cancel</button>
      <button type="button" onclick={finishCommand} disabled={finishing}>I've run it</button>
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
    {@render methodChoice()}
    {#if method === 'password'}
      <label class="field">Password
        <input type="password" bind:value={password} autocomplete="off" disabled={busy} />
      </label>
    {/if}
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
    border: 1px solid var(--border); border-radius: var(--radius-sm);
  }
  .row { display: flex; gap: 8px; margin-top: 8px; }
  .methods { display: flex; flex-wrap: wrap; gap: 4px 16px; margin: 8px 0; font-size: var(--size); }
  .choice { display: inline-flex; align-items: center; gap: 6px; }
  .cmd-block { margin: 8px 0; }
  .cmd-head { display: flex; align-items: center; gap: 8px; font-size: var(--size-sm); }
  .cmd-label { font-weight: var(--weight-semibold); }
  .copied { color: var(--success); }
  .cmd {
    display: block; margin-top: 4px; padding: 6px 8px; font-family: var(--font-mono); font-size: var(--size-sm);
    white-space: pre-wrap; word-break: break-all; user-select: text;
    background: var(--surface-panel); border: 1px solid var(--border); border-radius: var(--radius-sm);
  }
  button { padding: 6px 14px; font: inherit; }
  .host-name { margin: 4px 0; font-size: var(--size); }
  .cli-list { list-style: none; margin: 6px 0; padding: 0; display: grid; gap: 4px; font-size: var(--size); }
  .cli-list li { display: flex; gap: 8px; align-items: center; }
  .mark { display: inline-flex; color: var(--danger); }
  .mark.ok { color: var(--success); }
  .reason { color: var(--text-muted); font-size: var(--size-sm); overflow-wrap: anywhere; }
  .detail { color: var(--text-muted); font-family: var(--font-mono); font-size: var(--size-sm); overflow-wrap: anywhere; }
  .help { font-size: var(--size-sm); color: var(--accent-action); margin: 6px 0 0; line-height: var(--leading); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .help.warn { background: var(--warn-wash); border-left: 3px solid var(--accent); padding: 6px 8px; border-radius: var(--radius-sm); }
  .err { color: var(--danger); margin: 8px 0 0; font-size: var(--size-sm); white-space: pre-wrap; overflow-wrap: anywhere; }
</style>
