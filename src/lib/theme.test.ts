// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest';
import { getThemePref, setThemePref, applyTheme, THEME_KEY } from './theme';

// vitest 2.1.9's jsdom environment predates Node's own experimental global
// `localStorage` (Node 22+): its window-key proxy list doesn't include
// "localStorage", and since the key now already exists on Node's global
// object, the proxy setup skips it entirely -- `localStorage` then resolves
// to Node's non-functional stub (throws/undefined without
// --localstorage-file) instead of jsdom's real, working implementation.
// Same root cause whatsNew.test.ts sidesteps by avoiding jsdom altogether;
// this file needs jsdom for document.documentElement too, so it patches the
// global directly with a real Map-backed Storage instead.
const store = new Map<string, string>();
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
  },
});

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
});

describe('theme preference', () => {
  it('defaults to system', () => {
    expect(getThemePref()).toBe('system');
  });

  it('round-trips a stored preference', () => {
    setThemePref('dark');
    expect(localStorage.getItem(THEME_KEY)).toBe('dark');
    expect(getThemePref()).toBe('dark');
  });

  it('falls back to system when the stored value is junk', () => {
    localStorage.setItem(THEME_KEY, 'chartreuse');
    expect(getThemePref()).toBe('system');
  });

  it('uses the same jodd: localStorage namespace as whatsNew', () => {
    expect(THEME_KEY.startsWith('jodd:')).toBe(true);
  });
});

describe('localStorage failures (locked-down webview, private mode, etc.)', () => {
  it('getThemePref falls back to system when localStorage.getItem throws', () => {
    const original = globalThis.localStorage.getItem;
    globalThis.localStorage.getItem = () => {
      throw new Error('SecurityError: storage disabled by policy');
    };
    try {
      expect(getThemePref()).toBe('system');
    } finally {
      globalThis.localStorage.getItem = original;
    }
  });

  it('setThemePref still applies the theme when localStorage.setItem throws', () => {
    const original = globalThis.localStorage.setItem;
    globalThis.localStorage.setItem = () => {
      throw new Error('SecurityError: storage disabled by policy');
    };
    try {
      setThemePref('dark');
      expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    } finally {
      globalThis.localStorage.setItem = original;
    }
  });
});

describe('applyTheme', () => {
  it('stamps data-theme for an explicit choice', () => {
    applyTheme('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    applyTheme('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('removes data-theme for system, so the media query takes over', () => {
    applyTheme('dark');
    applyTheme('system');
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});
