// In-app updates, frontend side, for every platform (spec
// docs/superpowers/specs/2026-11-11-app-updater-design.md). The Rust commands
// (src-tauri/src/app_update/) find the update and say how it installs —
// `method` — so this module never guesses the platform for that:
//
//   androidInstaller  verified APK → Android's Package installer
//   desktopInstaller  Windows: signed installer; Jodd closes and reopens
//   releasePage       macOS (and an install Jodd can't match): open the release
//
// It holds the state the Settings section and the banner both show, and
// decides when the once-a-day check runs.
//
// Preferences live in localStorage beside jodd:theme: they are per-device
// conveniences, and losing one only means one extra check or one banner.

import { writable, get } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { openUrl } from '@tauri-apps/plugin-opener';
import { isAndroid } from './stores/platform';
import { flushEditors, setEditorsLocked } from './editorFlush';

export type InstallMethod = 'androidInstaller' | 'desktopInstaller' | 'releasePage';

export interface AvailableUpdate {
  version: string;
  /** Bytes, when the release says. */
  size: number | null;
  /** The GitHub release page. */
  releaseUrl: string;
  method: InstallMethod;
  /** Windows: this copy was installed from the MSI, so Windows asks for permission (UAC). */
  msi?: boolean;
}

export interface UpdateCheck {
  current: string;
  available: AvailableUpdate | null;
}

export type InstallOutcome = { kind: 'handedOff' } | { kind: 'needsPermission' } | { kind: 'restarting' };

export type UpdateState =
  | { phase: 'idle' }
  | { phase: 'checking' }
  | { phase: 'upToDate'; current: string }
  | { phase: 'available'; update: AvailableUpdate }
  | { phase: 'downloading'; update: AvailableUpdate; received: number; total: number }
  | { phase: 'handedOff'; update: AvailableUpdate }
  | { phase: 'needsPermission'; update: AvailableUpdate }
  /** Windows: the installer is running and Jodd is closing; it reopens on the new version. */
  | { phase: 'restarting'; update: AvailableUpdate }
  | { phase: 'error'; message: string; update: AvailableUpdate | null };

export const AUTO_CHECK_KEY = 'jodd:updateAutoCheck';
export const LAST_CHECK_KEY = 'jodd:updateLastCheck';
export const DISMISSED_KEY = 'jodd:updateDismissed';
export const DAY_MS = 24 * 60 * 60 * 1000;
/** A desktop Jodd stays open for days, so whether a check is due is asked again this often. */
export const RECHECK_EVERY_MS = 60 * 60 * 1000;

function read(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}
function write(key: string, value: string): void {
  try { localStorage.setItem(key, value); } catch { /* best effort */ }
}

/** Off only when the user turned it off. */
export function getAutoCheck(): boolean {
  return read(AUTO_CHECK_KEY) !== 'false';
}

/** A day since the last check, or never checked. A clock set backwards counts as due. */
export function isCheckDue(now: number, last: string | null): boolean {
  const t = last === null ? NaN : Number(last);
  if (!Number.isFinite(t)) return true;
  return now - t >= DAY_MS || now < t;
}

/**
 * Whether the banner shows. Not for a version the user said "Later" to —
 * a newer one shows again — and not while nothing is on offer. A failed
 * install keeps its banner, so the failure is not only in Settings.
 */
export function bannerVisible(state: UpdateState, dismissed: string | null): boolean {
  switch (state.phase) {
    case 'available':
    case 'needsPermission':
    case 'handedOff':
      return state.update.version !== dismissed;
    case 'downloading':
    case 'restarting':
      return true;
    case 'error':
      return state.update !== null && state.update.version !== dismissed;
    default:
      return false;
  }
}

/** "29 MB" — whole megabytes are precise enough for a download size. '' when unknown. */
export function formatSize(bytes: number | null): string {
  if (bytes === null) return '';
  return `${Math.max(1, Math.round(bytes / (1024 * 1024)))} MB`;
}

/** The backend's reasons are lowercase clauses; shown alone they need to read as a sentence. */
export function sentence(text: string): string {
  const t = text.trim();
  if (t === '') return t;
  const s = t[0].toUpperCase() + t.slice(1);
  return /[.!?]$/.test(s) ? s : `${s}.`;
}

/** The banner's and Settings' one line about an update on offer. */
export function offerText(u: AvailableUpdate): string {
  const size = u.method === 'releasePage' ? '' : formatSize(u.size);
  const base = `Jodd ${u.version} is available${size ? ` (${size})` : ''}.`;
  return u.msi && u.method === 'desktopInstaller' ? `${base} Windows will ask for permission.` : base;
}

/** What the install button says, by how the update installs. */
export function installLabel(u: AvailableUpdate): string {
  switch (u.method) {
    case 'desktopInstaller': return 'Install and restart';
    case 'releasePage': return 'Download';
    default: return 'Install';
  }
}

export function percent(received: number, total: number): number {
  return total > 0 ? Math.min(100, Math.floor((received * 100) / total)) : 0;
}

export const updateState = writable<UpdateState>({ phase: 'idle' });
export const autoCheck = writable<boolean>(getAutoCheck());
export const dismissedVersion = writable<string | null>(read(DISMISSED_KEY));

