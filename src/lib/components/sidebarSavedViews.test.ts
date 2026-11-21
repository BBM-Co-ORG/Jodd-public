// @vitest-environment jsdom
//
// Saved views in the sidebar's Views fold (spec 2026-11-19-properties-tasks-
// views §8.5, §8.7): rows under the four fixed ones with the same pick model
// and keys, New view for an account that can create folders, and the refusal
// in its place for one that cannot.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags, selectedSmartFolder, hydratedFolders,
  capabilitiesByAccount, error,
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() }));

import Sidebar from './Sidebar.svelte';
import { viewsByAccount, type ViewDef } from '../views/viewsStore';

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' };
const MS = { id: 'microsoft:c@d.com', email: 'c@d.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'microsoft', status: 'active' };
const TODAY: ViewDef = { uuid: 'v-today', name: 'Due today', label: 'Notes/__Views__', lines: ['Show: tasks', 'Due: <= today'], errors: [] };
const MS_CAPS = { has_trash: false, writes: { notes: true, relocate: false, folders: false, sidecars: false } };

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

async function settle() {
  for (let i = 0; i < 6; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}

function route(cmd: string, args?: Record<string, unknown>): unknown {
  switch (cmd) {
    case 'list_folders': return ['Notes'];
    case 'list_folder_kinds': return [['Notes', 'user']];
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'backend_capabilities': return args?.accountId === MS.id ? MS_CAPS : { has_trash: true };
    default: return [];
  }
}

async function mountFor(account: typeof GMAIL) {
  accounts.set([account]);
  currentAccount.set(account.id);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await settle();
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args?: Record<string, unknown>) => Promise.resolve(route(cmd, args)));
  localStorage.setItem('jodd.sidebar.viewsOpen', 'true');
  notes.set([]);
  selectedFolder.set('Notes');
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  capabilitiesByAccount.set({});
  viewsByAccount.set({});
  error.set(null);
});

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
  localStorage.removeItem('jodd.sidebar.viewsOpen');
});

const fold = () => host.querySelector<HTMLElement>('.workspace-views')!;
const viewRow = (uuid: string) => fold().querySelector<HTMLElement>(`[data-saved-view="${uuid}"]`);

describe('Sidebar: saved views in the Views fold', () => {
  it('lists the account\'s views after the four fixed rows', async () => {
    viewsByAccount.set({ [GMAIL.id]: [TODAY] });
    await mountFor(GMAIL);
    const rows = Array.from(fold().querySelectorAll<HTMLElement>('.folder-item'));
    const names = rows.map((r) => r.querySelector('.folder-name')?.textContent?.trim());
    expect(names.slice(0, 4)).toEqual(['Not linked', 'Untouched', 'Made with AI', 'Unreviewed']);
    expect(names.slice(4)).toEqual(['Due today', 'New view']);
  });

  it('a click opens the view and lights only its row', async () => {
    viewsByAccount.set({ [GMAIL.id]: [TODAY] });
    await mountFor(GMAIL);
    viewRow('v-today')!.click();
    await settle();
    expect(get(selectedSmartFolder)).toEqual({ account: GMAIL.id, kind: 'view', uuid: 'v-today' });
    expect(viewRow('v-today')!.classList.contains('active')).toBe(true);
    expect(fold().querySelectorAll('.folder-item.active')).toHaveLength(1);
  });

  it('Enter on the row opens it too', async () => {
    viewsByAccount.set({ [GMAIL.id]: [TODAY] });
    await mountFor(GMAIL);
    viewRow('v-today')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await settle();
    expect(get(selectedSmartFolder)).toEqual({ account: GMAIL.id, kind: 'view', uuid: 'v-today' });
  });

  it('New view calls create_view; a refusal is shown verbatim', async () => {
    await mountFor(GMAIL);
    invoke.mockImplementation((cmd: string, args?: Record<string, unknown>) =>
      cmd === 'create_view' ? Promise.reject('Views need an account that can create folders.') : Promise.resolve(route(cmd, args)));
    fold().querySelector<HTMLElement>('[data-new-view]')!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('create_view', { accountId: GMAIL.id });
    expect(get(error)).toBe('Views need an account that can create folders.');
  });

  it('an account without folder writes gets the refusal in New view\'s place, and its views still list', async () => {
    viewsByAccount.set({ [MS.id]: [TODAY] });
    await mountFor(MS as typeof GMAIL);
    expect(fold().querySelector('[data-new-view]')).toBeNull();
    expect(fold().querySelector('.view-refusal')?.textContent?.trim()).toBe('Views need an account that can create folders.');
    expect(viewRow('v-today')).not.toBeNull();
  });
});
