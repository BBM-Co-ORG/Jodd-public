// @vitest-environment jsdom
//
// The account chip's dot, mounted: it follows the current account's real
// state (lib/accountDot.ts) rather than the hard-coded green it used to be,
// and an account switch never shows the previous account's count under the
// new account's name (gotcha #28 — only a mounted component observes that).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(false),
}));

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const B = { id: 'icloud:c@d.com', email: 'c@d.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'icloud', status: 'active' };
const zero = { notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0 };
let countsFor: Record<string, unknown> = {};
let countFails = false;

const invoke = vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
  const a = (args ?? {}) as Record<string, unknown>;
  switch (cmd) {
    case 'list_folders': return ['Notes'];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes':
      if (countFails) throw new Error('db locked');
      return countsFor[String(a.accountId)] ?? zero;
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
const startAddAccount = vi.fn();
vi.mock('../controllers/accountActions', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../controllers/accountActions')>()),
  startAddAccount: (kind: string) => startAddAccount(kind),
}));

import Sidebar from './Sidebar.svelte';
import { accounts, currentAccount } from '../stores/notes';

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

const dot = () => host.querySelector<HTMLElement>('.account-chip-dot')!;

async function mountWith(list: unknown[], current: string | null) {
  accounts.set(list as never);
  currentAccount.set(current);
  comp = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await settle();
}

beforeEach(() => {
  invoke.mockClear();
  countsFor = {};
  countFails = false;
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
  vi.restoreAllMocks();
});

describe('account chip dot', () => {
  it('says Synced, with a text equivalent, when nothing is queued', async () => {
    await mountWith([A], A.id);
    expect(dot().dataset.state).toBe('synced');
    expect(dot().getAttribute('role')).toBe('img');
    expect(dot().getAttribute('aria-label')).toBe('Synced');
    expect(dot().title).toBe('Synced');
  });

  it('shows queued work as pending', async () => {
    countsFor[A.id] = { ...zero, notes: 2 };
    await mountWith([A], A.id);
    expect(dot().dataset.state).toBe('pending');
    expect(dot().getAttribute('aria-label')).toBe('Syncing — 2 changes waiting');
  });

  it("shows a push the backend refused, and an account's own block", async () => {
    countsFor[A.id] = { ...zero, blocked: 1 };
    await mountWith([A], A.id);
    expect(dot().getAttribute('aria-label')).toBe("Sync blocked — 1 change can't be sent");
    unmount(comp!);
    host.replaceChildren();
    await mountWith([{ ...A, blocked_reason: 'Advanced Data Protection is on.' }], A.id);
    expect(dot().dataset.state).toBe('blocked');
    expect(dot().getAttribute('aria-label')).toBe('Sync blocked — Advanced Data Protection is on.');
  });

  it('is never green when the count cannot be read', async () => {
    countFails = true;
    vi.spyOn(console, 'error').mockImplementation(() => {});
    await mountWith([A], A.id);
    expect(dot().dataset.state).toBe('unknown');
    expect(dot().getAttribute('aria-label')).toBe('Sync status unavailable');
  });

  it('shows signed out with no account, and inactive for a draining one', async () => {
    await mountWith([], null);
    expect(dot().getAttribute('aria-label')).toBe('Signed out');
    expect(invoke).not.toHaveBeenCalledWith('count_pending_pushes', expect.anything());
    unmount(comp!);
    host.replaceChildren();
    await mountWith([A, { ...B, status: 'draining' }], B.id);
    expect(dot().dataset.state).toBe('inactive');
    expect(dot().getAttribute('aria-label')).toBe('Inactive — finishing sync');
  });

  it("an account switch never shows the previous account's count", async () => {
    countsFor[A.id] = { ...zero, notes: 5 };
    await mountWith([A, B], A.id);
    expect(dot().getAttribute('aria-label')).toBe('Syncing — 5 changes waiting');
    currentAccount.set(B.id);
    flushSync();
    // Same pass as the switch: A's count is not B's. Every account is read
    // now (spec 2026-11-10 §3), so B's own answer is already there.
    expect(dot().getAttribute('aria-label')).not.toContain('5 changes');
    expect(dot().getAttribute('aria-label')).toBe('Synced');
    await settle();
    expect(dot().getAttribute('aria-label')).toBe('Synced');
    expect(invoke).toHaveBeenCalledWith('count_pending_pushes', { accountId: B.id });
  });

  it('re-reads on the worker cadence, so a drained queue turns green', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      countsFor[A.id] = { ...zero, notes: 1 };
      await mountWith([A], A.id);
      expect(dot().dataset.state).toBe('pending');
      countsFor[A.id] = zero;
      await vi.advanceTimersByTimeAsync(5_000);
      await settle();
      expect(dot().dataset.state).toBe('synced');
    } finally {
      vi.useRealTimers();
    }
  });
});

