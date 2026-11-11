// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('9.9.9') }));
const openUrl = vi.fn();
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: (...a: unknown[]) => openUrl(...a) }));
vi.mock('../whatsNew', () => ({ whatsNewForCurrentVersion: vi.fn().mockResolvedValue([]) }));

import About from './About.svelte';
import { aboutModalOpen, whatsNewOpen } from '../stores/ui';

let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  aboutModalOpen.set(false);
  whatsNewOpen.set(false);
  host = document.createElement('div');
  document.body.append(host);
  component = mount(About, { target: host });
  flushSync();
});
afterEach(async () => {
  await unmount(component);
  host.remove();
  vi.clearAllMocks();
});

async function openAbout() {
  aboutModalOpen.set(true);
  await tick();
  flushSync();
}
const dialog = () => host.querySelector('dialog');

it('renders nothing while closed', () => {
  expect(dialog()).toBeNull();
});

it('opens as a Modal titled Jodd, with the version', async () => {
  await openAbout();
  expect(dialog()!.hasAttribute('data-presentation')).toBe(true);
  expect(host.querySelector('h2')!.textContent).toBe('Jodd');
  await vi.waitFor(() => expect(host.textContent).toContain('Version 9.9.9'));
});

it('Esc closes through the store', async () => {
  await openAbout();
  dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await tick();
  flushSync();
  expect(get(aboutModalOpen)).toBe(false);
  expect(dialog()).toBeNull();
});

it('keeps exactly one close control: the Modal header, not a second Close button', async () => {
  await openAbout();
  const closers = [...host.querySelectorAll('button')].filter(
    (b) => b.textContent?.trim() === 'Close' || b.getAttribute('aria-label') === 'Close',
  );
  expect(closers).toHaveLength(1);
});

it("What's New hands over to the release notes", async () => {
  await openAbout();
  [...host.querySelectorAll('button')].find((b) => b.textContent === "What's New")!.click();
  await vi.waitFor(() => expect(get(whatsNewOpen)).toBe(true));
  expect(get(aboutModalOpen)).toBe(false);
});

it('Privacy opens the privacy page', async () => {
  await openAbout();
  [...host.querySelectorAll('button')].find((b) => b.textContent === 'Privacy')!.click();
  expect(openUrl).toHaveBeenCalledWith('https://jodd.bbmedia.co.th/privacy.html');
});
