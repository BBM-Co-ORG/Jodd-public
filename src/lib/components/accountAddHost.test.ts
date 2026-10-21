// @vitest-environment jsdom
//
// AccountAddHost owns the add-account flows that used to need a mounted
// Sidebar: Local Folder, both SSH dialogs, and the oauth-success post-add
// refresh. On the phone layout the Sidebar (Folders pane) is unmounted
// whenever another pane shows, and Settings › Accounts is the only add path
// there, so every case below runs with NO Sidebar mounted — except the
// single-handling cases at the end, which mount both to prove that exactly
// one of them answers.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import {
  accounts, notes, noteIndex, currentAccount, selectedFolder, selectedTags,
  selectedSmartFolder, hydratedFolders, refreshNotes, folderRefreshSignal,
} from '../stores/notes';
import { addAccountRequest, localFolderBusy } from '../stores/ui';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
// Every `listen` registration is recorded, so a test can fire an event at
// every listener that exists — and count how many exist.
const listeners: { name: string; fn: (e?: unknown) => unknown }[] = [];
vi.mock('@tauri-apps/api/event', () => ({
  listen: (name: string, fn: (e?: unknown) => unknown) => {
    const entry = { name, fn };
    listeners.push(entry);
    return Promise.resolve(() => {
      const i = listeners.indexOf(entry);
      if (i >= 0) listeners.splice(i, 1);
    });
  },
  emit: () => Promise.resolve(),
}));
vi.mock('../stores/platform', () => ({
  isAndroid: { subscribe: (f: (v: boolean) => void) => { f(false); return () => {}; } },
  isMacos: { subscribe: (f: (v: boolean) => void) => { f(false); return () => {}; } },
  supportsIcloud: { subscribe: (f: (v: boolean) => void) => { f(true); return () => {}; } },
}));

import AccountAddHost from './AccountAddHost.svelte';
import Sidebar from './Sidebar.svelte';
import { startAddAccount } from '../controllers/accountActions';

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const NEW_GMAIL = { id: 'gmail:new@b.com', email: 'new@b.com', added_at: '2026-10-20T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const OTHER = { id: 'microsoft:o@b.com', email: 'o@b.com', added_at: '2026-02-01T00:00:00Z', backend_kind: 'microsoft', status: 'active' };
const VAULT = { id: 'localfs:/x/Vault', email: '/x/Vault', added_at: '2026-10-20T00:00:00Z', backend_kind: 'localfs', status: 'active' };

let listAccountsReply: unknown[] = [GMAIL];
function routeCommand(cmd: string): unknown {
  switch (cmd) {
    case 'list_accounts': return listAccountsReply;
    case 'add_local_account': return VAULT;
    case 'list_folders': return ['Notes'];
    case 'list_folder_kinds': return [['Notes', 'user']];
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    default: return [];
  }
}

let target: HTMLElement;
const mounted: ReturnType<typeof mount>[] = [];
function mountHost() {
  const c = mount(AccountAddHost, { target });
  mounted.push(c);
  flushSync();
  return c;
}
function mountSidebar() {
  const c = mount(Sidebar, { target, props: { width: 200 } });
  mounted.push(c);
  flushSync();
  return c;
}
async function settle() {
  for (let i = 0; i < 8; i++) { await tick(); await Promise.resolve(); }
  flushSync();
}
function dialogButton(label: RegExp) {
  return [...target.querySelectorAll<HTMLButtonElement>('dialog[open] button')]
    .find((b) => label.test(b.textContent?.trim() ?? ''));
}
const callsOf = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(routeCommand(cmd)));
  listAccountsReply = [GMAIL];
  listeners.length = 0;
  addAccountRequest.set(null);
  localFolderBusy.set(false);
  notes.set([]);
  accounts.set([GMAIL as never]);
  currentAccount.set(GMAIL.id);
  selectedFolder.set('Notes');
  selectedTags.set(new Set());
  selectedSmartFolder.set(null);
  noteIndex.set(new Map());
  hydratedFolders.set(new Map());
  target = document.createElement('div');
  document.body.append(target);
});

