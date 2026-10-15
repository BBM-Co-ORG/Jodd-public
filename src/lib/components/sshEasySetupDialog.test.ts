// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import SshEasySetupDialog from './SshEasySetupDialog.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

let host: HTMLElement;
let component: ReturnType<typeof mount>;
const onAdded = vi.fn();
const onClose = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  host = document.createElement('div');
  document.body.append(host);
});
afterEach(() => { unmount(component); host.remove(); vi.restoreAllMocks(); });

it('walks address -> fingerprint -> trust -> progress -> done', async () => {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'ssh_fingerprint') return { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'box ssh-ed25519 AAAA' };
    if (cmd === 'ssh_setup_managed') return { id: 'ssh:new', email: 'box:Jodd-Notes' };
    throw new Error(`unexpected invoke ${cmd}`);
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();

  const hostInput = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-host"]')!;
  const userInput = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-user"]')!;
  const passwordInput = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-password"]')!;
  hostInput.value = 'box.example.com'; hostInput.dispatchEvent(new Event('input'));
  userInput.value = 'me'; userInput.dispatchEvent(new Event('input'));
  passwordInput.value = 'hunter2'; passwordInput.dispatchEvent(new Event('input'));
  flushSync();

  host.querySelector<HTMLButtonElement>('[data-testid="easy-ssh-continue"]')!.click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();

  expect(invoke).toHaveBeenCalledWith('ssh_fingerprint', { host: 'box.example.com', port: null });
  expect(host.textContent).toContain('SHA256:abc');

  host.querySelector<HTMLButtonElement>('[data-testid="easy-ssh-trust"]')!.click();
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  flushSync();

  expect(invoke).toHaveBeenCalledWith('ssh_setup_managed', expect.objectContaining({
    host: 'box.example.com', port: null, user: 'me',
    credential: { kind: 'Password', password: 'hunter2' },
    knownHostsLine: 'box ssh-ed25519 AAAA',
    root: '~/Jodd-Notes',
  }));
  expect(onAdded).toHaveBeenCalledWith(expect.objectContaining({ id: 'ssh:new' }));
});

it('sends the notes folder the user typed, so one server can host several vaults', async () => {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'ssh_fingerprint') return { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'box ssh-ed25519 AAAA' };
    if (cmd === 'ssh_setup_managed') return { id: 'ssh:new', email: 'box:Work' };
    throw new Error(`unexpected invoke ${cmd}`);
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  const set = (id: string, v: string) => {
    const el = host.querySelector<HTMLInputElement>(`[data-testid="${id}"]`)!;
    el.value = v; el.dispatchEvent(new Event('input'));
  };
  const root = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-root"]')!;
  expect(root.value).toBe('~/Jodd-Notes');
  set('easy-ssh-host', 'box.example.com');
  set('easy-ssh-user', 'me');
  set('easy-ssh-password', 'hunter2');
  set('easy-ssh-root', '  ~/Work  ');
  flushSync();
  host.querySelector<HTMLButtonElement>('[data-testid="easy-ssh-continue"]')!.click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();
  host.querySelector<HTMLButtonElement>('[data-testid="easy-ssh-trust"]')!.click();
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  flushSync();
  expect(invoke).toHaveBeenCalledWith('ssh_setup_managed', expect.objectContaining({ root: '~/Work' }));
});

it('shows a plain error and lets the user go back on a fingerprint failure', async () => {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    throw "Can't reach the server. Check the address, and that the server is running.";
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  const hostInput = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-host"]')!;
  hostInput.value = 'nope.invalid'; hostInput.dispatchEvent(new Event('input'));
  flushSync();
  host.querySelector<HTMLButtonElement>('[data-testid="easy-ssh-continue"]')!.click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();
  expect(host.textContent).toContain("Can't reach the server");
  expect(onAdded).not.toHaveBeenCalled();
});
