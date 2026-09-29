// @vitest-environment jsdom
//
// Account identity changed from a bare email to `{backend}:{email}` (see
// docs/superpowers/sdd/2026-08-19-account-identity), which makes it possible
// to hold two accounts with the SAME email on different backends
// (gmail:a@b.com and microsoft:a@b.com). The address alone can no longer
// tell them apart in the UI, so the backend must be named beside the
// address unconditionally — not only once a collision is detected.
import './__fixtures__/dialogStub';
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
// Managed SSH: ssh_key set (Jodd holds the private key), distinct from
// Advanced SSH's ssh_target-only accounts (the user's own ssh-agent/config).
const SSH_MANAGED_ACCOUNT = {
  id: 'ssh:jodd-zf',
  email: 'jodd-zf',
  added_at: '2026-09-27T00:00:00Z',
  backend_kind: 'ssh',
  status: 'active',
  ssh_target: 'jodd@jodd-zf',
  ssh_key: 'abc',
};

function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_folders':
      return ['Notes'];
    case 'list_folder_kinds':
      return [['Notes', 'user']];
    case 'count_pending_pushes':
      return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'remove_account':
      return 'removed';
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

// Managed SSH's removal offers to revoke Jodd's key from the server
// (best-effort, on by default); every other backend, including Advanced SSH,
// gets no such offer since there is nothing there for Jodd to revoke.
describe('Sidebar account removal — Managed SSH revoke checkbox', () => {
  function openAccountPanel() {
    (host.querySelector('.account-chip') as HTMLButtonElement).click();
  }
  function removeButtonFor(email: string): HTMLButtonElement {
    const row = Array.from(host.querySelectorAll('.account-row')).find((r) =>
      r.textContent?.includes(email)
    );
    if (!row) throw new Error(`no account row for ${email}`);
    const button = row.querySelector('.account-row-remove');
    if (!button) throw new Error(`no remove button for ${email}`);
    return button as HTMLButtonElement;
  }
  function promptButtons(): HTMLButtonElement[] {
    return Array.from(host.querySelectorAll('.prompt-btn')) as HTMLButtonElement[];
  }

  beforeEach(async () => {
    accounts.set([GMAIL_ACCOUNT, SSH_MANAGED_ACCOUNT]);
    await settle();
  });

  it('offers to revoke the key when removing a Managed SSH account, and passes the choice through', async () => {
    openAccountPanel();
    await settle();
    removeButtonFor(SSH_MANAGED_ACCOUNT.email).click();
    await settle();

    const checkbox = host.querySelector('.confirm-checkbox input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox, 'a Managed SSH removal must offer the revoke checkbox').toBeTruthy();
    expect(checkbox.checked, 'the checkbox defaults on, per spec').toBe(true);

    const buttons = promptButtons();
    buttons[buttons.length - 1].click(); // Confirm (the last prompt-btn)
    await settle();

    expect(invoke).toHaveBeenCalledWith('remove_account', expect.objectContaining({
      accountId: SSH_MANAGED_ACCOUNT.id,
      revokeKey: true,
    }));
  });

  it('shows no checkbox, and sends revokeKey: null, when removing a non-Managed account', async () => {
    openAccountPanel();
    await settle();
    removeButtonFor(GMAIL_ACCOUNT.email).click();
    await settle();

    expect(host.querySelector('.confirm-checkbox')).toBeNull();

    const buttons = promptButtons();
    buttons[buttons.length - 1].click();
    await settle();

    expect(invoke).toHaveBeenCalledWith('remove_account', expect.objectContaining({
      accountId: GMAIL_ACCOUNT.id,
      revokeKey: null,
    }));
  });

  it('resets the revoke checkbox to checked before each new removal attempt, not inheriting a stale uncheck', async () => {
    openAccountPanel();
    await settle();
    removeButtonFor(SSH_MANAGED_ACCOUNT.email).click();
    await settle();

    let checkbox = host.querySelector('.confirm-checkbox input[type="checkbox"]') as HTMLInputElement;
    checkbox.click(); // uncheck it
    await settle();
    expect(checkbox.checked).toBe(false);

    // Cancel this attempt (first prompt-btn). removeAccount returns before
    // touching accountPanelOpen when cancelled, so the panel is still open —
    // do not toggle it again here.
    promptButtons()[0].click();
    await settle();

    // Remove the same account again — must default back to checked.
    removeButtonFor(SSH_MANAGED_ACCOUNT.email).click();
    await settle();
    checkbox = host.querySelector('.confirm-checkbox input[type="checkbox"]') as HTMLInputElement;
    expect(checkbox.checked).toBe(true);
  });
});
