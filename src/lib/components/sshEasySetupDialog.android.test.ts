// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { readable } from 'svelte/store';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../stores/platform', () => ({ isAndroid: readable(true) }));

import SshEasySetupDialog from './SshEasySetupDialog.svelte';

let component: ReturnType<typeof mount>;
const host = document.createElement('div');
document.body.append(host);
afterEach(() => unmount(component));

it('offers only Password on Android: no Key file choice', () => {
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded: vi.fn(), onClose: vi.fn() } });
  flushSync();
  expect(host.querySelector('[data-testid="easy-ssh-password"]')).not.toBeNull();
  expect(host.querySelector('input[type="radio"][value="keyfile"]')).toBeNull();
  expect(host.textContent).not.toContain('Key file');
});

it('offers a notes-folder field on Android too, where Advanced setup is hidden', () => {
  component = mount(SshEasySetupDialog, { target: host, props: { onAdded: vi.fn(), onClose: vi.fn() } });
  flushSync();
  const root = host.querySelector<HTMLInputElement>('[data-testid="easy-ssh-root"]');
  expect(root?.value).toBe('~/Jodd-Notes');
});
