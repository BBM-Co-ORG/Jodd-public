// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import AiHostPanel from './AiHostPanel.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const openUrl = vi.fn();
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: (...a: unknown[]) => openUrl(...a) }));

let target: HTMLElement;
let app: ReturnType<typeof mount> | undefined;

beforeEach(() => {
  invoke.mockReset();
  openUrl.mockReset().mockResolvedValue(undefined);
  document.body.innerHTML = '';
  target = document.createElement('div');
  document.body.append(target);
});
afterEach(async () => {
  if (app) await unmount(app);
  app = undefined;
});

async function settle() {
  for (let i = 0; i < 10; i++) await tick();
  flushSync();
}
function field(label: string): HTMLInputElement {
  const l = [...target.querySelectorAll('label')].find(
    (el) => [...el.childNodes].filter((n) => n.nodeType === 3).map((n) => n.textContent).join('').trim() === label,
  );
  if (!l) throw new Error(`no field ${label}: ${target.innerHTML}`);
  return l.querySelector('input') as HTMLInputElement;
}
function type(el: HTMLInputElement, value: string) {
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
}
function button(name: string): HTMLButtonElement {
  const b = [...target.querySelectorAll('button')].find((x) => x.textContent?.trim() === name);
  if (!b) throw new Error(`no button ${name}: ${target.innerHTML}`);
  return b as HTMLButtonElement;
}

const FP = { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'vps ssh-ed25519 AAAA' };

