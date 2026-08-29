import { describe, it, expect, vi, beforeEach } from 'vitest';

// getVersion() is swapped per-test via mockResolvedValueOnce below.
const getVersion = vi.fn();
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => getVersion() }));

vi.mock('./generated/changelog.json', () => ({
  default: [
    { version: 'Unreleased', date: null, sections: {} },
    { version: '0.17.6', date: '2026-07-03', sections: { Fixed: ['dedup perf'] } },
    { version: '0.17.5', date: '2026-07-03', sections: { Added: ['trash preview'] } },
    { version: '0.17.4', date: '2026-07-02', sections: { Fixed: ['editor race'] } },
    { version: '0.16.6', date: '2026-06-15', sections: { Changed: ['stability'] } },
  ],
}));

// Minimal in-memory localStorage stub — this module doesn't otherwise
// touch the DOM, so no need to pull in a jsdom environment for it.
function stubLocalStorage() {
  const store = new Map<string, string>();
  (globalThis as any).localStorage = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  };
  return store;
}

describe('whatsNew', () => {
  let store: Map<string, string>;

  beforeEach(() => {
    vi.resetModules();
    getVersion.mockReset();
    store = stubLocalStorage();
  });

  it('whatsNewForLaunch: fresh install shows only the current version, and records it as seen', async () => {
    getVersion.mockResolvedValue('0.17.6');
    const { whatsNewForLaunch } = await import('./whatsNew');
    const versions = await whatsNewForLaunch();
    expect(versions.map((v) => v.version)).toEqual(['0.17.6']);
    expect(store.get('jodd:lastSeenVersion')).toBe('0.17.6');
  });

  it('whatsNewForLaunch: upgrading from an older seen version shows every entry newer than it', async () => {
    getVersion.mockResolvedValue('0.17.6');
    store.set('jodd:lastSeenVersion', '0.17.4');
    const { whatsNewForLaunch } = await import('./whatsNew');
    const versions = await whatsNewForLaunch();
    expect(versions.map((v) => v.version)).toEqual(['0.17.6', '0.17.5']);
  });

  it('whatsNewForLaunch: already-seen current version returns nothing (this is what starves the manual trigger if it shares state)', async () => {
    getVersion.mockResolvedValue('0.17.6');
    store.set('jodd:lastSeenVersion', '0.17.6');
    const { whatsNewForLaunch } = await import('./whatsNew');
    expect(await whatsNewForLaunch()).toEqual([]);
  });

  it('whatsNewForCurrentVersion: returns the running version notes even when the launch check already marked it seen', async () => {
    getVersion.mockResolvedValue('0.17.6');
    store.set('jodd:lastSeenVersion', '0.17.6'); // simulates About being opened after the auto-popup already fired
    const { whatsNewForCurrentVersion } = await import('./whatsNew');
    const versions = await whatsNewForCurrentVersion();
    expect(versions.map((v) => v.version)).toEqual(['0.17.6']);
  });

  it('whatsNewForCurrentVersion: does not mutate the "seen" bookkeeping', async () => {
    getVersion.mockResolvedValue('0.17.6');
    const { whatsNewForCurrentVersion } = await import('./whatsNew');
    await whatsNewForCurrentVersion();
    expect(store.has('jodd:lastSeenVersion')).toBe(false);
  });
});
