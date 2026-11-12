// @vitest-environment jsdom
//
// The arrival line follows a sign-in through the door, and only that: a
// launch that was already signed in has nothing to announce.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get, writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(false),
}));

const ICLOUD = { id: 'icloud:a@example.com', email: 'a@example.com', backend_kind: 'icloud', status: 'active', added_at: '' };
let signedIn = false;
const invoke = vi.fn(async (cmd: string): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return signedIn;
    case 'get_oauth_config': return { credentials_available: true };
    case 'list_accounts': return signedIn ? [ICLOUD] : [];
    case 'list_folders': return ['Notes'];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: false } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'index_account':
      return [{ id: 'm1', label: 'Notes' }, { id: 'm2', label: 'Notes' }, { id: 'm3', label: 'Notes/Ideas' }];
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string) => invoke(cmd),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import App from '../../App.svelte';
import { isAuthenticated, accounts, currentAccount, notes, noteIndex } from '../stores/notes';
import { arrival, dismissArrival } from '../stores/arrival';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

describe('App: the arrival after a sign-in', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    app = null;
    host?.remove();
    dismissArrival();
    isAuthenticated.set(false);
    accounts.set([]);
    currentAccount.set(null);
    notes.set([]);
    noteIndex.set(new Map());
  });

  async function launch() {
    history.replaceState(null, '', location.href);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await settle();
    return host;
  }

  it('signing in at the door ends in the count, and that the notes stayed put', async () => {
    signedIn = false;
    const el = await launch();
    expect(el.querySelector('.auth-screen')).not.toBeNull();
    expect(get(arrival)).toBeNull();

    signedIn = true;
    isAuthenticated.set(true); // what AuthScreen does when the sign-in lands
    await vi.waitFor(() => expect(get(arrival)).not.toBeNull());
    expect(get(arrival)!.lines).toEqual([
      '3 notes found in iCloud.',
      'They stay where they are. Jodd keeps a copy on this device.',
    ]);
    await settle();
    expect(el.querySelector('.arrival [role="status"]')!.textContent).toMatch(/3 notes found in iCloud\./);
  });

  it('a launch that was already signed in says nothing', async () => {
    signedIn = true;
    await launch();
    await vi.waitFor(() => expect(get(currentAccount)).toBe(ICLOUD.id));
    await vi.waitFor(() => expect(invoke).toHaveBeenCalledWith('index_account'));
    await settle();
    expect(get(arrival)).toBeNull();
  });
});
