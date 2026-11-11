// @vitest-environment jsdom
//
// Opening a folder paints it from the local cache first
// (`paintFolderFromCache`), replacing the list's notes in that folder with
// what SQLite holds. It matched on the folder NAME alone, so opening
// "Notes/Reading" in one account dropped every other account's notes in a
// folder of the same name from the list until the next full poll. Found next
// to the same defect in `loadFolderNotes`, fixed in #273.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get } from 'svelte/store';

const ACCOUNTS = [
  { id: 'gmail:a@x', email: 'a@x', backend_kind: 'gmail' },
  { id: 'gmail:b@x', email: 'b@x', backend_kind: 'gmail' },
];
const FOLDER = 'Notes/Reading';
const MINE = { uuid: 'A1', id: 'm1', title: 'In A', label: FOLDER, account_id: 'gmail:a@x', body_html: '' };
const THEIRS = { uuid: 'B1', id: 'm2', title: 'In B', label: FOLDER, account_id: 'gmail:b@x', body_html: '' };

const invoke = vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'list_accounts': return ACCOUNTS;
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_cached_notes_in_folder': {
      const a = args as { accountId: string; path: string };
      return a.accountId === 'gmail:a@x' && a.path === FOLDER ? [MINE] : [];
    }
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

describe("App: the cache paint of a folder covers one account's notes in it", () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;

  afterEach(() => {
    if (app) unmount(app);
    app = null;
    host?.remove();
    selectedFolder.set('Notes');
    notes.set([]);
    vi.clearAllMocks();
  });

  it("opening a folder in one account keeps another account's notes in a folder of the same name", async () => {
    selectedFolder.set('Notes');
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await settle(60);

    currentAccount.set('gmail:a@x');
    notes.set([THEIRS as never]);
    selectedFolder.set(FOLDER);
    await settle();

    expect(invoke).toHaveBeenCalledWith('list_cached_notes_in_folder', { accountId: 'gmail:a@x', path: FOLDER });
    const uuids = get(notes).map((n) => n.uuid).sort();
    expect(uuids).toEqual(['A1', 'B1']);
  });
});
