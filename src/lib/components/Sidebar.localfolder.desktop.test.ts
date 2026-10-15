// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readable } from 'svelte/store';
import { accounts, currentAccount } from '../stores/notes';

const invoke = vi.fn((cmd: string) => Promise.resolve(cmd === 'count_pending_pushes' ? { notes: 0, deletes: 0, pins: 0, folders: 0 } : []));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...(a as [string])) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() }));
vi.mock('../stores/platform', () => ({ isAndroid: readable(false), supportsIcloud: readable(true) }));

import Sidebar from './Sidebar.svelte';

let component: ReturnType<typeof mount>;
afterEach(() => unmount(component));

it('on desktop, Add Local Folder still opens the desktop folder dialog', async () => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  accounts.set([{ id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set('gmail:a@b.com');
  const host = document.createElement('div');
  document.body.append(host);
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  host.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')!.click();
  flushSync();
  [...host.querySelectorAll<HTMLButtonElement>('.account-panel-action')]
    .find((b) => b.textContent?.includes('Add Local Folder'))!.click();
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  expect(open).toHaveBeenCalledWith(expect.objectContaining({ directory: true }));
  expect(invoke).not.toHaveBeenCalledWith('pick_local_folder');
});
