// @vitest-environment jsdom
//
// Account identity changed from a bare email to `{backend}:{email}` (see
// docs/superpowers/sdd/2026-08-19-account-identity), which makes it possible
// to hold two accounts with the SAME email on different backends
// (gmail:a@b.com and microsoft:a@b.com). The address alone can no longer
// tell them apart in the UI, so the backend must be named beside the
// address unconditionally — not only once a collision is detected.
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

// Same email, two different backends — exactly the state this project makes
// reachable now that Account.id is qualified by backend.
const GMAIL_ACCOUNT = {
  id: 'gmail:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'gmail',
  status: 'active',
};
const MICROSOFT_ACCOUNT = {
  id: 'microsoft:a@b.com',
  email: 'a@b.com',
  added_at: '2026-08-12T11:58:15Z',
  backend_kind: 'microsoft',
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
  accounts.set([GMAIL_ACCOUNT, MICROSOFT_ACCOUNT]);
  currentAccount.set(GMAIL_ACCOUNT.id);
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

// Each assertion is scoped to its OWN account row. Asserting that /Gmail/ and
// /Outlook/ appear somewhere in the sidebar's text proves only that both words
// were rendered — it passes just as happily when the two labels are on the
// wrong rows, which for two accounts sharing an address is the exact failure
// the label exists to prevent.
function backendTagFor(accountId: string): string {
  const row = host.querySelector(`[data-account-id="${accountId}"]`);
  if (!row) throw new Error(`no sidebar row for account ${accountId}`);
  const tag = row.querySelector('.account-backend-tag');
  if (!tag) throw new Error(`account ${accountId} renders no backend label`);
  return tag.textContent?.trim() ?? '';
}

describe('Sidebar account backend labeling', () => {
  it('names the backend beside the address so two same-email accounts are distinguishable', () => {
    expect(backendTagFor(GMAIL_ACCOUNT.id)).toBe('Gmail');
    expect(backendTagFor(MICROSOFT_ACCOUNT.id)).toBe('Outlook');
  });
});
