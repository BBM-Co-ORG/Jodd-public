// @vitest-environment jsdom
//
// A pin or folder push the backend refused permanently (migration 23,
// gotcha #14) stops retrying — so unless the UI says so, a pin that will
// never reach other devices and a folder that will never reach Gmail look
// exactly like synced ones. The reason must be visible TEXT somewhere, not
// only a tooltip: Android has no hover. Asserted on the mounted components
// (gotcha #28).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import {
  accounts,
  notes,
  noteIndex,
  currentAccount,
  selectedFolder,
  selectedNote,
  selectedTags,
  selectedSmartFolder,
  selectedUuids,
  searchQuery,
  hydratedFolders,
  capabilitiesByAccount,
} from '../stores/notes';
import { syncBlocks } from '../syncBlocks';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));

import Sidebar from './Sidebar.svelte';
import NoteList from './NoteList.svelte';
import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = {
  id: 'gmail:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'gmail',
  status: 'active',
};
const PIN_REASON = "Gmail refused to create label 'Notes-Meta' (HTTP 409)";
const FOLDER_REASON = "Gmail refused to create label 'Notes/Work' (HTTP 409)";

const NOTE = {
  uuid: 'u1', id: 'm1', account_id: ACCOUNT.id, title: 'Pinned one',
  body_html: '<div>Pinned one</div><div>x</div>', date: '2026-10-08T00:00:00Z',
  label: 'Notes/Work', pinned: true, local_version: 1,
} as Note;

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes', 'Notes/Work', 'Notes/Play'];
    case 'list_folder_kinds':
      return [];
    case 'list_sync_blocks':
      return { pins: [{ uuid: 'u1', reason: PIN_REASON }], folders: [{ path: 'Notes/Work', reason: FOLDER_REASON }] };
    case 'count_pending_pushes':
      return { notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0 };
    case 'note_connections':
      return { outgoing: [], backlinks: [] };
    case 'note_persistence':
      return null;
    case 'retry_blocked_pin':
    case 'retry_blocked_folder':
      return true;
    default:
      return [];
  }
}

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(routeCommand(cmd)));
  syncBlocks.set({});
  accounts.set([ACCOUNT]);
  currentAccount.set(ACCOUNT.id);
  notes.set([NOTE]);
  selectedFolder.set('Notes/Work');
  selectedNote.set(null);
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  selectedUuids.set(new Set());
  searchQuery.set('');
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  capabilitiesByAccount.set({});
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  if (component) unmount(component);
  component = null;
  host?.remove();
});

describe('Sidebar marks a folder whose push was refused', () => {
  it('reads the blocks with the folders and marks only the blocked row', async () => {
    component = mount(Sidebar, { target: host, props: { width: 200 } });
    flushSync();
    await settle();

    expect(invoke.mock.calls.map((c) => c[0])).toContain('list_sync_blocks');
    const rows = [...host.querySelectorAll('.folder-name')].map((el) => el.closest('[class*="folder"]')!.parentElement!);
    const work = rows.find((r) => r.textContent?.includes('Work'))!;
    const play = rows.find((r) => r.textContent?.includes('Play'))!;
    const marker = work.querySelector('.folder-blocked-indicator') as HTMLElement | null;
    expect(marker?.getAttribute('title')).toContain(FOLDER_REASON);
    expect(play.querySelector('.folder-blocked-indicator')).toBeNull();
  });
});

describe('NoteList marks a blocked pin and says why a folder did not sync', () => {
  it('marks the pin and shows the folder reason as text, with Try again', async () => {
    syncBlocks.set({ [ACCOUNT.id]: { pins: { u1: PIN_REASON }, folders: { 'Notes/Work': FOLDER_REASON } } });
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    await settle();

    const pin = host.querySelector('.pin-indicator') as HTMLElement;
    expect(pin.classList.contains('pin-blocked')).toBe(true);
    expect(pin.getAttribute('title')).toContain(PIN_REASON);

    const banner = host.querySelector('.folder-blocked-banner') as HTMLElement;
    expect(banner.textContent).toContain(FOLDER_REASON);
    (banner.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    expect(invoke).toHaveBeenCalledWith('retry_blocked_folder', { accountId: ACCOUNT.id, path: 'Notes/Work' });
    expect(host.querySelector('.folder-blocked-banner')).toBeNull();
  });

  it('shows neither when nothing is blocked', async () => {
    component = mount(NoteList, { target: host, props: { width: 360 } });
    flushSync();
    await settle();

    expect(host.querySelector('.pin-indicator')?.classList.contains('pin-blocked')).toBe(false);
    expect(host.querySelector('.folder-blocked-banner')).toBeNull();
  });
});

describe('NoteEditor says a pin did not sync', () => {
  it('shows the reason as text, and Try again re-arms it', async () => {
    syncBlocks.set({ [ACCOUNT.id]: { pins: { u1: PIN_REASON }, folders: {} } });
    selectedNote.set(NOTE);
    component = mount(NoteEditor, { target: host });
    flushSync();
    await settle();

    const banner = host.querySelector('.pin-blocked-banner') as HTMLElement;
    expect(banner.textContent).toContain(PIN_REASON);
    (banner.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    expect(invoke).toHaveBeenCalledWith('retry_blocked_pin', { accountId: ACCOUNT.id, uuid: 'u1' });
    expect(host.querySelector('.pin-blocked-banner')).toBeNull();
  });
});
