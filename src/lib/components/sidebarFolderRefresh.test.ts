// @vitest-environment jsdom
//
// Regression test for the folder-only staleness gap.
//
// Sidebar's folder tree is re-read by refreshFolders(), which was reachable
// from exactly two triggers: the account list changing, and $noteIndex
// landing. $noteIndex only moves on cold start (indexAllAccounts, guarded by
// `lastAuthed` so it fires once per session) or when THIS app instance saves
// a note. Nothing bumps it on the 5s poll.
//
// So a change that touches only the `folders` table — a folder created by
// jodd-mcp writing straight to SQLite, or by the sync worker draining a
// dirty_new row — never reached the UI. Observed live on 2026-08-13: a folder
// created at 14:34:02 was still absent from the sidebar 90 seconds and ~20
// poll ticks later, and only appeared after an app restart.
//
// requestFolderRefresh() is the signal App.svelte's throttled refresh gate
// fires so folders re-read on the same cadence notes already do.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import {
  accounts,
  notes,
  noteIndex,
  currentAccount,
  selectedFolder,
  selectedTags,
  selectedSmartFolder,
  hydratedFolders,
  requestFolderRefresh,
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));

import Sidebar from './Sidebar.svelte';

const ACCOUNT = {
  id: 'jodd.demo@gmail.com',
  email: 'jodd.demo@gmail.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'gmail',
  status: 'active',
};

// Command router — Sidebar fires several commands on mount; only the folder
// pair matters here, the rest just need a shape that doesn't throw.
function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes', 'Notes/__Claude__', 'Notes/__Claude__/test'];
    case 'list_folder_kinds':
      return [
        ['Notes', 'user'],
        ['Notes/__Claude__', 'system_workflow'],
        ['Notes/__Claude__/test', 'user'],
      ];
    case 'count_pending_pushes':
      return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default:
      return [];
  }
}

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

/** Every command name passed to invoke since the last mockClear(). */
function invokedCommands(): string[] {
  return invoke.mock.calls.map((c) => c[0] as string);
}

/** Let the mounted component's pending promise chains settle. */
async function settle() {
  for (let i = 0; i < 5; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

beforeEach(async () => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(routeCommand(cmd)));
  notes.set([]);
  accounts.set([ACCOUNT]);
  currentAccount.set(ACCOUNT.id);
  selectedFolder.set('Notes');
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());

  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await settle();
});

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
});

describe('Sidebar folder refresh signal', () => {
  it('re-reads folders when a refresh is requested, with no note change', async () => {
    // Baseline: the mount-time read already happened. Anything after this
    // clear can only come from the signal.
    invoke.mockClear();

    requestFolderRefresh();
    await settle();

    // $noteIndex never moved — a folder-only change is exactly the case that
    // used to be invisible until restart.
    expect(invokedCommands()).toContain('list_folders');
  });

  it('re-reads folders on every request, not just the first', async () => {
    invoke.mockClear();

    requestFolderRefresh();
    await settle();
    const afterFirst = invokedCommands().filter((c) => c === 'list_folders').length;

    requestFolderRefresh();
    await settle();
    const afterSecond = invokedCommands().filter((c) => c === 'list_folders').length;

    // A boolean flag store would notify once and then go quiet, because
    // Svelte's safe_not_equal suppresses set(true) over an existing `true`.
    // The signal has to be a counter for the second poll tick to land.
    expect(afterSecond).toBeGreaterThan(afterFirst);
  });
});
