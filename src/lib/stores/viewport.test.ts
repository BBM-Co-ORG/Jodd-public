// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { get, writable } from 'svelte/store';

// keyboardInset is gated on isAndroid, which is backed by a Tauri invoke
// (platform.ts) that does not resolve under vitest. A writable stands in.
vi.mock('./platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});

import {
  deriveAndroidLayoutMode,
  ANDROID_TABLET_BREAKPOINT,
  ANDROID_TABLET_WIDE_BREAKPOINT,
  computeKeyboardInset,
  keyboardInset,
  installKeyboardInsetVar,
} from './viewport';
import { isAndroid as isAndroidReadonly } from './platform';
const isAndroid = isAndroidReadonly as unknown as ReturnType<typeof writable<boolean>>;

describe('deriveAndroidLayoutMode', () => {
  it('is phone below the tablet breakpoint', () => {
    expect(deriveAndroidLayoutMode(ANDROID_TABLET_BREAKPOINT - 1)).toBe('phone');
  });

  it('is tablet at or above the tablet breakpoint', () => {
    expect(deriveAndroidLayoutMode(ANDROID_TABLET_BREAKPOINT)).toBe('tablet');
  });

  it('is phone for a narrow phone width', () => {
    expect(deriveAndroidLayoutMode(360)).toBe('phone');
  });

  it('keeps the breakpoint values the layout depends on', () => {
    expect(ANDROID_TABLET_BREAKPOINT).toBe(700);
    expect(ANDROID_TABLET_WIDE_BREAKPOINT).toBe(1024);
  });

  it.each([
    [699, 'phone'],
    [700, 'tablet'],
    [1023, 'tablet'],
    [1024, 'tabletWide'],
  ])('%i → %s', (w, mode) => {
    expect(deriveAndroidLayoutMode(w)).toBe(mode);
  });
});

describe('computeKeyboardInset', () => {
  it('computes the covered height from the visual viewport', () => {
    expect(computeKeyboardInset(800, null)).toBe(0);
    expect(computeKeyboardInset(800, { height: 500, offsetTop: 0 })).toBe(300);
    expect(computeKeyboardInset(500, { height: 500, offsetTop: 0 })).toBe(0); // adjustResize case
    expect(computeKeyboardInset(800, { height: 900, offsetTop: 0 })).toBe(0); // never negative
  });

  it('subtracts offsetTop, since a scrolled visual viewport is not a keyboard', () => {
    expect(computeKeyboardInset(800, { height: 500, offsetTop: 100 })).toBe(200);
  });
});

function mockVisualViewport(height: number, offsetTop = 0) {
  const target = new EventTarget() as EventTarget & { height: number; offsetTop: number };
  target.height = height;
  target.offsetTop = offsetTop;
  Object.defineProperty(window, 'visualViewport', { configurable: true, value: target });
  return target;
}

describe('keyboardInset', () => {
  afterEach(() => {
    // jsdom has no visualViewport; remove our stand-in.
    delete (window as unknown as Record<string, unknown>).visualViewport;
    isAndroid.set(false);
    document.documentElement.style.removeProperty('--kb-inset');
  });

  it('is 0 off Android even when the visual viewport is shrunk', () => {
    window.innerHeight = 800;
    mockVisualViewport(400);
    isAndroid.set(false);
    expect(get(keyboardInset)).toBe(0);
  });

  it('is 0 on Android when window.visualViewport is unavailable', () => {
    isAndroid.set(true);
    expect(get(keyboardInset)).toBe(0);
  });

  it('tracks the visual viewport on Android', () => {
    window.innerHeight = 800;
    const vv = mockVisualViewport(800);
    isAndroid.set(true);
    const seen: number[] = [];
    const unsub = keyboardInset.subscribe((v) => seen.push(v));
    expect(seen.at(-1)).toBe(0);
    vv.height = 500;
    vv.dispatchEvent(new Event('resize'));
    expect(seen.at(-1)).toBe(300);
    vv.height = 800;
    vv.dispatchEvent(new Event('resize'));
    expect(seen.at(-1)).toBe(0);
    unsub();
  });

  it('installKeyboardInsetVar writes --kb-inset and stops on cleanup', () => {
    window.innerHeight = 800;
    const vv = mockVisualViewport(500);
    isAndroid.set(true);
    const stop = installKeyboardInsetVar();
    const root = document.documentElement;
    expect(root.style.getPropertyValue('--kb-inset')).toBe('300px');
    vv.height = 700;
    vv.dispatchEvent(new Event('resize'));
    expect(root.style.getPropertyValue('--kb-inset')).toBe('100px');
    stop();
    vv.height = 400;
    vv.dispatchEvent(new Event('resize'));
    expect(root.style.getPropertyValue('--kb-inset')).toBe('100px');
  });
});
