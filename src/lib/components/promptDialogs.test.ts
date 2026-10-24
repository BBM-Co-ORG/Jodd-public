// @vitest-environment jsdom
//
// PromptDialogs' confirm: a destructive one (remove account, delete folder or
// tag) names its verb and is danger-styled — never a primary "OK" (final
// review 2026-10-20; copy style: a destructive action is never the primary).
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import PromptDialogs from './PromptDialogs.svelte';

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let dialogs: any;

beforeEach(() => {
  host = document.createElement('div');
  document.body.append(host);
  dialogs = mount(PromptDialogs, { target: host, props: {} });
  flushSync();
});
afterEach(async () => {
  await unmount(dialogs);
  host.remove();
});

function confirmButton(): HTMLButtonElement {
  const buttons = host.querySelectorAll<HTMLButtonElement>('dialog button');
  return [...buttons].find((b) => b.textContent?.trim() !== 'Cancel' && !b.getAttribute('aria-label'))!;
}

it('a plain confirm keeps the primary "OK"', async () => {
  const answer = dialogs.askConfirm('Notes in this folder are not encrypted', 'Plain files.');
  await tick(); flushSync();
  const ok = confirmButton();
  expect(ok.textContent?.trim()).toBe('OK');
  expect(ok.classList.contains('primary')).toBe(true);
  expect(ok.classList.contains('danger')).toBe(false);
  ok.click();
  await expect(answer).resolves.toBe(true);
});

it('a destructive confirm is danger-styled and labelled with its verb', async () => {
  const answer = dialogs.askConfirm('Remove account?', 'Removes it.', null, { destructive: true, confirmLabel: 'Remove' });
  await tick(); flushSync();
  const remove = confirmButton();
  expect(remove.textContent?.trim()).toBe('Remove');
  expect(remove.classList.contains('danger')).toBe(true);
  expect(remove.classList.contains('primary')).toBe(false);
  remove.click();
  await expect(answer).resolves.toBe(true);
});

it('the options do not leak into the next confirm', async () => {
  const first = dialogs.askConfirm('Delete tag?', 'x', null, { destructive: true, confirmLabel: 'Delete' });
  await tick(); flushSync();
  confirmButton().click();
  await first;
  await tick(); flushSync();
  const second = dialogs.askConfirm('Stop waiting?', 'y');
  await tick(); flushSync();
  const ok = confirmButton();
  expect(ok.textContent?.trim()).toBe('OK');
  expect(ok.classList.contains('primary')).toBe(true);
  ok.click();
  await second;
});
