// @vitest-environment jsdom
//
// Account identity changed from a bare email to `{backend}:{email}` (see
// docs/superpowers/sdd/2026-08-19-account-identity), which makes it possible
// to hold two accounts with the SAME email on different backends
// (gmail:a@b.com and microsoft:a@b.com). The address alone can no longer
// tell them apart in the UI, so the backend must be named beside the
// address unconditionally — not only once a collision is detected.
import { get } from 'svelte/store';
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
import { SMART_FOLDER_NAMES } from '../viewName';

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

// The Views rows, the list header and the empty pane all name a Smart Folder.
// They used to spell the names separately, so a rename could change one and
// not the others; the rows now read SMART_FOLDER_NAMES like viewName does.
describe('Sidebar Smart Folder rows', () => {
  function rowNames(kind: string): string[] {
    return Array.from(host.querySelectorAll(`[data-smart-folder="${kind}"] .folder-name`))
      .map((el) => el.textContent?.trim() ?? '');
  }

  it('names every Smart Folder row from SMART_FOLDER_NAMES', () => {
    for (const [kind, name] of Object.entries(SMART_FOLDER_NAMES)) {
      // One row per account — both accounts render a Views group.
      expect(rowNames(kind), `row for ${kind}`).toEqual([name, name]);
    }
  });

  // selectSmartFolder leaves selectedFolder's value in place underneath, so a
  // folder row that checks only selectedFolder stays lit beside the Smart
  // Folder row — two rows highlighted for one view.
  it('highlights only the Smart Folder row, not the folder left underneath it', async () => {
    const activeNames = () => Array.from(host.querySelectorAll('.folder-item.active .folder-name'))
      .map((el) => el.textContent?.trim());
    expect(activeNames(), 'precondition: the Notes folder row is lit').toEqual(['Notes']);

    const stale = host.querySelector<HTMLElement>('[data-smart-folder="stale"]');
    stale!.click();
    await settle();

    expect(activeNames()).toEqual([SMART_FOLDER_NAMES.stale]);
  });

  // PR #140 review: picking another account from the panel left the Smart
  // Folder selected, so it kept showing the old account's view with no folder
  // row lit in the new one.
  it('picking another account from the panel leaves the Smart Folder', async () => {
    host.querySelector<HTMLElement>('[data-smart-folder="stale"]')!.click();
    await settle();
    expect(get(selectedSmartFolder)).not.toBeNull();

    (host.querySelector('.account-chip') as HTMLButtonElement).click();
    await settle();
    // Both accounts share an address; the one that is not current is Outlook.
    const pick = host.querySelector<HTMLButtonElement>('.account-row:not(.active) .account-row-pick')!;
    pick.click();
    await settle();

    expect(get(currentAccount)).toBe(MICROSOFT_ACCOUNT.id);
    expect(get(selectedSmartFolder)).toBeNull();
  });

  // Tags are per account (selectTag starts a fresh selection on a switch); a
  // filter carried over named tags the new account may not have, and NoteList
  // shows it ahead of the Notes folder the pick opened.
  it('picking another account from the panel leaves a tag filter', async () => {
    selectedTags.set(new Set(['trading']));
    await settle();

    (host.querySelector('.account-chip') as HTMLButtonElement).click();
    await settle();
    host.querySelector<HTMLButtonElement>('.account-row:not(.active) .account-row-pick')!.click();
    await settle();

    expect(get(currentAccount)).toBe(MICROSOFT_ACCOUNT.id);
    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes');
  });

  // The folder rows are not the only ones that must stand down: All and
  // Recently Deleted are set through selectedFolder too, so each needs the
  // same guard or it stays lit beside the Smart Folder.
  it.each([
    ['__ALL__', `All ${GMAIL_ACCOUNT.email}`],
    ['__TRASH__', 'Recently Deleted'],
  ])('a Smart Folder over %s highlights only the Smart Folder row', async (path, rowName) => {
    selectedFolder.set(path);
    await settle();
    expect(activeNames(), `precondition: ${rowName} is lit`).toEqual([rowName]);

    host.querySelector<HTMLElement>('[data-smart-folder="stale"]')!.click();
    await settle();

    expect(activeNames()).toEqual([SMART_FOLDER_NAMES.stale]);
  });
});

function activeNames(): (string | undefined)[] {
  return Array.from(host.querySelectorAll('.folder-item.active .folder-name'))
    .map((el) => el.textContent?.trim());
}