// Each account's state on its own header (spec 2026-11-10 §3), not only in
// the chip that describes the current one.
describe('account header state', () => {
  const header = (id: string) => host.querySelector<HTMLElement>(`[data-account-id="${id}"] .account-header`)!;
  const mark = (id: string) => header(id).querySelector<HTMLElement>('.account-mark');

  it('shows a waiting ring on an account that is not the current one', async () => {
    countsFor[B.id] = { ...zero, notes: 3 };
    await mountWith([A, B], A.id);
    expect(mark(A.id)).toBeNull();
    expect(mark(B.id)?.dataset.mark).toBe('waiting');
    expect(mark(B.id)?.getAttribute('aria-label')).toBe('Syncing — 3 changes waiting');
  });

  it('marks a refused push with the alert dot', async () => {
    countsFor[A.id] = { ...zero, blocked: 1 };
    await mountWith([A], A.id);
    expect(mark(A.id)?.dataset.mark).toBe('alert');
  });

  it('a signed-out account says so under its header and offers the way back', async () => {
    startAddAccount.mockClear();
    const out = { ...A, blocked_reason: 'Signed out. Sign in again to sync.' };
    await mountWith([out, B], B.id);
    const notice = host.querySelector<HTMLElement>(`[data-account-id="${A.id}"] .account-notice.danger`)!;
    expect(notice.textContent).toContain('Its notes and unsent changes are kept here.');
    expect(mark(A.id)?.dataset.mark).toBe('alert');
    const again = Array.from(notice.querySelectorAll('button')).find((b) => b.textContent === 'Sign in again')!;
    again.click();
    expect(startAddAccount).toHaveBeenCalledWith('gmail');
  });

  it('iCloud\'s other blocked reason is shown whole, with no sign-in button', async () => {
    const adp = { ...B, blocked_reason: 'Advanced Data Protection is on for this Apple ID.' };
    await mountWith([A, adp], A.id);
    const notice = host.querySelector<HTMLElement>(`[data-account-id="${B.id}"] .account-notice.danger`)!;
    expect(notice.textContent).toContain('Advanced Data Protection is on');
    expect(notice.querySelector('button')).toBeNull();
  });

  it('marks the current account for a screen reader and in full ink', async () => {
    await mountWith([A, B], B.id);
    expect(host.querySelector(`[data-account-id="${B.id}"]`)!.classList.contains('current')).toBe(true);
    expect(header(B.id).textContent).toContain('current account');
    expect(header(A.id).textContent).not.toContain('current account');
  });

  it('Views collapse like Tags, closed by default, and the choice survives a remount', async () => {
    localStorage.removeItem('jodd.sidebar.viewsOpen');
    await mountWith([A], A.id);
    const toggle = () => host.querySelector<HTMLButtonElement>('.workspace-views .views-toggle')!;
    const rows = () => host.querySelectorAll('.workspace-views [data-smart-folder]').length;
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(toggle().textContent).toContain('Views');
    expect(rows()).toBe(0);

    toggle().click();
    flushSync();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(rows()).toBeGreaterThan(0);

    unmount(comp!);
    comp = undefined;
    host.innerHTML = '';
    await mountWith([A], A.id);
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(rows()).toBeGreaterThan(0);
    toggle().click();
    flushSync();
    expect(rows()).toBe(0);
    localStorage.removeItem('jodd.sidebar.viewsOpen');
  });

  it('names the account Views and Tags show, in the "For" picker, only with more than one', async () => {
    await mountWith([A, B], B.id);
    // Before any choice the picker names the current account (spec 2026-11-12 §3).
    const picker = host.querySelector('.scope-button');
    expect(picker?.getAttribute('data-scope-account')).toBe(B.id);
    expect(picker?.textContent).toContain('c');
    unmount(comp!);
    comp = undefined;
    host.innerHTML = '';
    await mountWith([A], A.id);
    expect(host.querySelector('.scope-button')).toBeNull();
  });
});
