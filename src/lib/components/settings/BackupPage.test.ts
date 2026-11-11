// @vitest-environment jsdom
//
// Settings › Backup: the destination list, the folder disclosure, and the
// per-account Back up now with its status line (spec 2026-10-23
// backup-restore). `invoke` and the event bus are mocked, so what is pinned
// is which commands the page sends and what it shows for their answers.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '../__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readable } from 'svelte/store';

type Handler = (e: { payload: unknown }) => void;
const handlers: Handler[] = [];
const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  void cmd;
  return [];
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (_name: string, h: Handler) => {
    handlers.push(h);
    return () => {
      const i = handlers.indexOf(h);
      if (i >= 0) handlers.splice(i, 1);
    };
  },
  emit: async () => {},
}));
const openDialog = vi.fn();
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: (...a: unknown[]) => openDialog(...a) }));
vi.mock('../../stores/platform', () => ({ isAndroid: readable(false), isMacos: readable(true), supportsIcloud: readable(true) }));

import BackupPage from './BackupPage.svelte';
import AccountPage from './AccountPage.svelte';
import { accounts } from '../../stores/notes';
import { destinations, statuses } from '../../backup/state';

const ACCT = 'gmail:a@x.com';
const DEST = { id: 'd1', name: 'Dropbox', kind: 'local', path: '/Users/me/Dropbox/notes' };

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}
const button = (label: string) =>
  Array.from(host.querySelectorAll('button')).find((x) => x.textContent!.trim() === label) as HTMLButtonElement | undefined;
const status = () => host.querySelector('[data-backup-status]')?.textContent?.trim() ?? '';
const calls = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);

function backend(over: Record<string, (args?: unknown) => unknown> = {}) {
  invoke.mockImplementation(async (cmd: string, args?: unknown) => {
    if (over[cmd]) return over[cmd](args);
    if (cmd === 'backup_destinations') return [DEST];
    if (cmd === 'backup_statuses') return [];
    return [];
  });
}

async function mountPage() {
  comp = mount(BackupPage, { target: host });
  await settle();
}