// The gmail account's `Notes` folder row, by name — `.folder-name` alone also
// matches the All row and the Smart Folder rows.
function notesRow(): HTMLElement {
  const row = Array.from(host.querySelectorAll<HTMLElement>(`[data-account-id="${GMAIL_ACCOUNT.id}"] .folder-item`))
    .find((el) => el.querySelector('.folder-name')?.textContent?.trim() === 'Notes');
  if (!row) throw new Error('no Notes folder row');
  return row;
}

function menuItem(label: string): HTMLButtonElement {
  const item = Array.from(host.querySelectorAll<HTMLButtonElement>('.folder-menu button'))
    .find((b) => b.textContent?.includes(label));
  if (!item) throw new Error(`no "${label}" in the folder menu`);
  return item;
}

// A path that sets selectedFolder while a Smart Folder is selected must leave
// the Smart Folder in the same step: NoteList shows the Smart Folder FIRST, so
// otherwise the list stays on it and — since PR #140 — no folder row lights
// up either. Nothing on screen says where the new note or folder went.
describe('Sidebar folder actions leave a Smart Folder', () => {
  beforeEach(async () => {
    host.querySelector<HTMLElement>('[data-smart-folder="unreviewed"]')!.click();
    await settle();
    expect(get(selectedSmartFolder), 'precondition').toEqual({ account: GMAIL_ACCOUNT.id, kind: 'unreviewed' });
  });

  it('"New note here" opens the folder it files the note in', async () => {
    notesRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    menuItem('New note here').click();
    flushSync();

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes');
    expect(activeNames()).toEqual(['Notes']);
  });

  async function newSubFolder(name: string) {
    notesRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    menuItem('New sub-folder').click();
    flushSync();
    const input = host.querySelector<HTMLInputElement>('.prompt-input')!;
    input.value = name;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    Array.from(host.querySelectorAll<HTMLButtonElement>('.prompt-btn'))
      .find((b) => b.textContent?.trim() === 'OK')!
      .click();
  }

  it('"New sub-folder" opens the new folder before create_folder answers', async () => {
    let answer!: (v: unknown) => void;
    invoke.mockImplementation((cmd: string) => cmd === 'create_folder'
      ? new Promise((res) => { answer = res; })
      : Promise.resolve(routeCommand(cmd)));

    await newSubFolder('Ideas');
    await settle();

    // Optimistic: the IPC is still out.
    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes/Ideas');

    answer({ id: 'Label_1', name: 'Notes/Ideas' });
    await settle();
    expect(get(selectedSmartFolder)).toBeNull();
  });

  it('a refused "New sub-folder" puts the Smart Folder back', async () => {
    vi.stubGlobal('alert', vi.fn());
    invoke.mockImplementation((cmd: string) => cmd === 'create_folder'
      ? Promise.reject('duplicate name')
      : Promise.resolve(routeCommand(cmd)));

    await newSubFolder('Ideas');
    await settle();

    expect(get(selectedSmartFolder)).toEqual({ account: GMAIL_ACCOUNT.id, kind: 'unreviewed' });
    expect(get(selectedFolder)).toBe('Notes');
    vi.unstubAllGlobals();
  });
});

// A tag filter shadows the folder the same way, one step down: NoteList shows
// it ahead of selectedFolder, so these actions stayed on the tag view.
describe('Sidebar folder actions leave a tag filter', () => {
  beforeEach(async () => {
    selectedTags.set(new Set(['trading']));
    await settle();
  });

  it('"New note here" opens the folder it files the note in', async () => {
    notesRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    menuItem('New note here').click();
    flushSync();

    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes');
  });

  it('a refused "New sub-folder" puts the tag filter back', async () => {
    vi.stubGlobal('alert', vi.fn());
    let refuse!: (e: unknown) => void;
    invoke.mockImplementation((cmd: string) => cmd === 'create_folder'
      ? new Promise((_, rej) => { refuse = rej; })
      : Promise.resolve(routeCommand(cmd)));

    notesRow().dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }));
    flushSync();
    menuItem('New sub-folder').click();
    flushSync();
    const input = host.querySelector<HTMLInputElement>('.prompt-input')!;
    input.value = 'Ideas';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    Array.from(host.querySelectorAll<HTMLButtonElement>('.prompt-btn'))
      .find((b) => b.textContent?.trim() === 'OK')!
      .click();
    await settle();

    // Optimistic: the new folder is open while the IPC is out.
    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes/Ideas');

    refuse('duplicate name');
    await settle();

    expect([...get(selectedTags)]).toEqual(['trading']);
    expect(get(selectedFolder)).toBe('Notes');
    vi.unstubAllGlobals();
  });
});
