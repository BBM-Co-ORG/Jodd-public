// @vitest-environment jsdom
//
// Microsoft sign-in used to be hidden on Android because its only redirect
// was a loopback listener Android cannot run. It now redirects through the
// same App Links URL as Gmail, so the Android account panel offers it.
//
// iCloud stopped being an Android exclusion on 2026-09-10 (CLAUDE.md
// roadmap 4b): the live device pass proved the process-wide `CookieManager`
// jar carries the Apple session with no hidden second webview needed, so
// `supportsIcloud` now resolves `true` on Android and Sidebar offers
// "Add iCloud account" there (`Sidebar.svelte:1894`, gated on
// `$supportsIcloud`). Local Folder stays hidden for an unrelated, still-true
// reason — Android has no arbitrary filesystem to pick a folder from.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import type { Writable } from 'svelte/store';
import {
  accounts,
  notes,
  noteIndex,
  currentAccount,
  selectedFolder,
  selectedTags,
  selectedSmartFolder,
  hydratedFolders,
} from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));
vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return {
    isAndroid: writable(true),
    isMacos: writable(false),
    supportsIcloud: writable(true),
  };
});

import Sidebar from './Sidebar.svelte';
import { supportsIcloud as supportsIcloudReadable } from '../stores/platform';

// The real module exports `Readable<boolean>`; the mock above substitutes a
// `writable` so tests can flip it. Cast once here rather than at each call site.
const supportsIcloud = supportsIcloudReadable as Writable<boolean>;

const GMAIL_ACCOUNT = {
  id: 'gmail:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'gmail',
  status: 'active',
};

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes'];
    case 'list_folder_kinds':
      return [['Notes', 'user']];
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

beforeEach(async () => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(routeCommand(cmd)));
  notes.set([]);
  accounts.set([GMAIL_ACCOUNT]);
  currentAccount.set(GMAIL_ACCOUNT.id);
  selectedFolder.set('Notes');
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  supportsIcloud.set(true); // the real-device default as of the 2026-09-10 live pass

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

function panelActionLabels(): string[] {
  return Array.from(host.querySelectorAll('.account-panel-action .label')).map(
    (el) => el.textContent?.trim() ?? ''
  );
}

async function openAccountPanel() {
  const toggle = host.querySelector<HTMLElement>('.account-chip');
  if (!toggle) throw new Error('no account switcher rendered');
  toggle.click();
  await settle();
}

describe('Sidebar account panel on Android', () => {
  it('offers Gmail, Microsoft and iCloud, but not a local folder', async () => {
    await openAccountPanel();

    const labels = panelActionLabels();
    expect(labels).toContain('Add Gmail account');
    expect(labels).toContain('Add Microsoft account');
    expect(labels).toContain('Add iCloud account');
    expect(labels).not.toContain('Add Local Folder');
  });

  it('still hides iCloud when supportsIcloud is false (pins the gate itself, not the Android default)', async () => {
    supportsIcloud.set(false);
    await openAccountPanel();

    const labels = panelActionLabels();
    expect(labels).not.toContain('Add iCloud account');
  });
});
