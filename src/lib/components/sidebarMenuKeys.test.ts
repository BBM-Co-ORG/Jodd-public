// @vitest-environment jsdom
//
// The Sidebar's context menus and account panel from the keyboard (spec
// 2026-11-10, Keyboard and screen reader): a menu opened with Shift+F10
// takes focus, moves with the arrows, opens Move to with →, and gives focus
// back to its row on Esc. The account panel closes on Esc and on a click
// outside it.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(false),
}));

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const B = { id: 'icloud:c@d.com', email: 'c@d.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'icloud', status: 'active' };
const zero = { notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0 };
let countsFor: Record<string, unknown> = {};
let countFails = false;

const invoke = vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
  const a = (args ?? {}) as Record<string, unknown>;
  switch (cmd) {
    case 'list_folders': return ['Notes', 'Notes/Work', 'Notes/Work/Q1', 'Notes/Home'];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes':
      if (countFails) throw new Error('db locked');
      return countsFor[String(a.accountId)] ?? zero;
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
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
const startAddAccount = vi.fn();
vi.mock('../controllers/accountActions', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../controllers/accountActions')>()),
  startAddAccount: (kind: string) => startAddAccount(kind),
}));

import Sidebar from './Sidebar.svelte';
import { accounts, currentAccount } from '../stores/notes';
import { nextIndex } from '../menuKeys';

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

async function mountWith(list: unknown[], current: string | null) {
  accounts.set(list as never);
  currentAccount.set(current);
  comp = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  await settle();
  await vi.waitFor(() => expect(host.querySelector('.folder-name[title="Work"]')).not.toBeNull());
}

const key = (el: Element, k: string, init: KeyboardEventInit = {}) =>
  el.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init }));
const workRow = () => host.querySelector('.folder-name[title="Work"]')!.closest<HTMLElement>('.folder-item')!;
const menu = () => document.querySelector<HTMLElement>('.folder-menu');
const focusedLabel = () => (document.activeElement as HTMLElement | null)?.querySelector('.label')?.textContent?.trim();

beforeEach(() => {
  invoke.mockClear();
  countsFor = {};
  countFails = false;
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
});

describe('nextIndex', () => {
  it('wraps, and enters from either end', () => {
    expect(nextIndex('ArrowDown', -1, 3)).toBe(0);
    expect(nextIndex('ArrowUp', -1, 3)).toBe(2);
    expect(nextIndex('ArrowDown', 2, 3)).toBe(0);
    expect(nextIndex('ArrowUp', 0, 3)).toBe(2);
    expect(nextIndex('Home', 1, 3)).toBe(0);
    expect(nextIndex('End', 0, 3)).toBe(2);
    expect(nextIndex('Enter', 0, 3)).toBeNull();
    expect(nextIndex('ArrowDown', -1, 0)).toBeNull();
  });
});

describe('folder menu from the keyboard', () => {
  it('Shift+F10 opens it with focus on its first item; arrows move; Esc hands focus back', async () => {
    await mountWith([A], A.id);
    const row = workRow();
    row.focus();
    key(row, 'F10', { shiftKey: true });
    await settle();
    expect(menu()).not.toBeNull();
    expect(menu()!.getAttribute('aria-label')).toBe('Work folder');
    expect(focusedLabel()).toBe('Ask Jodd about this folder');
    key(document.activeElement!, 'ArrowDown');
    expect(focusedLabel()).toBe('New note here');
    key(document.activeElement!, 'ArrowUp');
    key(document.activeElement!, 'ArrowUp');
    // Wraps to the last item: Move to.
    expect(focusedLabel()).toBe('Move to');
    key(document.activeElement!, 'Escape');
    await settle();
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(workRow());
  });

  it('→ opens Move to and ← closes it, back on its trigger', async () => {
    await mountWith([A], A.id);
    workRow().focus();
    key(workRow(), 'F10', { shiftKey: true });
    await settle();
    key(document.activeElement!, 'End');
    const trigger = document.activeElement as HTMLElement;
    expect(trigger.getAttribute('aria-haspopup')).toBe('menu');
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    key(trigger, 'ArrowRight');
    await settle();
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
    expect(trigger.closest('.submenu-anchor')!.classList.contains('open')).toBe(true);
    const sub = trigger.closest('.submenu-anchor')!.querySelector('[role="menu"]')!;
    expect(sub.contains(document.activeElement)).toBe(true);
    key(document.activeElement!, 'ArrowLeft');
    await settle();
    expect(trigger.getAttribute('aria-expanded')).toBe('false');
    expect(document.activeElement).toBe(trigger);
    // Esc inside the menu proper still closes the whole menu.
    expect(menu()).not.toBeNull();
  });

  it('a click holds Move to open, so it is not hover-only', async () => {
    await mountWith([A], A.id);
    workRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
    await settle();
    const trigger = menu()!.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')!;
    trigger.click();
    flushSync();
    expect(trigger.getAttribute('aria-expanded')).toBe('true');
  });

  it('every item is a menuitem', async () => {
    await mountWith([A], A.id);
    workRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
    await settle();
    const buttons = Array.from(menu()!.querySelectorAll('button'));
    expect(buttons.length).toBeGreaterThan(3);
    for (const b of buttons) expect(b.getAttribute('role'), b.textContent ?? '').toBe('menuitem');
  });
});

// Critique 2026-11-10 (P2): Delete on a folder with folders inside was
// offered, then refused with a native alert(). The menu says so up front.
describe('folder menu: Delete', () => {
  const openMenuOn = (title: string) => {
    host.querySelector(`.folder-name[title="${title}"]`)!.closest('.folder-item')!
      .dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  };
  const deleteItem = () => Array.from(menu()!.querySelectorAll<HTMLButtonElement>('button'))
    .find((b) => b.textContent?.trim() === 'Delete')!;

  it('is disabled, with its reason, on a folder that holds folders', async () => {
    await mountWith([A], A.id);
    openMenuOn('Work');
    await settle();
    expect(deleteItem().disabled).toBe(true);
    expect(menu()!.querySelector('.menu-reason')!.textContent).toBe('Delete or move its folders first.');
  });

  it('is offered on a folder without', async () => {
    await mountWith([A], A.id);
    openMenuOn('Home');
    await settle();
    expect(deleteItem().disabled).toBe(false);
    expect(menu()!.textContent).not.toContain('Delete or move its folders first.');
  });
});

describe('account panel', () => {
  const chip = () => host.querySelector<HTMLButtonElement>('.account-chip')!;
  const panel = () => host.querySelector('.account-panel');

  it('Esc closes it and puts focus back on the chip', async () => {
    await mountWith([A, B], A.id);
    chip().click();
    flushSync();
    expect(panel()).not.toBeNull();
    expect(chip().getAttribute('aria-expanded')).toBe('true');
    expect(chip().getAttribute('aria-controls')).toBe(panel()!.id);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    flushSync();
    expect(panel()).toBeNull();
    expect(document.activeElement).toBe(chip());
  });

  it('a click outside closes it; a click inside does not', async () => {
    await mountWith([A, B], A.id);
    chip().click();
    flushSync();
    panel()!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    flushSync();
    expect(panel()).not.toBeNull();
    workRow().dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    flushSync();
    expect(panel()).toBeNull();
  });

  it('per-account buttons name their account', async () => {
    await mountWith([A, B], A.id);
    chip().click();
    flushSync();
    const labels = Array.from(panel()!.querySelectorAll('button[aria-label]')).map((b) => b.getAttribute('aria-label'));
    expect(labels).toContain('Remove a · Gmail');
    expect(labels).toContain('Settings for c · iCloud');
  });
});
