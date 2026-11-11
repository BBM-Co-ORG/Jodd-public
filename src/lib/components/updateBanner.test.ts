// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false), isMacos: writable(false) };
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue({ current: '0.46.0', available: null }) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn().mockResolvedValue(() => {}) }));
const openUrl = vi.fn().mockResolvedValue(undefined);
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: (...a: unknown[]) => openUrl(...a) }));

import UpdateBanner from './UpdateBanner.svelte';
import { updateState, type AvailableUpdate } from '../appUpdate';

const BASE: AvailableUpdate = {
  version: '0.47.0',
  size: 12_000_000,
  releaseUrl: 'https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.47.0',
  method: 'desktopInstaller',
};

let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  try { localStorage.clear(); } catch { /* jsdom */ }
  host = document.createElement('div');
  document.body.append(host);
  component = mount(UpdateBanner, { target: host });
  flushSync();
});
afterEach(async () => {
  await unmount(component);
  host.remove();
  updateState.set({ phase: 'idle' });
  vi.clearAllMocks();
});

const buttons = () => [...host.querySelectorAll('button')].map((b) => b.textContent?.trim());

it('a release-page update shows Download and opens the release', async () => {
  updateState.set({ phase: 'available', update: { ...BASE, size: null, method: 'releasePage' } });
  await tick();
  flushSync();
  expect(host.textContent).toContain('Jodd 0.47.0 is available.');
  expect(buttons()).toEqual(['Download', 'Later']);
  host.querySelector('button')!.click();
  await vi.waitFor(() => expect(openUrl).toHaveBeenCalledWith(BASE.releaseUrl));
});

it('a desktop update offers Install and restart', async () => {
  updateState.set({ phase: 'available', update: BASE });
  await tick();
  flushSync();
  expect(buttons()).toEqual(['Install and restart', 'Later']);
});

it('says Jodd is closing while the installer runs', async () => {
  updateState.set({ phase: 'restarting', update: BASE });
  await tick();
  flushSync();
  expect(host.textContent).toContain('Installing — Jodd will close and reopen.');
  expect(buttons()).toEqual([]);
});
