// @vitest-environment jsdom
//
// Measured live 2026-10-08: `Notes/__Claude__` was selected when its Gmail
// label went away and the folder prune dropped its row. Every later refresh
// of the selection asked the backend for it, the backend answered
// `Folder not found: Notes/__Claude__` (list_notes_in_folder, lib.rs), the
// ErrorBar showed it, and the header kept naming a folder that no longer
// existed. That answer is authoritative — the backend checked the label map
// AND the local folders table — so the selection goes back to the root.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get } from 'svelte/store';

const ACCOUNT = { id: 'gmail:test@example.com', email: 'test@example.com', backend_kind: 'gmail' };
const GONE = 'Notes/__Claude__';

const invoke = vi.fn(async (cmd: string, args?: { path?: string }): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return [ACCOUNT];
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_notes_in_folder':
      if (args?.path === GONE) throw `Folder not found: ${GONE}`;
      return [];
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: { path?: string }) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async () => () => {},
  emit: async () => {},
}));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));

import App from '../../App.svelte';
import { selectedFolder, error } from '../stores/notes';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

describe('App: a selected folder the backend says is gone', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    selectedFolder.set('Notes');
    error.set(null);
    vi.clearAllMocks();
  });

  it('moves the selection to the root instead of raising "Folder not found"', async () => {
    selectedFolder.set(GONE);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    for (let i = 0; i < 50 && !invoke.mock.calls.some(([c, a]) => c === 'list_notes_in_folder' && a?.path === GONE); i++) {
      await settle(1);
    }
    expect(invoke, 'the startup load asked for the gone folder').toHaveBeenCalledWith('list_notes_in_folder', expect.objectContaining({ path: GONE }));
    await settle();

    expect(get(selectedFolder)).toBe('Notes');
    expect(get(error) ?? '').not.toContain('Folder not found');
  });
});
