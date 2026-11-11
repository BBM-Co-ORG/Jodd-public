// @vitest-environment jsdom
//
// The unlock screen shown when the local cache did not open (2026-10-26: an
// unanswered Keychain prompt). Before it existed the app ran in full beside a
// throwaway cache and a Remove click deleted a real account's refresh token.
// What this pins: the screen offers Try again and nothing else, and Try again
// is the one command the locked backend still runs.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readFileSync } from 'node:fs';
import CacheLockedScreen from './CacheLockedScreen.svelte';
import { readCacheLock, cacheLockCopy, type CacheLock } from '../cacheLock';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

const KEYCHAIN: CacheLock = {
  kind: 'keychain',
  detail: 'keychain error: keychain read: Unable to obtain authorization for this operation',
};

function render(lock: CacheLock) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(CacheLockedScreen, { target, props: { lock } });
  flushSync();
  return { target, host };
}

describe('CacheLockedScreen', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  it('says the notes could not be unlocked, and that nothing was changed', () => {
    const { target, host } = render(KEYCHAIN);
    const text = target.textContent ?? '';
    expect(text).toContain("Couldn't unlock your notes");
    expect(text).toContain('timed out or been denied');
    expect(text).toContain('Nothing has been changed or deleted');
    expect(text).toContain('Unable to obtain authorization'); // the detail
    unmount(host);
  });

  it('invokes nothing on mount, and offers exactly one button', () => {
    const { target, host } = render(KEYCHAIN);
    expect(invoke).not.toHaveBeenCalled();
    const buttons = [...target.querySelectorAll('button')];
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(['Try again']);
    unmount(host);
  });

  it('Try again calls retry_cache_unlock and nothing else', async () => {
    invoke.mockReturnValue(new Promise(() => {})); // the app restarts; never resolves
    const { target, host } = render(KEYCHAIN);
    const btn = target.querySelector('button')!;
    btn.click();
    flushSync();
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('retry_cache_unlock');
    expect(btn.disabled).toBe(true);
    unmount(host);
  });

  it('a failed restart re-enables Try again and says what to do', async () => {
    invoke.mockRejectedValue('restart failed');
    const { target, host } = render(KEYCHAIN);
    target.querySelector('button')!.click();
    await tick();
    await tick();
    flushSync();
    expect(target.querySelector('button')!.disabled).toBe(false);
    expect(target.textContent).toContain('Quit Jodd and open it again');
    unmount(host);
  });

  it('a non-keychain failure gets its own wording', () => {
    const { target, host } = render({ kind: 'open_failed', detail: 'sqlite error: disk I/O' });
    expect(target.textContent).toContain("Couldn't open your notes");
    expect(target.textContent).not.toContain('keychain prompt');
    unmount(host);
  });
});

describe('readCacheLock', () => {
  it('is null when the backend announced no lock', () => {
    expect(readCacheLock({})).toBeNull();
  });

  it('reads the lock the init script set', () => {
    expect(readCacheLock({ __JODD_CACHE_LOCK__: KEYCHAIN })).toEqual(KEYCHAIN);
  });

  it('fails closed on a malformed value: still locked', () => {
    expect(readCacheLock({ __JODD_CACHE_LOCK__: 'garbage' })).toEqual({
      kind: 'open_failed',
      detail: '',
    });
  });

  it('every kind has a Try again', () => {
    expect(cacheLockCopy('keychain').action).toBe('Try again');
    expect(cacheLockCopy('open_failed').action).toBe('Try again');
  });
});

// main.ts cannot be imported in a test (it mounts into #app on load), so pin
// its decision by source: the lock is read before App mounts, and App is not
// mounted at all when it is set.
describe('main.ts', () => {
  it('mounts the unlock screen instead of App when the cache is locked', () => {
    const src = readFileSync('src/main.ts', 'utf8').replace(/\/\/.*$/gm, '');
    const read = src.indexOf('readCacheLock()');
    const mountLocked = src.indexOf('mount(CacheLockedScreen');
    const mountApp = src.indexOf('mount(App');
    expect(read).toBeGreaterThan(-1);
    expect(mountLocked).toBeGreaterThan(read);
    expect(mountApp).toBeGreaterThan(read);
    // One ternary: exactly one of the two mounts runs.
    expect(src).toMatch(/cacheLock\s*\?\s*mount\(CacheLockedScreen[\s\S]*:\s*mount\(App/);
  });
});
