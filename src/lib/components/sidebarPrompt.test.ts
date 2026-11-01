// @vitest-environment jsdom
//
// The Sidebar's text prompt (folder rename, new vault name, tag rename) is a
// Modal: Modal owns Esc, backdrop, focus and the Tab trap; the prompt owns the
// input, Enter-to-confirm and the resolved value.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder,
  selectedTags, selectedSmartFolder, hydratedFolders,
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
  id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z',
  backend_kind: 'gmail', status: 'active',
};

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders': return ['Notes', 'Notes/__Extracts__'];
    case 'list_folder_kinds': return [['Notes/__Extracts__', 'system_workflow']];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'rename_folder': return { id: 'x', name: 'Renamed' };
    case 'platform_name': return 'macos';
    default: return [];
  }
}
const actions = () => invoke.mock.calls.filter(([cmd]) => cmd !== 'platform_name');

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

async function settle() {
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}

async function openFolderMenuItem(label: string): Promise<HTMLDialogElement> {
  for (let pass = 0; pass < 5; pass++) {
    const toggles = host.querySelectorAll<HTMLElement>('[aria-label="Expand"]');
    if (toggles.length === 0) break;
    toggles.forEach((t) => t.click());
    await settle();
  }
  host
    .querySelector<HTMLElement>('.folder-item .folder-name[title="Extracts"]')!
    .closest('.folder-item')!
    .dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  await settle();
  [...host.querySelectorAll<HTMLElement>('.folder-menu button')]
    .find((b) => (b.textContent ?? '').trim() === label)!
    .click();
  await settle();
  return document.querySelector('dialog')!;
}

async function openRenamePrompt(): Promise<HTMLDialogElement> {
  for (let pass = 0; pass < 5; pass++) {
    const toggles = host.querySelectorAll<HTMLElement>('[aria-label="Expand"]');
    if (toggles.length === 0) break;
    toggles.forEach((t) => t.click());
    await settle();
  }
  host
    .querySelector<HTMLElement>('.folder-item .folder-name[title="Extracts"]')!
    .closest('.folder-item')!
    .dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  await settle();
  [...host.querySelectorAll<HTMLElement>('.folder-menu button')]
    .find((b) => (b.textContent ?? '').includes('Rename'))!
    .click();
  await settle();
  return document.querySelector('dialog')!;
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
  document.body.replaceChildren();
});

function key(target: Element, value: string) {
  const ev = new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true });
  target.dispatchEvent(ev);
  return ev;
}

describe('Sidebar text prompt', () => {
  it('opens as a Modal titled with the prompt, input focused and selected', async () => {
    const dialog = await openRenamePrompt();
    expect(dialog.hasAttribute('data-presentation')).toBe(true);
    expect(dialog.querySelector('h2')!.textContent).toBe('Rename "Notes/__Extracts__"');
    const input = dialog.querySelector<HTMLInputElement>('input')!;
    expect(input.value).toBe('__Extracts__');
    expect(document.activeElement).toBe(input);
  });

  it('Enter confirms with the typed value', async () => {
    const dialog = await openRenamePrompt();
    const input = dialog.querySelector<HTMLInputElement>('input')!;
    input.value = '  Renamed  ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    key(input, 'Enter');
    await settle();
    expect(actions().filter(([cmd]) => cmd === 'rename_folder')).toEqual([
      ['rename_folder', { accountId: ACCOUNT.id, path: 'Notes/__Extracts__', newName: 'Renamed' }],
    ]);
    expect(document.querySelector('dialog')).toBeNull();
  });

  it('OK confirms, Cancel does not', async () => {
    let dialog = await openRenamePrompt();
    [...dialog.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Cancel')!.click();
    await settle();
    expect(document.querySelector('dialog')).toBeNull();
    expect(actions().some(([cmd]) => cmd === 'rename_folder')).toBe(false);

    dialog = await openRenamePrompt();
    const input = dialog.querySelector<HTMLInputElement>('input')!;
    input.value = 'Renamed';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    [...dialog.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'OK')!.click();
    await settle();
    expect(actions().filter(([cmd]) => cmd === 'rename_folder')).toHaveLength(1);
  });

  it('Esc cancels without renaming', async () => {
    const dialog = await openRenamePrompt();
    key(dialog.querySelector('input')!, 'Escape');
    await settle();
    expect(document.querySelector('dialog')).toBeNull();
    expect(actions().some(([cmd]) => cmd === 'rename_folder')).toBe(false);
  });

  it('an empty value resolves as a cancel', async () => {
    const dialog = await openRenamePrompt();
    const input = dialog.querySelector<HTMLInputElement>('input')!;
    input.value = '   ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    key(input, 'Enter');
    await settle();
    expect(actions().some(([cmd]) => cmd === 'rename_folder')).toBe(false);
  });
});

// Final review 2026-10-20: a destructive confirm is never a primary "OK".
describe('Sidebar Delete folder confirm', () => {
  it('names its verb and is danger-styled, not the primary', async () => {
    const dialog = await openFolderMenuItem('Delete');
    expect(dialog.querySelector('h2')!.textContent).toBe('Delete folder?');
    const buttons = [...dialog.querySelectorAll<HTMLButtonElement>('button')]
      .filter((b) => ['Cancel', 'Delete', 'OK'].includes(b.textContent?.trim() ?? ''));
    expect(buttons.map((b) => b.textContent?.trim())).toEqual(['Cancel', 'Delete']);
    const confirm = buttons[1];
    expect(confirm.classList.contains('danger')).toBe(true);
    expect(confirm.classList.contains('primary')).toBe(false);
  });
});
