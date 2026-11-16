// @vitest-environment jsdom
import './__fixtures__/dialogStub';
// jsdom does not implement native keyboard activation or modal inertness.
// These tests assert event ownership/callbacks; the browser fixture covers native behavior.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import type { Writable } from 'svelte/store';

// Modal's presentation gate is `$isAndroid && $androidLayoutMode === 'phone'`;
// both are plain writables here so a test can flip them.
vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import ConfirmDialog from './ConfirmDialog.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const androidLayoutMode = layoutReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

let host: HTMLElement;
let opener: HTMLButtonElement;
let component: ReturnType<typeof mount>;
const confirm = vi.fn();
const cancel = vi.fn();
beforeEach(() => {
  vi.clearAllMocks();
  opener = document.createElement('button');
  document.body.append(opener);
  opener.focus();
  host = document.createElement('div');
  document.body.append(host);
  component = mount(ConfirmDialog, { target: host, props: {
    title: 'Delete fixture?', message: 'Synthetic data only', destructive: true,
    onConfirm: confirm, onCancel: cancel,
  } });
  flushSync();
});
afterEach(async () => {
  await dispose();
  host.remove(); opener.remove(); vi.restoreAllMocks();
});
async function dispose() { if (component) { await unmount(component); component = undefined!; } }
const buttons = () => host.querySelectorAll('button');
function key(target: Element, value: string, shiftKey = false) {
  const event = new KeyboardEvent('keydown', { key: value, shiftKey, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}
it('names the dialog and initially focuses Cancel', () => {
  const dialog = host.querySelector('dialog')!;
  const labelledBy = dialog.getAttribute('aria-labelledby')!;
  expect(host.querySelector(`#${labelledBy}`)!.textContent).toBe('Delete fixture?');
  expect(document.activeElement).toBe(buttons()[0]);
});
it.each(['Enter', ' '])('leaves %s activation to the focused Cancel button', (value) => {
  buttons()[0].focus();
  expect(key(buttons()[0], value).defaultPrevented).toBe(false);
  expect(confirm).not.toHaveBeenCalled();
  // Explicit click tests the callback only, not a simulated browser default action.
  buttons()[0].click();
  expect(cancel).toHaveBeenCalledTimes(1);
});
it('confirms once through the confirm button', () => {
  buttons()[1].click(); buttons()[1].click();
  expect(confirm).toHaveBeenCalledTimes(1);
  expect(cancel).not.toHaveBeenCalled();
});
it('owns Escape before a parent/menu window handler can run', () => {
  const parent = vi.fn(); window.addEventListener('keydown', parent);
  key(buttons()[0], 'Escape'); key(buttons()[0], 'Escape');
  window.removeEventListener('keydown', parent);
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(parent).not.toHaveBeenCalled();
  expect(confirm).not.toHaveBeenCalled();
});
it('wraps Tab and Shift+Tab between Cancel and Confirm', () => {
  buttons()[1].focus(); key(buttons()[1], 'Tab');
  expect(document.activeElement).toBe(buttons()[0]);
  key(buttons()[0], 'Tab', true);
  expect(document.activeElement).toBe(buttons()[1]);
});
it('restores focus on unmount', async () => {
  buttons()[0].focus(); await dispose();
  expect(document.activeElement).toBe(opener);
});
it('tolerates an opener removed while the modal is open', async () => {
  opener.remove(); await dispose();
  expect(document.activeElement?.isConnected).toBe(true);
});

it('handles the native cancel event once', () => {
  const dialog = host.querySelector('dialog')!;
  const event = new Event('cancel', { cancelable: true });
  dialog.dispatchEvent(event); dialog.dispatchEvent(new Event('cancel', { cancelable: true }));
  expect(event.defaultPrevented).toBe(true);
  expect(cancel).toHaveBeenCalledTimes(1);
});

// checkboxLabel/checkboxChecked (Sidebar's Managed-SSH removal revoke
// option): optional and additive. The main suite's mount above never passes
// them, so this also proves every other existing caller is unaffected.
it('renders no checkbox unless checkboxLabel is provided', () => {
  expect(host.querySelector('input[type="checkbox"]')).toBeNull();
});

it('renders a checked checkbox with its label when checkboxLabel/checkboxChecked are provided, and toggles it', () => {
  const checkboxHost = document.createElement('div');
  document.body.append(checkboxHost);
  const checkboxConfirm = vi.fn();
  const checkboxCancel = vi.fn();
  const checkboxComponent = mount(ConfirmDialog, { target: checkboxHost, props: {
    title: 'Remove account?',
    message: 'Remove ssh:jodd-zf from Jodd?',
    checkboxLabel: "Also remove Jodd's key from the server",
    checkboxChecked: true,
    onConfirm: checkboxConfirm,
    onCancel: checkboxCancel,
  } });
  flushSync();
  try {
    const checkbox = checkboxHost.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox).toBeTruthy();
    expect(checkbox.checked).toBe(true);
    expect(checkboxHost.textContent).toContain("Also remove Jodd's key from the server");
    checkbox.click();
    flushSync();
    expect(checkbox.checked).toBe(false);
  } finally {
    unmount(checkboxComponent);
    checkboxHost.remove();
  }
});

it('backdrop click routes to onCancel once', () => {
  const dialog = host.querySelector('dialog')!;
  dialog.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  dialog.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(confirm).not.toHaveBeenCalled();
});

it('keeps the destructive confirm as .danger and a neutral confirm as primary', async () => {
  expect(buttons()[1].classList.contains('danger')).toBe(true);
  const neutralHost = document.createElement('div');
  document.body.append(neutralHost);
  const neutral = mount(ConfirmDialog, { target: neutralHost, props: {
    title: 'Remove?', message: 'x', onConfirm: confirm, onCancel: cancel,
  } });
  flushSync();
  try {
    const confirmBtn = neutralHost.querySelectorAll('button')[1];
    expect(confirmBtn.classList.contains('danger')).toBe(false);
    expect(confirmBtn.classList.contains('primary')).toBe(true);
    expect(confirmBtn.textContent?.trim()).toBe('OK');
  } finally {
    await unmount(neutral);
    neutralHost.remove();
  }
});

it('desktop presentation is a small card, with no close button', () => {
  const dialog = host.querySelector('dialog')!;
  expect(dialog.dataset.presentation).toBe('card');
  expect(dialog.dataset.size).toBe('sm');
  expect(host.querySelector('button.close')).toBeNull();
});

it('phone presentation is alert', async () => {
  isAndroid.set(true);
  androidLayoutMode.set('phone');
  window.addEventListener('popstate', handlePopState);
  const phoneHost = document.createElement('div');
  document.body.append(phoneHost);
  const phone = mount(ConfirmDialog, { target: phoneHost, props: {
    title: 'Remove account?', message: 'Synthetic', onConfirm: confirm, onCancel: cancel,
  } });
  flushSync();
  try {
    const dialog = phoneHost.querySelector('dialog')!;
    expect(dialog.dataset.presentation).toBe('alert');
    expect(dialog.dataset.size).toBe('sm');
    expect(document.activeElement).toBe(phoneHost.querySelectorAll('button')[0]);
    // On tablet the same dialog is a card.
    androidLayoutMode.set('tablet');
    flushSync();
    expect(dialog.dataset.presentation).toBe('card');
  } finally {
    await unmount(phone);
    phoneHost.remove();
    // The modal's overlay history entry is popped asynchronously.
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
    window.removeEventListener('popstate', handlePopState);
    isAndroid.set(false);
    androidLayoutMode.set('phone');
  }
});
