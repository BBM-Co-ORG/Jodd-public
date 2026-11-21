// @vitest-environment jsdom
//
// 2026-11-05, note 80D6C8E1: Share to Jodd on a Tab S7 that had sat a day in
// the background appended to the note as that device last saw it. Android
// freezes a backgrounded Jodd, so nothing pulled while it slept; on return the
// focus settle waited 10 s and then re-read only the SELECTED folder — never
// the share target's. Two fixes, both background reads that never hold a
// write (local-first doctrine):
//  - the capture sheet asks for the target note's folder (`jodd:refresh-folder`),
//    in whichever account it lives;
//  - becoming visible again re-reads the selected folder within a second.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';

const ACCOUNTS = [
  { id: 'gmail:a@x', email: 'a@x', backend_kind: 'gmail' },
  { id: 'gmail:b@x', email: 'b@x', backend_kind: 'gmail' },
];

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return ACCOUNTS;
    case 'note_connections': return { outgoing: [], backlinks: [] };
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

import App from '../../App.svelte';
import { selectedFolder, currentAccount, notes } from '../stores/notes';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

const folderReads = () =>
  invoke.mock.calls
    .filter(([c]) => c === 'list_notes_in_folder')
    .map(([, a]) => a as unknown as { accountId: string; path: string });

function setVisibility(state: 'visible' | 'hidden') {
  Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => state });
  document.dispatchEvent(new Event('visibilitychange'));
}

describe('App: the share target and a return to the app are re-read promptly', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;

  async function start() {
    selectedFolder.set('Notes');
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await settle(60);
    invoke.mockClear();
  }

  afterEach(() => {
    vi.useRealTimers();
    if (app) unmount(app);
    app = null;
    host?.remove();
    selectedFolder.set('Notes');
    notes.set([]);
    vi.clearAllMocks();
  });

  it("a capture's target folder is pulled, in the account the note lives in", async () => {
    await start();
    currentAccount.set('gmail:a@x');
    window.dispatchEvent(new CustomEvent('jodd:refresh-folder', { detail: { accountId: 'gmail:b@x', folder: 'Notes/Reading' } }));
    await settle();
    expect(folderReads()).toContainEqual(expect.objectContaining({ accountId: 'gmail:b@x', path: 'Notes/Reading' }));
  });

  it("pulling another account's folder leaves this account's notes in a folder of the same name", async () => {
    await start();
    const mine = { uuid: 'A1', id: 'm1', title: 'Mine', label: 'Notes/Reading', account_id: 'gmail:a@x', body_html: '' };
    notes.set([mine as never]);
    window.dispatchEvent(new CustomEvent('jodd:refresh-folder', { detail: { accountId: 'gmail:b@x', folder: 'Notes/Reading' } }));
    await settle();
    let found = false;
    notes.subscribe((ns) => (found = ns.some((n) => n.uuid === 'A1')))();
    expect(found).toBe(true);
  });

  it('becoming visible again re-reads the selected folder within a second, not ten', async () => {
    await start();
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    setVisibility('hidden');
    setVisibility('visible');
    await vi.advanceTimersByTimeAsync(1_500);
    vi.useRealTimers();
    await settle();
    expect(folderReads()).toContainEqual(expect.objectContaining({ path: 'Notes' }));
  });
});
