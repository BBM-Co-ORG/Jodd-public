// @vitest-environment jsdom
//
// The folder tree from the keyboard (2026-11-09 workspace review, audit P1):
// sub-folders could not be opened without a mouse (the expand toggle was
// tabindex=-1), folder actions were right-click only, and every row was its
// own Tab stop.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags, selectedSmartFolder, hydratedFolders,
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() }));

import Sidebar from './Sidebar.svelte';

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' };
const FOLDERS = ['Notes', 'Notes/Work', 'Notes/Work/Q3', 'Notes/Home'];

function route(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders': return FOLDERS;
    case 'list_folder_kinds': return FOLDERS.map((f) => [f, 'user']);
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default: return [];
  }
}

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

async function settle() {
  for (let i = 0; i < 6; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}

beforeEach(async () => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(route(cmd)));
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

const rowNamed = (name: string) =>
  Array.from(host.querySelectorAll<HTMLElement>('nav .folder-item'))
    .find((r) => r.querySelector('.folder-name')?.textContent?.trim() === name) ?? null;

async function press(el: Element, key: string, init: KeyboardEventInit = {}) {
  el.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init }));
  await settle();
}

describe('Sidebar folder tree from the keyboard', () => {
  it('has one Tab stop: the open folder', () => {
    const stops = Array.from(host.querySelectorAll('nav .folder-item')).filter((r) => r.getAttribute('tabindex') === '0');
    expect(stops).toHaveLength(1);
    expect(stops[0]).toBe(rowNamed('Notes'));
    expect(rowNamed('Notes')!.getAttribute('aria-current')).toBe('true');
  });

  it('says which folders are open and closed', () => {
    expect(rowNamed('Notes')!.getAttribute('aria-expanded')).toBe('true');
    expect(rowNamed('Work')!.getAttribute('aria-expanded')).toBe('false');
    expect(rowNamed('Home')!.hasAttribute('aria-expanded')).toBe(false);
  });

  it('arrows move focus, and the Tab stop follows', async () => {
    const notesRow = rowNamed('Notes')!;
    notesRow.focus();
    // Sub-folders sort by name: Notes, Home, Work.
    await press(notesRow, 'ArrowDown');
    expect(document.activeElement).toBe(rowNamed('Home'));
    expect(rowNamed('Home')!.getAttribute('tabindex')).toBe('0');
    expect(notesRow.getAttribute('tabindex')).toBe('-1');
  });

  it('Right opens a closed folder and then steps into it; Left steps out and closes', async () => {
    const work = rowNamed('Work')!;
    work.focus();
    expect(rowNamed('Q3')).toBeNull();
    await press(work, 'ArrowRight');
    expect(rowNamed('Work')!.getAttribute('aria-expanded')).toBe('true');
    expect(rowNamed('Q3')).not.toBeNull();
    expect(document.activeElement).toBe(rowNamed('Work'));
    await press(rowNamed('Work')!, 'ArrowRight');
    expect(document.activeElement).toBe(rowNamed('Q3'));
    await press(rowNamed('Q3')!, 'ArrowLeft');
    expect(document.activeElement).toBe(rowNamed('Work'));
    await press(rowNamed('Work')!, 'ArrowLeft');
    expect(rowNamed('Q3')).toBeNull();
  });

  it('Shift+F10 opens the folder menu', async () => {
    const work = rowNamed('Work')!;
    work.focus();
    await press(work, 'F10', { shiftKey: true });
    expect(host.querySelector('.folder-menu')).not.toBeNull();
  });
});