export function setAutoCheck(on: boolean): void {
  autoCheck.set(on);
  write(AUTO_CHECK_KEY, on ? 'true' : 'false');
}

export function dismissUpdate(version: string): void {
  dismissedVersion.set(version);
  write(DISMISSED_KEY, version);
}

function busy(): boolean {
  const p = get(updateState).phase;
  return p === 'checking' || p === 'downloading' || p === 'restarting';
}

/** Ask GitHub for the latest release. `quiet` keeps a failed automatic check out of sight. */
export async function checkForUpdate(quiet = false): Promise<void> {
  if (busy()) return;
  const before = get(updateState);
  updateState.set({ phase: 'checking' });
  try {
    const r = await invoke<UpdateCheck>('check_app_update');
    write(LAST_CHECK_KEY, String(Date.now()));
    updateState.set(r.available ? { phase: 'available', update: r.available } : { phase: 'upToDate', current: r.current });
  } catch (e) {
    // A daily check that fails offline is not news; put back what was shown.
    updateState.set(quiet ? before : { phase: 'error', message: sentence(String(e)), update: null });
  }
}

/** Open the update's release page in the browser (macOS, or an install Jodd can't match). */
export async function openReleasePage(update: AvailableUpdate): Promise<void> {
  try {
    await openUrl(update.releaseUrl);
  } catch (e) {
    updateState.set({ phase: 'error', message: sentence(`couldn't open the release page: ${String(e)}`), update });
  }
}

/**
 * Install `update` the way its `method` says.
 *
 * - releasePage: only opens the release page.
 * - desktopInstaller (Windows): downloads and verifies first, with the
 *   editors still open. Only then locks every editor and saves it — Jodd is
 *   about to close, and anything typed during the download must reach SQLite
 *   — and asks Rust to install. Rust emits `app-update-restarting` just before
 *   the process exits, since that call never returns there.
 * - androidInstaller: downloads, verifies and opens Android's installer.
 *   Without "Install unknown apps" it sends the user to Settings and, if they
 *   allow it there, carries on by itself.
 */
export async function installUpdate(update: AvailableUpdate): Promise<void> {
  if (update.method === 'releasePage') {
    await openReleasePage(update);
    return;
  }
  if (busy()) return;
  updateState.set({ phase: 'downloading', update, received: 0, total: update.size ?? 0 });
  const unlisten = await listen<{ received: number; total: number }>('app-update-progress', (e) => {
    const s = get(updateState);
    if (s.phase === 'downloading') updateState.set({ ...s, received: e.payload.received, total: e.payload.total });
  });
  const unlistenRestart = await listen<{ version: string }>('app-update-restarting', (e) => {
    if (e.payload.version === update.version) updateState.set({ phase: 'restarting', update });
  });
  let out: InstallOutcome;
  try {
    if (update.method === 'desktopInstaller') {
      // Download while the user can still type; Rust keeps the verified file.
      await invoke('download_app_update', { version: update.version });
      setEditorsLocked(true);
      try {
        await flushEditors();
      } catch (e) {
        setEditorsLocked(false);
        updateState.set({
          phase: 'error',
          message: sentence(`couldn't save the open note (${e instanceof Error ? e.message : String(e)}), so Jodd didn't close to update`),
          update,
        });
        return;
      }
    }
    out = await invoke<InstallOutcome>('install_app_update', { version: update.version });
  } catch (e) {
    setEditorsLocked(false);
    updateState.set({ phase: 'error', message: sentence(String(e)), update });
    return;
  } finally {
    unlisten();
    unlistenRestart();
  }
  if (out.kind === 'restarting') {
    updateState.set({ phase: 'restarting', update });
    return;
  }
  if (out.kind === 'handedOff') {
    updateState.set({ phase: 'handedOff', update });
    return;
  }
  // Settings is asked AFTER the download state is cleared: if Android
  // recreates the activity while Settings is open, this call never returns,
  // and the banner must still offer Install.
  updateState.set({ phase: 'needsPermission', update });
  let granted = false;
  try {
    granted = await invoke<boolean>('request_install_permission');
  } catch (e) {
    updateState.set({ phase: 'error', message: sentence(String(e)), update });
    return;
  }
  const now = get(updateState);
  if (granted && now.phase === 'needsPermission' && now.update.version === update.version) {
    await installUpdate(update);
  }
}

/**
 * The once-a-day check, a little after launch so it does not compete with the
 * first sync. On desktop, where Jodd stays open for days, whether a check is
 * due is asked again every `recheckMs`; Android keeps the after-launch check
 * only, as before. Returns a cleanup.
 */
export function scheduleAutoCheck(delayMs = 10_000, recheckMs = RECHECK_EVERY_MS): () => void {
  const due = () => get(autoCheck) && isCheckDue(Date.now(), read(LAST_CHECK_KEY));
  const first = setTimeout(() => {
    if (due()) void checkForUpdate(true);
  }, delayMs);
  const every = setInterval(() => {
    if (!get(isAndroid) && due()) void checkForUpdate(true);
  }, recheckMs);
  return () => {
    clearTimeout(first);
    clearInterval(every);
  };
}
