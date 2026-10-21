// @vitest-environment jsdom
//
// Gotcha #6 / spec §5: a `remote` event the sync worker records flips a note to
// `unreviewed` with no user command behind it. The route back to the chip is
// remote-changed -> requestRefresh -> loadNotes -> refreshUnreviewed ->
// list_unreviewed_trust -> store. This mounts App itself and fires the real
// listener, rather than asserting on the event name.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';

const ACCOUNT = { id: 'gmail:test@example.com', email: 'test@example.com', backend_kind: 'gmail' };
const handlers = new Map<string, (e: { payload: unknown }) => void>();
// Commands listed here never resolve — a remote pull that has not come back.
const hanging = new Set<string>();

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  if (hanging.has(cmd)) return new Promise(() => {});
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return [ACCOUNT];
    case 'list_unreviewed_trust': return [];
    case 'note_connections': return { outgoing: [], backlinks: [] };
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (name: string, fn: (e: { payload: unknown }) => void) => { handlers.set(name, fn); return () => handlers.delete(name); },
  emit: async () => {},
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));

import App from '../../App.svelte';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

describe('App: remote-changed refreshes the unreviewed set', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    handlers.clear();
    hanging.clear();
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  it('a remote-changed event invokes list_unreviewed_trust', async () => {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    expect(handlers.has('remote-changed'), 'App registered the remote-changed listener').toBe(true);
    await settle();
    invoke.mockClear();
    // Startup's folder hydration just refreshed; a nudge inside the 2 s throttle
    // is dropped by design (the next focus/poll carries it). Step past it.
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);

    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_unreviewed_trust'); i++) await settle(1);

    expect(invoke).toHaveBeenCalledWith('list_unreviewed_trust', { accountId: ACCOUNT.id });
  });

  // Local-first (spec §5): the chip reads local provenance, so it must not wait
  // on the network. On 2026-10-08 a 48 s Gmail `list_notes` held it back.
  it('startup invokes list_unreviewed_trust while the remote pulls never return', async () => {
    hanging.add('list_notes');
    hanging.add('index_account');
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_unreviewed_trust'); i++) await settle(1);

    expect(invoke).toHaveBeenCalledWith('list_unreviewed_trust', { accountId: ACCOUNT.id });
  });

  it('a refresh invokes list_unreviewed_trust before its list_notes returns', async () => {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    await settle();
    invoke.mockClear();
    hanging.add('list_notes');
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);

    handlers.get('remote-changed')!({ payload: ACCOUNT.id });
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_unreviewed_trust'); i++) await settle(1);

    expect(invoke).toHaveBeenCalledWith('list_notes', { accountId: ACCOUNT.id });
    expect(invoke).toHaveBeenCalledWith('list_unreviewed_trust', { accountId: ACCOUNT.id });
  });

  // A nudge that lands while a slow pull is already running must not wait for
  // it: the refresh queue parks it behind the in-flight loadNotes, and the 2 s
  // throttle drops it outright. The worker records the `remote` event before it
  // emits, so the chip can be refreshed from SQLite at once.
  it('a nudge during an in-flight pull refreshes the chip without waiting for it', async () => {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !handlers.has('remote-changed'); i++) await settle(1);
    await settle();
    hanging.add('list_notes');
    const realNow = Date.now.bind(Date);
    vi.spyOn(Date, 'now').mockImplementation(() => realNow() + 5_000);
    handlers.get('remote-changed')!({ payload: ACCOUNT.id }); // starts a pull that never returns
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_notes'); i++) await settle(1);
    await settle();

    invoke.mockClear();
    handlers.get('remote-changed')!({ payload: ACCOUNT.id }); // inside the throttle, behind the pull
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c]) => c === 'list_unreviewed_trust'); i++) await settle(1);

    expect(invoke).toHaveBeenCalledWith('list_unreviewed_trust', { accountId: ACCOUNT.id });
  });
});