afterEach(() => {
  while (mounted.length) unmount(mounted.pop()!);
  target.remove();
  vi.restoreAllMocks();
});

describe('AccountAddHost with no Sidebar mounted', () => {
  it('localfs request is handled with no Sidebar mounted', async () => {
    const { open } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(open).mockClear();
    vi.mocked(open).mockResolvedValueOnce('/x/Vault');
    const refresh = vi.fn();
    refreshNotes.set(refresh);
    listAccountsReply = [GMAIL, VAULT];
    mountHost();
    await settle();

    startAddAccount('localfs');
    await settle();
    expect(open).toHaveBeenCalledWith(expect.objectContaining({ directory: true }));
    expect(get(addAccountRequest)).toBeNull(); // consumed
    expect(get(localFolderBusy)).toBe(true);

    // The encryption disclosure, then the vault name — the host's own dialogs.
    expect(target.querySelector('dialog[open]')?.textContent).toMatch(/not encrypted/);
    dialogButton(/^OK$/)!.click();
    await settle();
    const input = target.querySelector<HTMLInputElement>('dialog[open] input.prompt-input')!;
    expect(input.value).toBe('Vault');
    dialogButton(/^OK$/)!.click();
    await settle();

    expect(invoke).toHaveBeenCalledWith('add_local_account', { path: '/x/Vault', name: 'Vault' });
    expect(get(accounts).map((a) => a.id)).toEqual([GMAIL.id, VAULT.id]);
    expect(get(currentAccount)).toBe(VAULT.id);
    expect(refresh).toHaveBeenCalled();
    expect(get(localFolderBusy)).toBe(false);
    expect(invoke).not.toHaveBeenCalledWith('pick_local_folder');
  });

  it('ssh request opens the SSH dialog with no Sidebar mounted', async () => {
    mountHost();
    await settle();
    startAddAccount('ssh');
    await settle();
    expect(target.querySelector('[data-testid="easy-ssh-host"]')).toBeTruthy();
    expect(get(addAccountRequest)).toBeNull();

    startAddAccount('ssh_advanced');
    await settle();
    expect(target.textContent).toContain('Add SSH server');
  });

  it('a request made before mount is handled on mount', async () => {
    startAddAccount('ssh');
    expect(get(addAccountRequest)?.kind).toBe('ssh'); // kept, not dropped
    mountHost();
    await settle();
    expect(target.querySelector('[data-testid="easy-ssh-host"]')).toBeTruthy();
    expect(get(addAccountRequest)).toBeNull();
  });

  it('a consumed request is not replayed by a later mount', async () => {
    const first = mountHost();
    await settle();
    startAddAccount('ssh');
    await settle();
    unmount(first);
    mounted.splice(mounted.indexOf(first), 1);
    target.replaceChildren();
    mountHost();
    await settle();
    expect(target.querySelector('[data-testid="easy-ssh-host"]')).toBeNull();
  });

  it('oauth-success selects the new account with no Sidebar mounted', async () => {
    const refresh = vi.fn();
    refreshNotes.set(refresh);
    const signal = get(folderRefreshSignal);
    mountHost();
    await settle();
    listAccountsReply = [GMAIL, NEW_GMAIL];

    const handlers = listeners.filter((l) => l.name === 'oauth-success');
    expect(handlers).toHaveLength(1);
    await handlers[0].fn();
    await settle();

    expect(get(accounts).map((a) => a.id)).toEqual([GMAIL.id, NEW_GMAIL.id]);
    expect(get(currentAccount)).toBe(NEW_GMAIL.id);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(get(folderRefreshSignal)).toBe(signal + 1);
  });

  it('oauth-success for an account already known (a resumed sign-in) keeps the selection', async () => {
    // The selection is NOT the first account, so a handler that picked
    // list[0], or any account it wrongly took for new, would move it.
    accounts.set([GMAIL as never, OTHER as never]);
    currentAccount.set(OTHER.id);
    mountHost();
    await settle();
    listAccountsReply = [GMAIL, OTHER];
    const before = callsOf('list_accounts').length;
    await listeners.find((l) => l.name === 'oauth-success')!.fn();
    await settle();
    expect(callsOf('list_accounts').length - before).toBe(1);
    expect(get(currentAccount)).toBe(OTHER.id);
  });

  it('unmounting mid-flow frees the Local Folder busy flag', async () => {
    const { open } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(open).mockImplementationOnce(() => new Promise(() => {})); // never answers
    const c = mountHost();
    await settle();
    startAddAccount('localfs');
    await settle();
    expect(get(localFolderBusy)).toBe(true);
    unmount(c);
    mounted.splice(mounted.indexOf(c), 1);
    expect(get(localFolderBusy)).toBe(false);
  });

  it('stops listening for oauth-success when unmounted', async () => {
    const c = mountHost();
    await settle();
    expect(listeners.filter((l) => l.name === 'oauth-success')).toHaveLength(1);
    unmount(c);
    mounted.splice(mounted.indexOf(c), 1);
    await settle();
    expect(listeners.filter((l) => l.name === 'oauth-success')).toHaveLength(0);
  });
});

