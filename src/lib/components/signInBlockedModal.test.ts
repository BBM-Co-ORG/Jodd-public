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

import SignInBlocked from './SignInBlocked.svelte';
import { signInBlocked } from '../stores/ui';

const URL = 'https://login.microsoftonline.com/organizations/adminconsent?client_id=X';

let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  signInBlocked.set(null);
  host = document.createElement('div');
  document.body.append(host);
  component = mount(SignInBlocked, { target: host });
  flushSync();
});
afterEach(async () => {
  await unmount(component);
  host.remove();
  vi.restoreAllMocks();
});

async function show(block: { message: string; adminConsentUrl: string | null }) {
  signInBlocked.set(block);
  await tick();
  flushSync();
}
const dialog = () => host.querySelector('dialog');
const closers = () =>
  [...host.querySelectorAll('button')].filter(
    (b) => b.textContent?.trim() === 'Close' || b.getAttribute('aria-label') === 'Close',
  );

it('renders nothing while there is no block', () => {
  expect(dialog()).toBeNull();
});

it('admin-consent variant: a Modal titled for the approval, with the link and both copy buttons', async () => {
  await show({ message: 'Sign-in was not completed.', adminConsentUrl: URL });
  expect(dialog()!.hasAttribute('data-presentation')).toBe(true);
  expect(host.querySelector('h2')!.textContent).toBe("Sign-in needs your organisation's approval");
  expect(host.querySelector('code')!.textContent).toBe(URL);
  const labels = [...host.querySelectorAll('button')].map((b) => b.textContent?.trim());
  expect(labels).toContain('Copy link');
  expect(labels).toContain('Copy message for IT');
});

it('plain variant: a Modal titled "Sign-in was not completed" showing the message', async () => {
  await show({ message: 'It went wrong.', adminConsentUrl: null });
  expect(host.querySelector('h2')!.textContent).toBe('Sign-in was not completed');
  expect(host.textContent).toContain('It went wrong.');
});

it('Esc clears the store', async () => {
  await show({ message: 'm', adminConsentUrl: URL });
  dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await tick();
  flushSync();
  expect(get(signInBlocked)).toBeNull();
  expect(dialog()).toBeNull();
});

it('keeps exactly one close control in both variants', async () => {
  await show({ message: 'm', adminConsentUrl: URL });
  expect(closers()).toHaveLength(1);
  await show({ message: 'm', adminConsentUrl: null });
  expect(closers()).toHaveLength(1);
});

it('Copy link writes the link to the clipboard and confirms', async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
  await show({ message: 'm', adminConsentUrl: URL });
  [...host.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Copy link')!.click();
  await vi.waitFor(() => expect(host.textContent).toContain('Copied'));
  expect(writeText).toHaveBeenCalledWith(URL);
});
