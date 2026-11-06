// @vitest-environment jsdom
//
// The account actions, called directly — no Sidebar mounted. The desktop
// Sidebar's account panel and Settings › Accounts both call these same
// functions, so what each action does to the stores and the backend is
// asserted here once. What stays in the component tests is what only a
// mounted Sidebar can show: that the confirm dialog renders its revoke
// checkbox, that the checkbox defaults on and is reset per attempt, and that
// a store-routed add request opens the Sidebar's own dialog.
//
// Moved here, assertions kept: every case of removeAccountMessage.test.ts
// (the function now lives in this module), and Sidebar.test.ts's
// "non-Managed account: no checkbox, revokeKey: null" case — its "no
// checkbox" half is now the `checkboxLabel` the action hands its confirm.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
  accounts, notes, selectedNote, selectedFolder, currentAccount, isAuthenticated,
  refreshNotes, noteIndex, folderRefreshSignal, noteTagsByAccount,
} from '../stores/notes';
import { addAccountRequest } from '../stores/ui';
import type { Account, Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import {
  removeAccount, removeAccountMessage, setAccountActive, setAccountStatus,
  accountStatusChangeSeq, startAddAccount, addOAuthAccount, addICloudAccount,
  totalPending, totalUnsent, stopWaiting, stopWaitingMessage, REMOVE_ACCOUNT_CONFIRM,
} from './accountActions';

const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const commands = () => invoke.mock.calls.map((c) => c[0]).filter((c) => c !== 'platform_name');
const callOf = (cmd: string) => invoke.mock.calls.find((c) => c[0] === cmd);

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' } as Account;
const MICROSOFT = { id: 'microsoft:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'microsoft', status: 'active' } as Account;
const SSH_MANAGED = {
  id: 'ssh:jodd-zf', email: 'jodd-zf', added_at: '2026-09-27T00:00:00Z', backend_kind: 'ssh', status: 'active',
  ssh_target: 'jodd@jodd-zf', ssh_key: 'abc',
} as Account;
const NOTE = (account_id: string, uuid: string) => ({ uuid, id: 'm-' + uuid, account_id, title: uuid, body_html: '', date: '', label: 'Notes' }) as Note;

const ZERO = { notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0 };
function route(overrides: Record<string, unknown> = {}) {
  invoke.mockImplementation((cmd: string) => {
    if (cmd in overrides) {
      const v = overrides[cmd];
      return v instanceof Error ? Promise.reject(v) : Promise.resolve(v);
    }
    if (cmd === 'count_pending_pushes') return Promise.resolve(ZERO);
    if (cmd === 'remove_account') return Promise.resolve('removed');
    return Promise.resolve([]);
  });
}

beforeEach(() => {
  invoke.mockReset();
  route();
  accounts.set([GMAIL, MICROSOFT]);
  currentAccount.set(GMAIL.id);
  selectedFolder.set('Notes/Work');
  isAuthenticated.set(true);
  notes.set([NOTE(GMAIL.id, 'g1'), NOTE(MICROSOFT.id, 'm1')]);
  selectedNote.set(null);
  noteIndex.set(new Map());
  refreshNotes.set(() => {});
  addAccountRequest.set(null);
  vi.stubGlobal('alert', vi.fn());
});
afterEach(() => vi.unstubAllGlobals());

// ─── removeAccountMessage (moved from removeAccountMessage.test.ts) ──────
describe('removeAccountMessage', () => {
  const vault = { id: 'localfs:4A33B704-4C3E-4C27-8209-7237E26D1C1A', email: 'JoddVaultCopy', backend_kind: 'local_fs' } as Account;

  it('names the account the way the sidebar does, not by its id', () => {
    const m = removeAccountMessage(vault, 0);
    expect(m).toBe('Remove localfs:JoddVaultCopy from Jodd?');
    expect(m).not.toContain('4A33B704');
  });

  it('warns about a confirmed unsent count', () => {
    expect(removeAccountMessage(vault, 3)).toBe(
      'Remove localfs:JoddVaultCopy from Jodd? 3 unsent edit(s) have not reached the server yet and will be deleted with it.',
    );
  });

  it('warns when the unsent count could not be confirmed', () => {
    expect(removeAccountMessage(vault, undefined)).toContain('could not be confirmed');
  });

  it('uses the display name for a draining account too', () => {
    const m = removeAccountMessage({ ...vault, status: 'draining' } as Account, undefined);
    expect(m.startsWith('Remove localfs:JoddVaultCopy from Jodd? It\'s still finishing a sync')).toBe(true);
  });
});

describe('pending-push totals', () => {
  it('the drain count excludes blocked notes; the removal warning includes them', () => {
    const c = { notes: 1, deletes: 2, pins: 3, folders: 4, blocked: 5 };
    expect(totalPending(c)).toBe(10);
    expect(totalUnsent(c)).toBe(15);
  });
});

// ─── removeAccount ───────────────────────────────────────────────────────
describe('removeAccount', () => {
  const yes = (checked = true) => vi.fn().mockResolvedValue({ ok: true, checked });

  // Moved from Sidebar.test.ts ("shows no checkbox, and sends revokeKey: null,
  // when removing a non-Managed account").
  it('asks with no checkbox, and sends revokeKey: null, when removing a non-Managed account', async () => {
    const confirm = yes();
    await removeAccount(GMAIL, { confirm });

    expect(confirm.mock.calls[0][2]).toBeNull();
    expect(invoke).toHaveBeenCalledWith('remove_account', expect.objectContaining({
      accountId: GMAIL.id,
      revokeKey: null,
    }));
  });

  it('offers the revoke checkbox for a Managed SSH account and sends what the user chose', async () => {
    accounts.set([GMAIL, SSH_MANAGED]);
    const kept = yes(true);
    await removeAccount(SSH_MANAGED, { confirm: kept });
    expect(kept).toHaveBeenCalledWith(
      'Remove account?', expect.any(String), "Also remove Jodd's key from the server", REMOVE_ACCOUNT_CONFIRM,
    );
    expect(callOf('remove_account')![1]).toEqual({ accountId: SSH_MANAGED.id, revokeKey: true });

    invoke.mockClear();
    accounts.set([GMAIL, SSH_MANAGED]);
    await removeAccount(SSH_MANAGED, { confirm: yes(false) });
    expect(callOf('remove_account')![1]).toEqual({ accountId: SSH_MANAGED.id, revokeKey: false });
  });

  // A destructive confirm is never a primary "OK" (final review 2026-10-20).
  it('asks with a danger-styled "Remove" button', async () => {
    const confirm = yes();
    await removeAccount(GMAIL, { confirm });
    expect(confirm.mock.calls[0][3]).toEqual({ destructive: true, confirmLabel: 'Remove' });
  });

  it('a cancel removes nothing and reports cancelled', async () => {
    const onRemoved = vi.fn();
    const confirm = vi.fn().mockResolvedValue({ ok: false, checked: true });
    expect(await removeAccount(GMAIL, { confirm, onRemoved })).toBe('cancelled');
    expect(callOf('remove_account')).toBeUndefined();
    expect(onRemoved).not.toHaveBeenCalled();
    expect(get(accounts)).toEqual([GMAIL, MICROSOFT]);
  });

  it('warns about unsent work, blocked notes included, before asking', async () => {
    route({ count_pending_pushes: { notes: 1, deletes: 0, pins: 0, folders: 0, blocked: 2 } });
    const confirm = yes();
    await removeAccount(GMAIL, { confirm });
    expect(callOf('count_pending_pushes')![1]).toEqual({ accountId: GMAIL.id });
    expect(confirm.mock.calls[0][1]).toContain('3 unsent edit(s)');
  });

  it('an unreadable count warns rather than reading as zero', async () => {
    route({ count_pending_pushes: new Error('db locked') });
    const confirm = yes();
    await removeAccount(GMAIL, { confirm });
    expect(confirm.mock.calls[0][1]).toContain('could not be confirmed');
  });

  it('does not count a draining account, whose removal the backend queues', async () => {
    const draining = { ...GMAIL, status: 'draining' } as Account;
    const confirm = yes();
    await removeAccount(draining, { confirm });
    expect(callOf('count_pending_pushes')).toBeUndefined();
    expect(confirm.mock.calls[0][1]).toContain("still finishing a sync");
  });

  it('removed: drops the account and its notes, runs onRemoved, and switches away from it', async () => {
    const onRemoved = vi.fn();
    expect(await removeAccount(GMAIL, { confirm: yes(), onRemoved })).toBe('removed');
    expect(get(accounts)).toEqual([MICROSOFT]);
    expect(get(notes).map((n) => n.uuid)).toEqual(['m1']);
    expect(onRemoved).toHaveBeenCalledWith(GMAIL.id);
    expect(get(currentAccount)).toBe(MICROSOFT.id);
    expect(get(selectedFolder)).toBe('Notes');
    expect(get(isAuthenticated)).toBe(true);
  });

  it('removed: leaves the current account alone when another one was removed', async () => {
    await removeAccount(MICROSOFT, { confirm: yes() });
    expect(get(currentAccount)).toBe(GMAIL.id);
    expect(get(selectedFolder)).toBe('Notes/Work');
  });

  it('removing the last account signs out', async () => {
    accounts.set([GMAIL]);
    notes.set([NOTE(GMAIL.id, 'g1')]);
    selectedNote.set(NOTE(GMAIL.id, 'g1'));
    await removeAccount(GMAIL, { confirm: yes() });
    expect(get(accounts)).toEqual([]);
    expect(get(currentAccount)).toBeNull();
    expect(get(isAuthenticated)).toBe(false);
    expect(get(notes)).toEqual([]);
    expect(get(selectedNote)).toBeNull();
  });

  it('queued: marks the row pending_removal and keeps everything else', async () => {
    route({ remove_account: 'queued' });
    const onRemoved = vi.fn();
    expect(await removeAccount(GMAIL, { confirm: yes(), onRemoved })).toBe('queued');
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.pending_removal).toBe(true);
    expect(get(notes)).toHaveLength(2);
    expect(onRemoved).not.toHaveBeenCalled();
    expect(get(currentAccount)).toBe(GMAIL.id);
  });

  it('a failed remove_account changes nothing and reports failed', async () => {
    route({ remove_account: new Error('nope') });
    const onRemoved = vi.fn();
    expect(await removeAccount(GMAIL, { confirm: yes(), onRemoved })).toBe('failed');
    expect(get(accounts)).toEqual([GMAIL, MICROSOFT]);
    expect(onRemoved).not.toHaveBeenCalled();
  });
});

// ─── Deactivate / reactivate ─────────────────────────────────────────────
describe('setAccountActive', () => {
  it('deactivate flips the row to draining before set_account_status answers', async () => {
    let answer!: () => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === 'set_account_status' ? new Promise<void>((r) => { answer = r; }) : Promise.resolve([]));
    const pending = setAccountActive(GMAIL.id, false);
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('draining');
    expect(callOf('set_account_status')![1]).toEqual({ accountId: GMAIL.id, status: 'draining' });
    answer();
    await pending;
    expect(callOf('index_account')).toBeUndefined();
  });

  it('a refused deactivate rolls back and rejects with the error', async () => {
    route({ set_account_status: new Error('refused') });
    await expect(setAccountActive(GMAIL.id, false)).rejects.toThrow('refused');
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('active');
  });

  it('reactivate flips the row to active, then warms the account up', async () => {
    accounts.set([{ ...GMAIL, status: 'inactive' }, MICROSOFT]);
    const idx = [{ id: 'm-1', uuid: 'u1', label: 'Notes' }];
    route({ index_account: idx, list_note_tags: [{ uuid: 'u1', tag: 'work' }] });
    const refresh = vi.fn();
    refreshNotes.set(refresh);
    const signal = get(folderRefreshSignal);

    await setAccountActive(GMAIL.id, true);
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('active');
    expect(callOf('set_account_status')![1]).toEqual({ accountId: GMAIL.id, status: 'active' });
    await flush();

    expect(commands()).toEqual(expect.arrayContaining(['index_account', 'sync_pin_state', 'sync_tag_state', 'list_note_tags']));
    expect(get(noteIndex).get(GMAIL.id)).toEqual(idx);
    expect(get(noteTagsByAccount).get(GMAIL.id)?.get('u1')).toEqual(['work']);
    expect(get(folderRefreshSignal)).toBe(signal + 1);
    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it('a refused reactivate rolls back and does not warm up', async () => {
    accounts.set([{ ...GMAIL, status: 'inactive' }, MICROSOFT]);
    route({ set_account_status: new Error('refused') });
    await expect(setAccountActive(GMAIL.id, true)).rejects.toThrow('refused');
    await flush();
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('inactive');
    expect(callOf('index_account')).toBeUndefined();
  });
});

describe('setAccountStatus', () => {
  it('bumps the status-change sequence, and a refusal rolls back and returns false', async () => {
    const before = accountStatusChangeSeq();
    route({ set_account_status: new Error('refused') });
    expect(await setAccountStatus(GMAIL.id, 'inactive')).toBe(false);
    expect(accountStatusChangeSeq()).toBe(before + 1);
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('active');

    route();
    expect(await setAccountStatus(GMAIL.id, 'inactive')).toBe(true);
    expect(get(accounts).find((a) => a.id === GMAIL.id)?.status).toBe('inactive');
  });
});

describe('stopWaiting', () => {
  const DRAINING = { ...GMAIL, status: 'draining' } as Account;
  beforeEach(() => accounts.set([DRAINING, MICROSOFT]));
  const statusOf = (id: string) => get(accounts).find((a) => a.id === id)?.status;

  it('a confirmed 0 skips the confirm and flips the account to inactive', async () => {
    const confirm = vi.fn(async () => true);
    expect(await stopWaiting(DRAINING, { confirm })).toBe('stopped');
    expect(confirm).not.toHaveBeenCalled();
    expect(callOf('set_account_status')?.[1]).toEqual({ accountId: GMAIL.id, status: 'inactive' });
    expect(statusOf(GMAIL.id)).toBe('inactive');
  });

  it('a queue asks first, naming the count (blocked notes are not queued work)', async () => {
    route({ count_pending_pushes: { ...ZERO, notes: 2, folders: 1, blocked: 5 } });
    const confirm = vi.fn(async () => true);
    expect(await stopWaiting(DRAINING, { confirm })).toBe('stopped');
    expect(confirm).toHaveBeenCalledWith('Stop waiting?', stopWaitingMessage(3));
    expect(stopWaitingMessage(3)).toBe('3 edit(s) will stay on this device and will be sent if you reactivate this account.');
  });

  it('an unreadable count warns rather than reading as zero', async () => {
    route({ count_pending_pushes: new Error('db locked') });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    const confirm = vi.fn(async () => true);
    await stopWaiting(DRAINING, { confirm });
    expect(confirm).toHaveBeenCalledWith('Stop waiting?', stopWaitingMessage(undefined));
    expect(stopWaitingMessage(undefined)).toContain('could not be confirmed');
  });

  it('a cancel writes nothing', async () => {
    route({ count_pending_pushes: { ...ZERO, notes: 1 } });
    expect(await stopWaiting(DRAINING, { confirm: async () => false })).toBe('cancelled');
    expect(commands()).not.toContain('set_account_status');
    expect(statusOf(GMAIL.id)).toBe('draining');
  });

  it('a refused status write reports failed and rolls back', async () => {
    route({ set_account_status: new Error('refused') });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(await stopWaiting(DRAINING, { confirm: async () => true })).toBe('failed');
    expect(statusOf(GMAIL.id)).toBe('draining');
  });
});

// ─── Add account ─────────────────────────────────────────────────────────
describe('startAddAccount', () => {
  it('gmail opens the Google sign-in URL', async () => {
    route({ get_auth_url: 'https://accounts.example/auth' });
    startAddAccount('gmail');
    await flush();
    expect(callOf('get_auth_url')).toEqual(['get_auth_url', undefined]);
    expect(callOf('open_auth_url')![1]).toEqual({ url: 'https://accounts.example/auth' });
  });

  it('microsoft asks for the Microsoft sign-in URL', async () => {
    route({ get_auth_url: 'https://login.example/auth' });
    startAddAccount('microsoft');
    await flush();
    expect(callOf('get_auth_url')).toEqual(['get_auth_url', { backend: 'microsoft' }]);
    expect(callOf('open_auth_url')![1]).toEqual({ url: 'https://login.example/auth' });
  });

  it('a sign-in that cannot start says so', async () => {
    route({ get_auth_url: new Error('no client id') });
    await addOAuthAccount();
    expect(alert).toHaveBeenCalledWith('Failed to start sign-in: Error: no client id');
    expect(callOf('open_auth_url')).toBeUndefined();
  });

  it('icloud signs in, then shows and selects the new account', async () => {
    const ICLOUD = { id: 'icloud:me@icloud.com', email: 'me@icloud.com', backend_kind: 'icloud', status: 'active' } as Account;
    route({ icloud_sign_in: 'me@icloud.com', list_accounts: [GMAIL, MICROSOFT, ICLOUD] });
    const refresh = vi.fn();
    refreshNotes.set(refresh);
    const signal = get(folderRefreshSignal);

    startAddAccount('icloud');
    await flush();
    expect(commands()).toEqual(['icloud_sign_in', 'list_accounts']);
    expect(get(accounts)).toEqual([GMAIL, MICROSOFT, ICLOUD]);
    expect(get(currentAccount)).toBe(ICLOUD.id);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(get(folderRefreshSignal)).toBe(signal + 1);
  });

  it('an iCloud sign-in that does not complete says so', async () => {
    route({ icloud_sign_in: new Error('window closed') });
    await addICloudAccount();
    expect(alert).toHaveBeenCalledWith('iCloud sign-in did not complete: Error: window closed');
    expect(callOf('list_accounts')).toBeUndefined();
  });

  it('local folder and SSH are requests the Sidebar handles, each a new one', () => {
    startAddAccount('localfs');
    const first = get(addAccountRequest);
    expect(first?.kind).toBe('localfs');
    startAddAccount('ssh');
    expect(get(addAccountRequest)?.kind).toBe('ssh');
    expect(get(addAccountRequest)!.seq).toBeGreaterThan(first!.seq);
    startAddAccount('ssh_advanced');
    expect(get(addAccountRequest)?.kind).toBe('ssh_advanced');
    expect(commands()).toEqual([]);
  });
});