describe('single handling with the Sidebar and the host both mounted', () => {
  it('an ssh request opens exactly one dialog', async () => {
    mountSidebar();
    mountHost();
    await settle();
    startAddAccount('ssh');
    await settle();
    expect(target.querySelectorAll('[data-testid="easy-ssh-host"]')).toHaveLength(1);
    expect(get(addAccountRequest)).toBeNull();
  });

  it('a localfs request opens the folder picker exactly once', async () => {
    const { open } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(open).mockClear();
    mountSidebar();
    mountHost();
    await settle();
    startAddAccount('localfs');
    await settle();
    expect(open).toHaveBeenCalledTimes(1);
  });

  it('the Sidebar alone does not consume a request', async () => {
    mountSidebar();
    await settle();
    startAddAccount('ssh');
    await settle();
    expect(target.querySelector('[data-testid="easy-ssh-host"]')).toBeNull();
    expect(get(addAccountRequest)?.kind).toBe('ssh'); // still waiting for the host
  });

  it('exactly one oauth-success listener exists, and it refetches once', async () => {
    mountSidebar();
    mountHost();
    await settle();
    listAccountsReply = [GMAIL, NEW_GMAIL];
    const handlers = listeners.filter((l) => l.name === 'oauth-success');
    expect(handlers).toHaveLength(1);
    const before = callsOf('list_accounts').length;
    for (const h of handlers) await h.fn();
    await settle();
    expect(callsOf('list_accounts').length - before).toBe(1);
    expect(get(currentAccount)).toBe(NEW_GMAIL.id);
  });

  it("the Sidebar's Add Local Folder button reaches the host, and is disabled while the host is busy", async () => {
    const { open } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(open).mockClear();
    let pick!: (v: string | null) => void;
    vi.mocked(open).mockImplementationOnce(() => new Promise((r) => { pick = r as (v: string | null) => void; }));
    mountSidebar();
    mountHost();
    await settle();
    const openMenu = () => target.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')!.click();
    const addBtn = () => [...target.querySelectorAll<HTMLButtonElement>('.account-panel-action')]
      .find((b) => b.textContent?.includes('Add Local Folder'))!;
    openMenu();
    flushSync();
    addBtn().click();
    await settle();
    expect(open).toHaveBeenCalledTimes(1);
    openMenu();
    flushSync();
    expect(addBtn().disabled).toBe(true);
    pick(null); // cancelled
    await settle();
    expect(addBtn().disabled).toBe(false);
  });
});
