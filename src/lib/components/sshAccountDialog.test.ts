// @vitest-environment jsdom
//
// SshAccountDialog renders through the shared Modal: Esc, the backdrop and the
// header close are Modal's; "Add" is the one primary. The dialog keeps its
// form and the browse / submit calls.
import './__fixtures__/dialogStub';
import { beforeEach, afterEach, describe, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import SshAccountDialog from './SshAccountDialog.svelte';

let host: HTMLElement;
let component: ReturnType<typeof mount>;
const onAdded = vi.fn();
const onClose = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  invoke.mockReset();
  invoke.mockImplementation(async (cmd: string) => {
    if (cmd === 'platform_name') return 'macos';
    if (cmd === 'add_ssh_account') return { id: 'ssh:new', email: 'box:notes' };
    if (cmd === 'list_remote_dirs') return { path: '/home/me', dirs: ['notes'] };
    throw new Error(`unexpected invoke ${cmd}`);
  });
  host = document.createElement('div');
  document.body.append(host);
  component = mount(SshAccountDialog, { target: host, props: { onAdded, onClose } });
  flushSync();
});
afterEach(() => { unmount(component); host.remove(); });

const dialog = () => host.querySelector('dialog')!;
const button = (text: RegExp) =>
  [...dialog().querySelectorAll<HTMLButtonElement>('button')].find((b) => text.test(b.textContent?.trim() ?? ''))!;
function fill(placeholder: string, value: string) {
  const el = dialog().querySelector<HTMLInputElement>(`input[placeholder="${placeholder}"]`)!;
  el.value = value;
  el.dispatchEvent(new Event('input'));
}
const callsExceptPlatform = () => invoke.mock.calls.filter((c) => c[0] !== 'platform_name');

describe('SshAccountDialog as a Modal', () => {
  it('is an md Modal titled Add SSH server, with Add as its one primary', () => {
    expect(dialog().hasAttribute('data-presentation')).toBe(true);
    expect(dialog().dataset.size).toBe('md');
    expect(dialog().querySelector('h2')!.textContent).toBe('Add SSH server');
    expect(dialog().querySelectorAll('.btn.primary')).toHaveLength(1);
    expect(button(/^Add$/).classList.contains('primary')).toBe(true);
  });

  it('Esc closes', () => {
    dialog().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('has no footer Cancel: the header close is the one close control', () => {
    expect(button(/^Cancel$/)).toBeUndefined();
    dialog().querySelector<HTMLButtonElement>('button[aria-label="Close"]')!.click();
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Add without a folder explains instead of connecting', () => {
    fill('me@host or an ~/.ssh/config alias', 'me@box');
    flushSync();
    button(/^Add$/).click();
    flushSync();
    expect(dialog().querySelector('[role="alert"]')!.textContent).toBe('Choose a directory on the server.');
    expect(callsExceptPlatform()).toHaveLength(0);
  });

  it('Add with a server and folder sends add_ssh_account and reports the account', async () => {
    fill('me@host or an ~/.ssh/config alias', 'me@box');
    fill('~/notes', '~/notes');
    flushSync();
    button(/^Add$/).click();
    for (let i = 0; i < 5; i++) await tick();
    expect(invoke).toHaveBeenCalledWith('add_ssh_account', expect.objectContaining({ target: 'me@box', root: '~/notes' }));
    expect(onAdded).toHaveBeenCalledWith(expect.objectContaining({ id: 'ssh:new' }));
  });

  it('Browse lists folders, and Use this folder fills the field', async () => {
    fill('me@host or an ~/.ssh/config alias', 'me@box');
    flushSync();
    button(/^Browse…$/).click();
    for (let i = 0; i < 5; i++) await tick();
    expect(invoke).toHaveBeenCalledWith('list_remote_dirs', { target: 'me@box', path: '~' });
    button(/^Use this folder$/).click();
    flushSync();
    expect(dialog().querySelector<HTMLInputElement>('input[placeholder="~/notes"]')!.value).toBe('/home/me');
  });
});