beforeEach(() => {
  host = document.createElement('div');
  document.body.append(host);
  accounts.set([{ id: ACCT, email: 'a@x.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  destinations.set([]);
  statuses.set([]);
  handlers.length = 0;
  invoke.mockReset();
  openDialog.mockReset();
  try { localStorage.clear(); } catch { /* jsdom without storage */ }
});
afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
});

describe('BackupPage', () => {
  it('lists destinations and runs a backup', async () => {
    backend({ backup_run: () => ({ stamp: '2026-10-24T09-15-00Z', notes: 12, partial: 0, skipped: 0, bytes: 1000 }) });
    await mountPage();
    expect(host.textContent).toContain('Dropbox');
    expect(host.textContent).toContain('/Users/me/Dropbox/notes');

    button('Back up now')!.click();
    await settle();

    expect(calls('backup_run')).toEqual([['backup_run', { accountId: ACCT, destinationId: 'd1' }]]);
    expect(status()).not.toContain('T09-15-00Z'); // local time, not the file stamp
    expect(status()).toContain(new Date(2026, 9, 24).getFullYear().toString());
    expect(status()).toContain('12 notes');
  });

  it('names partial notes beside skipped ones', async () => {
    backend({ backup_run: () => ({ stamp: '2026-10-24T09-15-00Z', notes: 12, partial: 2, skipped: 1, bytes: 1000 }) });
    await mountPage();
    button('Back up now')!.click();
    await settle();
    expect(status()).toContain('Backed up 12 notes (2 partial, 1 skipped)');
  });

  it('shows the not-synced message', async () => {
    backend({
      backup_run: () => {
        throw 'NOT_SYNCED_YET';
      },
    });
    await mountPage();
    button('Back up now')!.click();
    await settle();
    // A failed status says when it happened (live pass 2026-10-24: it looked still in progress).
    expect(status()).toMatch(/^Waiting for this account's first full sync · \S/);
  });

  it('shows another failure as it came', async () => {
    backend({
      backup_run: () => {
        throw 'disk is read-only';
      },
    });
    await mountPage();
    button('Back up now')!.click();
    await settle();
    expect(status()).toMatch(/^disk is read-only · \S/);
  });

  it('shows the stored status on open, and a stored not-synced failure in words', async () => {
    backend({
      backup_statuses: () => [
        { accountId: ACCT, destinationId: 'd1', atMs: 1_792_747_800_000, ok: false, message: 'NOT_SYNCED_YET' },
      ],
    });
    await mountPage();
    expect(status()).toContain("Waiting for this account's first full sync");
  });

  it('selects the folder this account last backed up to, and remembers a change', async () => {
    const D2 = { id: 'd2', name: 'USB', kind: 'local', path: '/Volumes/usb' };
    backend({
      backup_destinations: () => [DEST, D2],
      backup_statuses: () => [{ accountId: ACCT, destinationId: 'd2', atMs: 2, ok: true, message: 'Backed up 4 notes' }],
    });
    await mountPage();
    const select = host.querySelector('select[aria-label="Backup destination"]') as HTMLSelectElement;
    expect(select.value).toBe('d2');
    expect(status()).toContain('Backed up 4 notes');

    select.value = 'd1';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(localStorage.getItem(`jodd.backup.destination.${ACCT}`)).toBe('d1');
  });

  it('each account row opens a restore of that account', async () => {
    backend({ backup_list_slugs: () => [] });
    await mountPage();
    button('Restore…')!.click();
    await settle();
    expect(document.body.textContent).toContain('Restore from backup');
    expect(calls('backup_list_slugs')).toEqual([['backup_list_slugs', { destinationId: 'd1' }]]);
  });

  it('add folder shows the encryption disclosure and cancels cleanly', async () => {
    backend();
    openDialog.mockResolvedValue('/Users/me/Backups');
    await mountPage();

    button('Add local folder')!.click();
    await settle();

    expect(openDialog).toHaveBeenCalledOnce();
    expect(host.ownerDocument.body.textContent).toContain('Backups are not encrypted');
    expect(host.ownerDocument.body.textContent).toContain(
      'Jodd writes backups as plain files in the folder you choose. Anyone who can open that folder can read your notes. Encrypted backups are planned.',
    );
    expect(calls('backup_add_local_destination')).toHaveLength(0);

    const cancel = Array.from(document.querySelectorAll('button')).find((b) => b.textContent!.trim() === 'Cancel')!;
    cancel.click();
    await settle();

    expect(calls('backup_add_local_destination')).toHaveLength(0);
    expect(host.ownerDocument.body.textContent).not.toContain('Backups are not encrypted');
  });

  it('adds the folder once the disclosure is accepted', async () => {
    const added = { id: 'd2', name: 'Backups', kind: 'local', path: '/Users/me/Backups' };
    let list = [DEST];
    backend({
      backup_add_local_destination: () => {
        list = [DEST, added];
        return added;
      },
      backup_destinations: () => list,
    });
    openDialog.mockResolvedValue('/Users/me/Backups');
    await mountPage();

    button('Add local folder')!.click();
    await settle();
    const ok = Array.from(document.querySelectorAll('button')).find((b) => b.textContent!.trim() === 'OK')!;
    ok.click();
    await settle();

    expect(calls('backup_add_local_destination')).toEqual([
      ['backup_add_local_destination', { name: 'Backups', path: '/Users/me/Backups' }],
    ]);
    expect(host.textContent).toContain('/Users/me/Backups');
    // The button frees itself, so a second folder can be added.
    expect(button('Add local folder')!.disabled).toBe(false);
  });

  it('does nothing when the folder picker is cancelled', async () => {
    backend();
    openDialog.mockResolvedValue(null);
    await mountPage();
    button('Add local folder')!.click();
    await settle();
    expect(host.ownerDocument.body.textContent).not.toContain('Backups are not encrypted');
    expect(calls('backup_add_local_destination')).toHaveLength(0);
  });

  it('Browse backups opens the restore dialog on that destination', async () => {
    backend({ backup_list_slugs: () => [] });
    await mountPage();
    expect(host.querySelector('dialog')).toBeNull();

    button('Browse backups…')!.click();
    await settle();

    expect(host.textContent).toContain('Restore from backup');
    expect(calls('backup_list_slugs')).toEqual([['backup_list_slugs', { destinationId: 'd1' }]]);
  });

  it('removes a destination', async () => {
    let list = [DEST];
    backend({
      backup_remove_destination: () => {
        list = [];
      },
      backup_destinations: () => list,
    });
    await mountPage();
    button('Remove')!.click();
    await settle();
    expect(calls('backup_remove_destination')).toEqual([['backup_remove_destination', { id: 'd1' }]]);
    expect(host.textContent).not.toContain('/Users/me/Dropbox/notes');
  });

  it('status refreshes on backup-finished from another view', async () => {
    backend();
    await mountPage();
    expect(status()).toBe('');

    expect(handlers.length).toBeGreaterThan(0);
    for (const h of [...handlers]) {
      h({ payload: { accountId: ACCT, destinationId: 'd1', ok: true, message: 'Backed up 7 notes (2 skipped)' } });
    }
    await settle();

    expect(status()).toContain('Backed up 7 notes (2 skipped)');
  });

  it('stops listening when it goes away', async () => {
    backend();
    await mountPage();
    unmount(comp!);
    comp = undefined;
    await settle();
    expect(handlers).toHaveLength(0);
  });

  it('says so when there is no destination yet', async () => {
    backend({ backup_destinations: () => [] });
    await mountPage();
    expect(button('Back up now')).toBeUndefined();
    expect(host.textContent).toContain('Add a backup folder');
  });
});

describe('AccountPage backup line', () => {
  it('backs up just that account and shows its status', async () => {
    backend({
      backup_run: () => ({ stamp: '2026-10-24T09-15-00Z', notes: 3, partial: 0, skipped: 1, bytes: 10 }),
      get_account_settings: () => ({ notes_label: 'Notes', meta_label: 'Notes-Meta' }),
    });
    comp = mount(AccountPage, { target: host, props: { accountId: ACCT, leave: async () => true } });
    await vi.waitFor(() => expect(host.querySelector('.loading')).toBeNull());
    await settle();

    button('Back up now')!.click();
    await settle();

    expect(calls('backup_run')).toEqual([['backup_run', { accountId: ACCT, destinationId: 'd1' }]]);
    expect(status()).toContain('Backed up 3 notes (1 skipped)');
    expect(status()).not.toContain('T09-15-00Z'); // local time, not the file stamp
    expect(status()).toContain(new Date(2026, 9, 24).getFullYear().toString());
  });

  it('offers Back up now while the account waits to be removed, not once inactive', async () => {
    // Spec §7: an account in pending_removal (Draining) can still be backed up.
    backend({ get_account_settings: () => ({ notes_label: 'Notes', meta_label: 'Notes-Meta' }) });
    destinations.set([DEST as never]);
    for (const [status, offered] of [['draining', true], ['inactive', false]] as const) {
      accounts.set([{ id: ACCT, email: 'a@x.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status, pending_removal: status === 'draining' } as never]);
      comp = mount(AccountPage, { target: host, props: { accountId: ACCT, leave: async () => true } });
      await vi.waitFor(() => expect(host.querySelector('.loading')).toBeNull());
      await settle();
      expect(!!button('Back up now'), status).toBe(offered);
      unmount(comp);
      comp = undefined;
    }
  });
});
