// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import SshEasySetupDialog from './SshEasySetupDialog.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

let host: HTMLElement;
let component: ReturnType<typeof mount>;
const onAdded = vi.fn();
const onClose = vi.fn();

// Modal owns the primary button, so it is found by its label, not a test id.
const primaryButton = (label: string) =>
  [...host.querySelectorAll<HTMLButtonElement>('button.btn.primary')].find((b) => b.textContent?.trim() === label)!;

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

  primaryButton('Continue').click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();

  expect(invoke).toHaveBeenCalledWith('ssh_fingerprint', { host: 'box.example.com', port: null });
  expect(host.textContent).toContain('SHA256:abc');

  primaryButton('Trust and connect').click();
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
  primaryButton('Continue').click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();
  primaryButton('Trust and connect').click();
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
  primaryButton('Continue').click();
  await Promise.resolve(); await Promise.resolve();
  flushSync();
  expect(host.textContent).toContain("Can't reach the server");
  expect(onAdded).not.toHaveBeenCalled();
});

// ---- Modal: Esc, the header close, and the progress-step guard --------------

const dialogs = () => [...document.querySelectorAll('dialog')] as HTMLDialogElement[];
const outer = () => dialogs()[0];
const guard = () => dialogs()[1];
const esc = (d: Element) => {
  d.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  flushSync();
};
const buttonIn = (root: Element, text: string) =>
  [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === text) as HTMLButtonElement;

function fillConnect() {
  const set = (id: string, v: string) => {
    const el = host.querySelector<HTMLInputElement>(`[data-testid="${id}"]`)!;
    el.value = v; el.dispatchEvent(new Event('input'));
  };
  set('easy-ssh-host', 'box.example.com');
  set('easy-ssh-user', 'me');
  set('easy-ssh-password', 'hunter2');
  flushSync();
}

/** Mounts and walks to the progress step; ssh_setup_managed stays pending until `finish`. */
async function mountAtProgress() {
  let finish!: (account: unknown) => void;
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'ssh_fingerprint') return { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'box ssh-ed25519 AAAA' };
    if (cmd === 'ssh_setup_managed') return new Promise((resolve) => { finish = resolve; });
    throw new Error(`unexpected invoke ${cmd}`);
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  fillConnect();
  buttonIn(outer(), 'Continue').click();
  for (let i = 0; i < 4; i++) await tick();
  buttonIn(outer(), 'Trust and connect').click();
  for (let i = 0; i < 4; i++) await tick();
  flushSync();
  return { finish: (a: unknown) => finish(a) };
}

it('is an md Modal; each screen is titled, with one primary and no footer Cancel', async () => {
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'ssh_fingerprint') return { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'box ssh-ed25519 AAAA' };
    throw new Error(`unexpected invoke ${cmd}`);
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  expect(outer().hasAttribute('data-presentation')).toBe(true);
  expect(outer().dataset.size).toBe('md');
  expect(outer().querySelector('h2')!.textContent).toBe('Connect to your server');
  expect(outer().querySelectorAll('.btn.primary')).toHaveLength(1);
  expect(buttonIn(outer(), 'Cancel')).toBeUndefined();

  fillConnect();
  buttonIn(outer(), 'Continue').click();
  for (let i = 0; i < 4; i++) await tick();
  flushSync();
  expect(outer().querySelector('h2')!.textContent).toBe('Is this your server?');
  expect(buttonIn(outer(), 'Back')).toBeTruthy();
  expect(outer().querySelectorAll('.btn.primary')).toHaveLength(1);
});

it('Esc closes on the connect screen, without a guard', () => {
  invoke.mockImplementation(async () => 'macos');
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  esc(outer());
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(guard()).toBeUndefined();
});

it('during progress the dialog cannot be closed: Esc, the backdrop and the close button do nothing', async () => {
  await mountAtProgress();
  expect(outer().querySelector('h2')!.textContent).toBe('Setting up your account');
  esc(outer());
  outer().click(); // backdrop
  const close = outer().querySelector<HTMLButtonElement>('button[aria-label="Close"]')!;
  expect(close.getAttribute('aria-disabled')).toBe('true');
  close.click();
  flushSync();
  expect(onClose).not.toHaveBeenCalled();
  expect(guard()).toBeUndefined();
  expect(outer().open).toBe(true);
  expect(onAdded).not.toHaveBeenCalled();
});

it('after setup completes, onAdded fires once and the dialog was never closed meanwhile', async () => {
  const { finish } = await mountAtProgress();
  esc(outer());
  expect(onClose).not.toHaveBeenCalled();
  finish({ id: 'ssh:new', email: 'box:Jodd-Notes' });
  for (let i = 0; i < 4; i++) await tick();
  flushSync();
  expect(onAdded).toHaveBeenCalledTimes(1);
  expect(onAdded).toHaveBeenCalledWith(expect.objectContaining({ id: 'ssh:new' }));
  expect(onClose).not.toHaveBeenCalled();
});

it('a failed setup unlocks the dialog: back on the trust screen, Esc closes again', async () => {
  let fail!: (e: unknown) => void;
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'ssh_fingerprint') return { algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'box ssh-ed25519 AAAA' };
    if (cmd === 'ssh_setup_managed') return new Promise((_, reject) => { fail = reject; });
    throw new Error(`unexpected invoke ${cmd}`);
  });
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
  fillConnect();
  buttonIn(outer(), 'Continue').click();
  for (let i = 0; i < 4; i++) await tick();
  buttonIn(outer(), 'Trust and connect').click();
  for (let i = 0; i < 4; i++) await tick();
  esc(outer());
  expect(onClose).not.toHaveBeenCalled();
  fail('Permission denied');
  for (let i = 0; i < 4; i++) await tick();
  flushSync();
  expect(outer().querySelector('h2')!.textContent).toBe('Is this your server?');
  expect(outer().querySelector('[role="alert"]')!.textContent).toBe('Permission denied');
  esc(outer());
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(guard()).toBeUndefined();
});
