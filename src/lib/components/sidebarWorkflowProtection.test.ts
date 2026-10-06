// @vitest-environment jsdom
//
// Extract filing (2026-09-15) retired `__Extracts__` as a place, so the user
// must be able to rename and delete it. `__Claude__` is different: jodd-mcp
// uses it as its write allowlist (mcp_write_scope.json), and renaming it
// would silently break MCP writes — it stays protected.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder,
  selectedTags, selectedSmartFolder, hydratedFolders,
} from '../stores/notes';
import { smartFolderCommand } from '../smartFolders';

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
  id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z',
  backend_kind: 'gmail', status: 'active',
};

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes', 'Notes/__Claude__', 'Notes/__Extracts__'];
    case 'list_folder_kinds':
      return [['Notes/__Claude__', 'system_workflow'], ['Notes/__Extracts__', 'system_workflow']];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes':
      return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default:
      return [];
  }
}

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

async function settle() {
  for (let i = 0; i < 5; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

/** Workflow rows sit under the Notes root; expand every collapsed toggle until none is left. */
async function expandAll() {
  for (let pass = 0; pass < 5; pass++) {
    const toggles = host.querySelectorAll<HTMLElement>('[aria-label="Expand"]');
    if (toggles.length === 0) return;
    toggles.forEach((t) => t.click());
    await settle();
  }
}

async function openMenuFor(displayName: string): Promise<HTMLElement> {
  await expandAll();
  const name = host.querySelector<HTMLElement>(`.folder-item .folder-name[title="${displayName}"]`);
  if (!name) throw new Error(`no folder row "${displayName}"; sidebar text: ${host.textContent}`);
  name.closest('.folder-item')!.dispatchEvent(
    new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }),
  );
  await settle();
  const menu = host.querySelector<HTMLElement>('.folder-menu');
  if (!menu) throw new Error('folder menu did not open');
  return menu;
}

function hasItem(menu: HTMLElement, label: string): boolean {
  return Array.from(menu.querySelectorAll('button')).some((b) => (b.textContent ?? '').includes(label));
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

describe('workflow folder protection after extract filing', () => {
  it('offers Rename and Delete on the retired __Extracts__ folder', async () => {
    const menu = await openMenuFor('Extracts');
    expect(hasItem(menu, 'Rename')).toBe(true);
    expect(hasItem(menu, 'Delete')).toBe(true);
  });

  it('keeps __Claude__ protected — jodd-mcp writes there', async () => {
    const menu = await openMenuFor('Claude');
    expect(hasItem(menu, 'Rename')).toBe(false);
    expect(hasItem(menu, 'Delete')).toBe(false);
    expect(menu.textContent).toContain('Workflow folder — managed by Jodd');
  });

  it('opens the Extracts view, which reads list_extract_notes', async () => {
    const row = host.querySelector<HTMLElement>('[data-smart-folder="extracts"]');
    expect(row, 'no Extracts row under Views').toBeTruthy();
    row!.click();
    await settle();
    expect(get(selectedSmartFolder)).toEqual({ account: ACCOUNT.id, kind: 'extracts' });
    expect(smartFolderCommand('extracts')).toBe('list_extract_notes');
    expect(smartFolderCommand('orphaned')).toBe('list_orphaned_notes');
    expect(smartFolderCommand('stale')).toBe('list_stale_notes');
  });
});
