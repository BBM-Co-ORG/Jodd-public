import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

const h = vi.hoisted(() => {
  // A real Map-backed Storage: Node 22's global `localStorage` is a stub
  // without --localstorage-file (see theme.test.ts). Installed before the
  // module under test loads, since it reads its preferences at import.
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, 'localStorage', {
    configurable: true,
    value: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, String(v)),
      removeItem: (k: string) => void store.delete(k),
      clear: () => store.clear(),
    },
  });
  return {
    invoke: vi.fn(),
    progress: null as null | ((e: { payload: { received: number; total: number } }) => void),
    restarting: null as null | ((e: { payload: { version: string } }) => void),
    unlisten: vi.fn(),
    openUrl: vi.fn(),
    flush: vi.fn(),
    lock: vi.fn(),
    order: [] as string[],
  };
});

vi.mock('@tauri-apps/api/core', () => ({ invoke: h.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, cb: unknown) => {
    if (name === 'app-update-progress') h.progress = cb as typeof h.progress;
    if (name === 'app-update-restarting') h.restarting = cb as typeof h.restarting;
    return Promise.resolve(h.unlisten);
  },
}));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: (...a: unknown[]) => h.openUrl(...a) }));
vi.mock('./editorFlush', () => ({
  flushEditors: () => {
    h.order.push('flush');
    return h.flush();
  },
  setEditorsLocked: (v: boolean) => {
    h.order.push(v ? 'lock' : 'unlock');
    h.lock(v);
  },
}));
vi.mock('./stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});

import {
  bannerVisible, checkForUpdate, DAY_MS, formatSize, installUpdate, isCheckDue, percent, sentence,
  scheduleAutoCheck, updateState, setAutoCheck, LAST_CHECK_KEY, RECHECK_EVERY_MS, type AvailableUpdate,
  offerText, installLabel,
} from './appUpdate';
import { isAndroid } from './stores/platform';

const UPDATE: AvailableUpdate = {
  version: '0.46.0',
  size: 30_426_016,
  releaseUrl: 'https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.46.0',
  method: 'androidInstaller',
};
const WINDOWS: AvailableUpdate = { ...UPDATE, version: '0.47.0', size: 12_000_000, method: 'desktopInstaller',
  releaseUrl: 'https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.47.0' };
const MAC: AvailableUpdate = { ...WINDOWS, size: null, method: 'releasePage' };

beforeEach(() => {
  h.invoke.mockReset();
  h.unlisten.mockReset();
  h.progress = null;
  h.restarting = null;
  h.openUrl.mockReset().mockResolvedValue(undefined);
  h.flush.mockReset().mockResolvedValue(undefined);
  h.lock.mockReset();
  h.order = [];
  updateState.set({ phase: 'idle' });
  localStorage.clear();
  setAutoCheck(true);
});

describe('isCheckDue', () => {
  const now = 1_800_000_000_000;
  it('is due when never checked or the stored value is junk', () => {
    expect(isCheckDue(now, null)).toBe(true);
    expect(isCheckDue(now, 'yesterday')).toBe(true);
  });
  it('waits a full day', () => {
    expect(isCheckDue(now, String(now - DAY_MS + 1))).toBe(false);
    expect(isCheckDue(now, String(now - DAY_MS))).toBe(true);
  });
  it('treats a last check in the future (clock moved back) as due', () => {
    expect(isCheckDue(now, String(now + 60_000))).toBe(true);
  });
});

describe('bannerVisible', () => {
  it('shows an offer until the user says Later to that version', () => {
    expect(bannerVisible({ phase: 'available', update: UPDATE }, null)).toBe(true);
    expect(bannerVisible({ phase: 'available', update: UPDATE }, '0.46.0')).toBe(false);
    // A newer release than the one dismissed shows again.
    expect(bannerVisible({ phase: 'available', update: { ...UPDATE, version: '0.47.0' } }, '0.46.0')).toBe(true);
  });
  it('shows Jodd closing to install', () => {
    expect(bannerVisible({ phase: 'restarting', update: WINDOWS }, '0.47.0')).toBe(true);
  });
  it('always shows a download in progress', () => {
    expect(bannerVisible({ phase: 'downloading', update: UPDATE, received: 0, total: 1 }, '0.46.0')).toBe(true);
  });
  it('shows an install failure, but not a failed check', () => {
    expect(bannerVisible({ phase: 'error', message: 'x', update: UPDATE }, null)).toBe(true);
    expect(bannerVisible({ phase: 'error', message: 'x', update: null }, null)).toBe(false);
  });
  it('shows nothing when idle, checking or up to date', () => {
    for (const s of [{ phase: 'idle' }, { phase: 'checking' }, { phase: 'upToDate', current: '0.46.0' }] as const) {
      expect(bannerVisible(s, null)).toBe(false);
    }
  });
});

describe('offer copy', () => {
  it('names the version, and the size when known', () => {
    expect(offerText(UPDATE)).toBe('Jodd 0.46.0 is available (29 MB).');
    expect(offerText(WINDOWS)).toBe('Jodd 0.47.0 is available (11 MB).');
    expect(offerText(MAC)).toBe('Jodd 0.47.0 is available.');
    expect(offerText({ ...WINDOWS, size: null })).toBe('Jodd 0.47.0 is available.');
  });
  it('warns that Windows asks for permission on an MSI install', () => {
    expect(offerText({ ...WINDOWS, msi: true })).toBe('Jodd 0.47.0 is available (11 MB). Windows will ask for permission.');
  });
  it('labels the button by how it installs', () => {
    expect(installLabel(UPDATE)).toBe('Install');
    expect(installLabel(WINDOWS)).toBe('Install and restart');
    expect(installLabel(MAC)).toBe('Download');
  });
});

describe('formatting', () => {
  it('rounds sizes to whole megabytes, never 0', () => {
    expect(formatSize(30_426_016)).toBe('29 MB');
    expect(formatSize(10)).toBe('1 MB');
  });
  it('says nothing about a size the release did not give', () => {
    expect(formatSize(null)).toBe('');
  });
  it('turns a backend reason into a sentence', () => {
    expect(sentence("the download doesn't match the release's SHA-256; nothing was installed"))
      .toBe("The download doesn't match the release's SHA-256; nothing was installed.");
    expect(sentence('Already a sentence!')).toBe('Already a sentence!');
    expect(sentence('  ')).toBe('');
  });
  it('clamps the percentage', () => {
    expect(percent(50, 100)).toBe(50);
    expect(percent(150, 100)).toBe(100);
    expect(percent(5, 0)).toBe(0);
  });
});

describe('checkForUpdate', () => {
  it('offers a newer release and records when it checked', async () => {
    h.invoke.mockResolvedValueOnce({ current: '0.45.0', available: UPDATE });
    await checkForUpdate();
    expect(h.invoke).toHaveBeenCalledWith('check_app_update');
    expect(get(updateState)).toEqual({ phase: 'available', update: UPDATE });
    expect(localStorage.getItem(LAST_CHECK_KEY)).not.toBeNull();
  });
  it('says up to date when there is nothing newer', async () => {
    h.invoke.mockResolvedValueOnce({ current: '0.46.0', available: null });
    await checkForUpdate();
    expect(get(updateState)).toEqual({ phase: 'upToDate', current: '0.46.0' });
  });
  it('shows a failed manual check, and hides a failed automatic one', async () => {
    h.invoke.mockRejectedValueOnce("couldn't reach GitHub");
    await checkForUpdate();
    expect(get(updateState)).toEqual({ phase: 'error', message: "Couldn't reach GitHub.", update: null });

    updateState.set({ phase: 'idle' });
    h.invoke.mockRejectedValueOnce("couldn't reach GitHub");
    await checkForUpdate(true);
    expect(get(updateState)).toEqual({ phase: 'idle' });
    // A failure is not a check: the next launch tries again.
    expect(localStorage.getItem(LAST_CHECK_KEY)).toBeNull();
  });
});

describe('installUpdate', () => {
  it('passes only the version, follows progress, and ends handed off', async () => {
    let finish!: (v: unknown) => void;
    h.invoke.mockReturnValueOnce(new Promise((r) => (finish = r)));
    const run = installUpdate(UPDATE);
    await Promise.resolve();
    await Promise.resolve();
    expect(h.invoke).toHaveBeenCalledWith('install_app_update', { version: '0.46.0' });
    h.progress!({ payload: { received: 15_000_000, total: UPDATE.size! } });
    expect(get(updateState)).toMatchObject({ phase: 'downloading', received: 15_000_000 });
    finish({ kind: 'handedOff' });
    await run;
    expect(get(updateState)).toEqual({ phase: 'handedOff', update: UPDATE });
    expect(h.unlisten).toHaveBeenCalled();
  });
  it('asks for the permission separately and carries on once it is given', async () => {
    h.invoke
      .mockResolvedValueOnce({ kind: 'needsPermission' })
      .mockResolvedValueOnce(true)
      .mockResolvedValueOnce({ kind: 'handedOff' });
    await installUpdate(UPDATE);
    expect(h.invoke.mock.calls.map((c) => c[0])).toEqual([
      'install_app_update', 'request_install_permission', 'install_app_update',
    ]);
    expect(get(updateState)).toEqual({ phase: 'handedOff', update: UPDATE });
  });
  it('stays on needsPermission when the user does not allow it', async () => {
    h.invoke.mockResolvedValueOnce({ kind: 'needsPermission' }).mockResolvedValueOnce(false);
    await installUpdate(UPDATE);
    expect(get(updateState)).toEqual({ phase: 'needsPermission', update: UPDATE });
  });
  it('leaves Install usable while the Settings call is still pending', async () => {
    // Activity recreation: request_install_permission never returns.
    h.invoke.mockResolvedValueOnce({ kind: 'needsPermission' }).mockReturnValueOnce(new Promise(() => {}));
    void installUpdate(UPDATE);
    await vi.waitFor(() => expect(h.invoke).toHaveBeenCalledTimes(2));
    expect(get(updateState)).toEqual({ phase: 'needsPermission', update: UPDATE });
    // The user allowed it and came back; tapping Install works.
    h.invoke.mockResolvedValueOnce({ kind: 'handedOff' });
    await installUpdate(UPDATE);
    expect(get(updateState)).toEqual({ phase: 'handedOff', update: UPDATE });
  });
  it('reports a failed download', async () => {
    h.invoke.mockRejectedValueOnce("the download doesn't match the release's SHA-256; nothing was installed");
    await installUpdate(UPDATE);
    expect(get(updateState)).toMatchObject({ phase: 'error', update: UPDATE });
    // Both listeners (progress, restarting) are removed, each once.
    expect(h.unlisten).toHaveBeenCalledTimes(2);
  });
  it('does not start a second download while one runs', async () => {
    updateState.set({ phase: 'downloading', update: UPDATE, received: 0, total: 1 });
    await installUpdate(UPDATE);
    expect(h.invoke).not.toHaveBeenCalled();
  });
  it('an Android install never flushes the editor (Jodd does not close)', async () => {
    h.invoke.mockResolvedValueOnce({ kind: 'handedOff' });
    await installUpdate(UPDATE);
    expect(h.flush).not.toHaveBeenCalled();
  });
});

describe('installUpdate on desktop', () => {
  it('release page updates open the browser and never invoke install', async () => {
    await installUpdate(MAC);
    expect(h.openUrl).toHaveBeenCalledWith('https://github.com/BBM-Co-ORG/Jodd-public/releases/tag/v0.47.0');
    expect(h.invoke).not.toHaveBeenCalled();
    expect(h.flush).not.toHaveBeenCalled();
  });
  it('a browser that will not open is an error the user sees', async () => {
    h.openUrl.mockRejectedValueOnce('no browser');
    await installUpdate(MAC);
    expect(get(updateState)).toMatchObject({ phase: 'error', update: MAC });
  });
  // The desktop flow is two calls: download (editors open), then install
  // (editors locked and saved first). `install` answers install_app_update.
  function desktop(download: () => Promise<unknown>, install: () => Promise<unknown>) {
    h.invoke.mockImplementation((cmd: string) => {
      h.order.push(cmd);
      return cmd === 'download_app_update' ? download() : install();
    });
  }
  const restarting = () => Promise.resolve({ kind: 'restarting' });

  it('edits typed during the download are saved before Jodd closes', async () => {
    let downloaded!: () => void;
    desktop(() => new Promise<void>((r) => (downloaded = r)), restarting);
    const run = installUpdate(WINDOWS);
    await vi.waitFor(() => expect(h.order).toContain('download_app_update'));
    // Still downloading: the editors stay open and nothing is flushed yet.
    expect(h.flush).not.toHaveBeenCalled();
    expect(h.lock).not.toHaveBeenCalled();
    downloaded();
    await run;
    expect(h.order).toEqual(['download_app_update', 'lock', 'flush', 'install_app_update']);
    expect(h.invoke).toHaveBeenCalledWith('download_app_update', { version: '0.47.0' });
    expect(h.invoke).toHaveBeenCalledWith('install_app_update', { version: '0.47.0' });
  });
  it('install waits for the editor flush', async () => {
    let flushed!: () => void;
    h.flush.mockReturnValueOnce(new Promise<void>((r) => (flushed = r)));
    desktop(() => Promise.resolve(null), restarting);
    const run = installUpdate(WINDOWS);
    await vi.waitFor(() => expect(h.flush).toHaveBeenCalled());
    expect(h.order).not.toContain('install_app_update');
    flushed();
    await run;
    expect(h.order).toContain('install_app_update');
  });
  it('a failed editor flush stops the install and unlocks the editors', async () => {
    h.flush.mockRejectedValueOnce('disk full');
    desktop(() => Promise.resolve(null), restarting);
    await installUpdate(WINDOWS);
    expect(h.order).toEqual(['download_app_update', 'lock', 'flush', 'unlock']);
    const s = get(updateState);
    expect(s).toMatchObject({ phase: 'error', update: WINDOWS });
    expect(s.phase === 'error' && s.message).toContain('disk full');
    expect(s.phase === 'error' && s.message).toContain("didn't close");
  });
  it('a refused install (still sending changes) unlocks the editors', async () => {
    desktop(() => Promise.resolve(null), () => Promise.reject('Jodd is still sending changes — try again in a minute'));
    await installUpdate(WINDOWS);
    expect(h.order.at(-1)).toBe('unlock');
    expect(get(updateState)).toMatchObject({ phase: 'error', update: WINDOWS });
  });
  it('a failed download never locks or flushes', async () => {
    desktop(() => Promise.reject('download interrupted: connection reset'), restarting);
    await installUpdate(WINDOWS);
    expect(h.flush).not.toHaveBeenCalled();
    expect(h.order).toEqual(['download_app_update', 'unlock']);
    expect(get(updateState)).toMatchObject({ phase: 'error', update: WINDOWS });
    // Try again works.
    desktop(() => Promise.resolve(null), restarting);
    await installUpdate(WINDOWS);
    expect(get(updateState)).toEqual({ phase: 'restarting', update: WINDOWS });
  });
  it('a desktop install ends in restarting', async () => {
    desktop(() => Promise.resolve(null), restarting);
    await installUpdate(WINDOWS);
    expect(get(updateState)).toEqual({ phase: 'restarting', update: WINDOWS });
  });
  it('the restarting event shows before the process exits', async () => {
    desktop(() => Promise.resolve(null), () => new Promise(() => {})); // Windows exits; the call never returns
    void installUpdate(WINDOWS);
    await vi.waitFor(() => expect(h.order).toContain('install_app_update'));
    h.restarting!({ payload: { version: '0.47.0' } });
    expect(get(updateState)).toEqual({ phase: 'restarting', update: WINDOWS });
  });
  it('a second Install while downloading does nothing', async () => {
    desktop(() => new Promise(() => {}), restarting);
    void installUpdate(WINDOWS);
    await vi.waitFor(() => expect(h.invoke).toHaveBeenCalledTimes(1));
    await installUpdate(WINDOWS);
    expect(h.invoke).toHaveBeenCalledTimes(1);
  });
});

describe('scheduleAutoCheck', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    (isAndroid as unknown as { set(v: boolean): void }).set(false);
  });

  it('the daily check runs on desktop too', () => {
    h.invoke.mockResolvedValue({ current: '0.46.0', available: null });
    const stop = scheduleAutoCheck(10);
    vi.advanceTimersByTime(10);
    expect(h.invoke).toHaveBeenCalledWith('check_app_update');
    stop();
  });
  it('a desktop left open re-checks when a day has passed', async () => {
    h.invoke.mockResolvedValue({ current: '0.46.0', available: null });
    const stop = scheduleAutoCheck(10);
    await vi.advanceTimersByTimeAsync(10);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    // Hourly re-evaluation does nothing until a day has passed…
    await vi.advanceTimersByTimeAsync(RECHECK_EVERY_MS);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    // …then checks again.
    await vi.advanceTimersByTimeAsync(DAY_MS);
    expect(h.invoke).toHaveBeenCalledTimes(2);
    stop();
  });
  it('Android checks after launch only, as before', async () => {
    h.invoke.mockResolvedValue({ current: '0.46.0', available: null });
    (isAndroid as unknown as { set(v: boolean): void }).set(true);
    const stop = scheduleAutoCheck(10);
    await vi.advanceTimersByTimeAsync(10 + 2 * DAY_MS);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    stop();
  });
  it('a failed automatic check stays quiet', async () => {
    h.invoke.mockRejectedValue('This release has no update information yet');
    const stop = scheduleAutoCheck(10);
    await vi.advanceTimersByTimeAsync(10);
    expect(get(updateState)).toEqual({ phase: 'idle' });
    stop();
  });
  it('checks once on Android when due, and not when turned off or done today', () => {
    h.invoke.mockResolvedValue({ current: '0.46.0', available: null });
    (isAndroid as unknown as { set(v: boolean): void }).set(true);
    let stop = scheduleAutoCheck(10);
    vi.advanceTimersByTime(10);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    stop();

    localStorage.setItem(LAST_CHECK_KEY, String(Date.now()));
    stop = scheduleAutoCheck(10);
    vi.advanceTimersByTime(10);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    stop();

    localStorage.removeItem(LAST_CHECK_KEY);
    setAutoCheck(false);
    stop = scheduleAutoCheck(10);
    vi.advanceTimersByTime(10);
    expect(h.invoke).toHaveBeenCalledTimes(1);
    stop();
  });
});
