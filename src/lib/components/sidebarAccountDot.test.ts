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
    // Same pass as the switch: A's count is not B's.
    expect(dot().getAttribute('aria-label')).toBe('Checking sync…');
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