describe('AiHostPanel', () => {
  it('links to the server set-up guide', async () => {
    app = mount(AiHostPanel, { target, props: { host: null, onchange: vi.fn() } });
    await settle();
    button('How to set up a server').click();
    expect(openUrl).toHaveBeenCalledWith('https://jodd.bbmedia.co.th/ai-host.html');
  });

  it('walks fingerprint → password → setup and reports the host', async () => {
    const presets = [{ id: 'codex', path: '/usr/bin/codex' }, { id: 'claude', path: null }];
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ssh_fingerprint') return FP;
      if (cmd === 'ai_host_setup')
        return { host: { target: 'me@vps', port: null, key_dir: 'k' }, status: { version: 1, current: true, presets } };
      throw new Error(cmd);
    });
    const onchange = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: null, onchange } });
    type(field('Server'), 'vps');
    type(field('Username'), 'me');
    flushSync();
    button('Continue').click();
    await settle();
    expect(target.textContent).toContain('SHA256:abc');
    // "hosting provider", not "provider": on an AI settings page a bare
    // "provider" reads as the LLM provider.
    expect(target.textContent!.replace(/\s+/g, ' ')).toContain('If your hosting provider shows a fingerprint');
    type(field('Password'), 'pw');
    flushSync();
    button('Trust and set up').click();
    await settle();
    expect(target.textContent).toContain('Codex CLI');
    expect(onchange).toHaveBeenCalledWith({ target: 'me@vps', port: null, key_dir: 'k' }, presets);
    expect(invoke).toHaveBeenCalledWith('ai_host_setup', {
      host: 'vps', port: null, user: 'me', password: 'pw', knownHostsLine: 'vps ssh-ed25519 AAAA',
    });
  });

  it('shows "needs an update" when the wrapper is stale', async () => {
    invoke.mockResolvedValue({ version: 0, current: false, presets: [] });
    app = mount(AiHostPanel, { target, props: { host: { target: 'me@vps', port: null, key_dir: 'k' }, onchange: vi.fn() } });
    await settle();
    expect(target.textContent).toMatch(/needs an update/i);
  });

  it('clears the password field after a failed setup', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ssh_fingerprint') return { ...FP, known_hosts_line: 'l' };
      throw 'The server rejected that username or password.';
    });
    app = mount(AiHostPanel, { target, props: { host: null, onchange: vi.fn() } });
    type(field('Server'), 'vps');
    type(field('Username'), 'me');
    flushSync();
    button('Continue').click();
    await settle();
    const pw = field('Password');
    type(pw, 'bad');
    flushSync();
    button('Trust and set up').click();
    await settle();
    expect(target.textContent).toContain('rejected');
    expect(field('Password').value).toBe('');
  });

  it('lists installed and missing CLIs for a configured host', async () => {
    invoke.mockResolvedValue({
      version: 1, current: true,
      presets: [{ id: 'codex', path: '/usr/bin/codex' }, { id: 'claude', path: null }],
    });
    app = mount(AiHostPanel, { target, props: { host: { target: 'me@vps', port: null, key_dir: 'k' }, onchange: vi.fn() } });
    await settle();
    expect(target.textContent).toContain('/usr/bin/codex');
    expect(target.textContent).toContain('not installed');
    expect(invoke).toHaveBeenCalledWith('ai_host_status', { host: { target: 'me@vps', port: null, key_dir: 'k' } });
  });

  it('Remove asks first, forgets the host, then reports null', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ai_host_status') return { version: 1, current: true, presets: [] };
      if (cmd === 'ai_host_forget') return null;
      throw new Error(cmd);
    });
    const onchange = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: { target: 'me@vps', port: null, key_dir: 'k' }, onchange } });
    await settle();
    button('Remove').click();
    await settle();
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('ai_host_forget');
    [...target.querySelectorAll('dialog button')].find((b) => b.textContent?.trim() === 'Remove server')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await settle();
    expect(invoke).toHaveBeenCalledWith('ai_host_forget', { host: { target: 'me@vps', port: null, key_dir: 'k' } });
    expect(onchange).toHaveBeenCalledWith(null, []);
  });

  it('Remove also tells the parent it is gone (the key is deleted at once)', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ai_host_status') return { version: 1, current: true, presets: [] };
      if (cmd === 'ai_host_forget') return null;
      throw new Error(cmd);
    });
    const onremoved = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: { target: 'me@vps', port: null, key_dir: 'k' }, onchange: vi.fn(), onremoved } });
    await settle();
    button('Remove').click();
    await settle();
    expect(target.querySelector('dialog')?.textContent).toContain('takes effect immediately');
    [...target.querySelectorAll('dialog button')].find((b) => b.textContent?.trim() === 'Remove server')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await settle();
    expect(onremoved).toHaveBeenCalledTimes(1);
    expect(target.textContent).not.toContain('me@vps');
  });

  it('a failed Remove does not report the server gone', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ai_host_status') return { version: 1, current: true, presets: [] };
      throw 'boom';
    });
    const onremoved = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: { target: 'me@vps', port: null, key_dir: 'k' }, onchange: vi.fn(), onremoved } });
    await settle();
    button('Remove').click();
    await settle();
    [...target.querySelectorAll('dialog button')].find((b) => b.textContent?.trim() === 'Remove server')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await settle();
    expect(onremoved).not.toHaveBeenCalled();
  });

  it('rejects an "@" in Server instead of building me@me@box', async () => {
    app = mount(AiHostPanel, { target, props: { host: null, onchange: vi.fn() } });
    type(field('Server'), 'me@box');
    type(field('Username'), 'me');
    flushSync();
    button('Continue').click();
    await settle();
    expect(target.querySelector('[role="alert"]')?.textContent).toContain('without “@”');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('the trust screen says the key is restricted but Codex can read files', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ssh_fingerprint') return FP;
      throw new Error(cmd);
    });
    app = mount(AiHostPanel, { target, props: { host: null, onchange: vi.fn() } });
    type(field('Server'), 'vps');
    type(field('Username'), 'me');
    flushSync();
    button('Continue').click();
    await settle();
    const t = target.textContent ?? '';
    expect(t).toContain('can only run Claude Code or Codex');
    expect(t).toContain('Codex can read any file this account can');
    expect(t).not.toContain('same access to the server');
  });

  const OLD = { target: 'me@vps', port: 2222, key_dir: 'old' };
  const NEW = { target: 'jodd@box', port: null, key_dir: 'new' };
  const STATUS = { version: 1, current: true, presets: [{ id: 'claude', path: '/usr/bin/claude' }] };

  it('"Reinstall helper" reinstalls with the password and says so', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ai_host_status' || cmd === 'ai_host_update') return STATUS;
      throw new Error(cmd);
    });
    app = mount(AiHostPanel, { target, props: { host: OLD, onchange: vi.fn() } });
    await settle();
    expect([...target.querySelectorAll('button')].map((b) => b.textContent?.trim())).not.toContain('Update');
    button('Reinstall helper').click();
    flushSync();
    expect(target.textContent).toContain("reinstall Jodd's helper");
    type(field('Password'), 'pw');
    flushSync();
    button('Reinstall now').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('ai_host_update', { host: OLD, password: 'pw' });
  });

  it('Change server prefills the form from the current host', async () => {
    invoke.mockResolvedValue(STATUS);
    app = mount(AiHostPanel, { target, props: { host: OLD, onchange: vi.fn() } });
    await settle();
    button('Change server').click();
    flushSync();
    expect(field('Server').value).toBe('vps');
    expect(field('Port').value).toBe('2222');
    expect(field('Username').value).toBe('me');
    expect(target.textContent).toContain('keeps working until the new one is set up');
  });

  it('Change server sets up the new host first, then forgets the old one', async () => {
    const order: string[] = [];
    invoke.mockImplementation(async (cmd: string) => {
      order.push(cmd);
      if (cmd === 'ai_host_status') return STATUS;
      if (cmd === 'ssh_fingerprint') return FP;
      if (cmd === 'ai_host_setup') return { host: NEW, status: STATUS };
      if (cmd === 'ai_host_forget') return null;
      throw new Error(cmd);
    });
    const onchange = vi.fn();
    const onreplaced = vi.fn();
    const onremoved = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: OLD, onchange, onreplaced, onremoved } });
    await settle();
    button('Change server').click();
    flushSync();
    type(field('Server'), 'box');
    type(field('Port'), '');
    type(field('Username'), 'jodd');
    flushSync();
    button('Continue').click();
    await settle();
    type(field('Password'), 'pw');
    flushSync();
    button('Trust and set up').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('ai_host_setup', {
      host: 'box', port: null, user: 'jodd', password: 'pw', knownHostsLine: FP.known_hosts_line,
    });
    expect(invoke).toHaveBeenCalledWith('ai_host_forget', { host: OLD });
    expect(order.indexOf('ai_host_setup')).toBeLessThan(order.indexOf('ai_host_forget'));
    expect(onchange).toHaveBeenLastCalledWith(NEW, STATUS.presets);
    expect(onreplaced).toHaveBeenCalledWith(OLD, NEW);
    expect(onremoved).not.toHaveBeenCalled();
    expect(target.textContent).toContain('Server: box');
  });

  it('a failed Change server keeps the old host and its key', async () => {
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'ai_host_status') return STATUS;
      if (cmd === 'ssh_fingerprint') return FP;
      throw 'The server rejected that username or password.';
    });
    const onreplaced = vi.fn();
    const onchange = vi.fn();
    app = mount(AiHostPanel, { target, props: { host: OLD, onchange, onreplaced } });
    await settle();
    onchange.mockClear();
    button('Change server').click();
    flushSync();
    button('Continue').click();
    await settle();
    type(field('Password'), 'bad');
    flushSync();
    button('Trust and set up').click();
    await settle();
    expect(target.textContent).toContain('rejected');
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('ai_host_forget');
    expect(onreplaced).not.toHaveBeenCalled();
    expect(onchange).not.toHaveBeenCalled();
    button('Back').click();
    flushSync();
    button('Cancel').click();
    flushSync();
    expect(target.textContent).toContain('Server: vps:2222');
  });

  it('Cancel leaves Change server without touching anything', async () => {
    invoke.mockResolvedValue(STATUS);
    app = mount(AiHostPanel, { target, props: { host: OLD, onchange: vi.fn() } });
    await settle();
    invoke.mockClear();
    button('Change server').click();
    flushSync();
    button('Cancel').click();
    flushSync();
    expect(invoke).not.toHaveBeenCalled();
    expect(button('Change server')).toBeTruthy();
  });
});
