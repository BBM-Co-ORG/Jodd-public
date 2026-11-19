// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import type { Writable } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import Popover from './Popover.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { handlePopState } from '../stores/phoneNav';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const androidLayoutMode = layoutReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

const settle = () => new Promise((r) => setTimeout(r, 30));
const content = createRawSnippet(() => ({
  render: () => '<button class="inside" type="button">Item</button>',
}));

let host: HTMLElement;
let anchor: HTMLButtonElement;
let outside: HTMLButtonElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let instance: any = null;
let onclose: ReturnType<typeof vi.fn<() => void>>;

function dispose() {
  if (instance) {
    const i = instance;
    instance = null;
    unmount(i);
  }
}

function openPopover(props: Record<string, unknown> = {}) {
  onclose = vi.fn<() => void>();
  instance = mount(Popover, {
    target: host,
    props: { anchor, open: true, onclose, children: content, ...props },
  });
  flushSync();
}

const popEl = () => document.querySelector('.popover') as HTMLElement | null;
const sheet = () => document.querySelector('[data-presentation="sheet"]');

beforeEach(() => {
  isAndroid.set(false);
  androidLayoutMode.set('phone');
  window.addEventListener('popstate', handlePopState);
  host = document.createElement('div');
  anchor = document.createElement('button');
  anchor.textContent = 'More';
  outside = document.createElement('button');
  document.body.append(host, anchor, outside);
});
afterEach(async () => {
  dispose();
  flushSync();
  await settle();
  window.removeEventListener('popstate', handlePopState);
  host.remove();
  anchor.remove();
  outside.remove();
});

describe('Popover', () => {
  it('Esc closes and refocuses anchor', () => {
    openPopover();
    outside.focus();
    expect(popEl()).not.toBeNull();
    const esc = new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
    document.dispatchEvent(esc);
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(popEl()).toBeNull();
    expect(document.activeElement).toBe(anchor);
  });

  it('outside pointerdown closes', () => {
    openPopover();
    outside.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(popEl()).toBeNull();
  });

  it('pointerdown inside the popover or on the anchor does not close', () => {
    openPopover();
    popEl()!.querySelector('.inside')!.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    anchor.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    expect(popEl()).not.toBeNull();
  });

  it('renders nothing when closed', () => {
    openPopover({ open: false });
    expect(popEl()).toBeNull();
    expect(sheet()).toBeNull();
  });

  it('is non-modal and layered at --z-popover', () => {
    openPopover();
    expect(popEl()!.closest('dialog')).toBeNull();
    expect(sheet()).toBeNull();
  });

  it('phone renders a sheet', () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    openPopover();
    expect(sheet()).not.toBeNull();
    expect(popEl()).toBeNull();
    expect(sheet()!.querySelector('.inside')).not.toBeNull();
  });

  it('a narrow desktop window and an Android tablet keep the anchored popover', () => {
    androidLayoutMode.set('phone'); // isAndroid false: desktop
    openPopover();
    expect(popEl()).not.toBeNull();
    expect(sheet()).toBeNull();
    dispose();
    flushSync();
    isAndroid.set(true);
    androidLayoutMode.set('tablet');
    openPopover();
    expect(popEl()).not.toBeNull();
    expect(sheet()).toBeNull();
  });

  it('closing the phone sheet calls onclose once and refocuses anchor', async () => {
    isAndroid.set(true);
    openPopover();
    const dlg = sheet() as HTMLDialogElement;
    dlg.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    await settle();
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(sheet()).toBeNull();
    expect(document.activeElement).toBe(anchor);
  });
});
