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
      return [
        'Notes', 'Notes/Work', 'Notes/Lessons', 'Notes/__Claude__', 'Notes/__Extracts__',
        'Notes/__Agent__', 'Notes/__Agent__/Projects',
      ];
    case 'list_folder_kinds':
      // `Notes/Lessons`: a legacy plain-named row — the kind is never demoted.
      // `__Agent__/Projects` is kind user: only the leaf decides (gotcha #4).
      return [
        ['Notes/__Claude__', 'system_workflow'], ['Notes/__Extracts__', 'system_workflow'],
        ['Notes/Lessons', 'system_workflow'], ['Notes/__Agent__', 'system_workflow'],
      ];
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
    expect(menu.querySelector('.menu-reason')?.textContent).toBe("Agent folder: agents write here, so it can't be renamed or moved.");
  });

  it('protects an agent folder itself, not the folders inside it', async () => {
    expect(hasItem(await openMenuFor('Agent'), 'Rename')).toBe(false);
    document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    await settle();
    expect(hasItem(await openMenuFor('Projects'), 'Rename')).toBe(true);
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

  it('opens the Unreviewed view, which reads list_unreviewed_notes', async () => {
    const row = host.querySelector<HTMLElement>('[data-smart-folder="unreviewed"]');
    expect(row, 'no Unreviewed row under Views').toBeTruthy();
    // M10: a remote change (another device) lands here too, not only AI writes.
    expect(row!.title).toBe("Notes changed by agents, Jodd's AI or another device that you have not reviewed");
    row!.click();
    await settle();
    expect(get(selectedSmartFolder)).toEqual({ account: ACCOUNT.id, kind: 'unreviewed' });
    expect(smartFolderCommand('unreviewed')).toBe('list_unreviewed_notes');
  });
});

describe('the Agents group (spec 2026-10-28 §6)', () => {
  const groupTitles = (group: Element) =>
    Array.from(group.querySelectorAll('.folder-item .folder-name')).map((n) => n.getAttribute('title'));

  it('holds agent folders and what is inside them; retired and legacy folders list with the rest', async () => {
    await expandAll();
    const groups = Array.from(host.querySelectorAll('.folder-group'));
    const agents = groups.find((g) => g.querySelector('.group-header')?.textContent?.trim() === 'Agents')!;
    expect(agents, 'no Agents group').toBeTruthy();
    expect(groupTitles(agents)).toEqual(['Agent', 'Projects', 'Claude']);
    const ordinary = groups.find((g) => !g.querySelector('.group-header'))!;
    expect(groupTitles(ordinary)).toEqual(['Notes', 'Extracts', 'Lessons', 'Work']);
    expect(host.textContent).not.toContain('Workflows');
  });

  it('a folder cannot be moved into an agent folder or anything inside one', async () => {
    const menu = await openMenuFor('Work');
    const targets = Array.from(menu.querySelectorAll('.submenu.folder-list .label')).map((l) => l.textContent?.trim());
    expect(targets).toEqual(expect.arrayContaining(['Notes', 'Lessons', 'Extracts']));
    for (const agent of ['Agent', 'Projects', 'Claude']) expect(targets).not.toContain(agent);
  });
});
