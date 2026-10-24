// @vitest-environment jsdom
//
// Settings › Accounts › one account: the "Notes label / Metadata label"
// fields are Gmail labels (`notes_label` / `meta_label` are read by no other
// backend), so they render for Gmail only. Before this they sat in the
// `{:else}` of `{#if isLocalFs}` and every iCloud, Outlook and SSH account
// was offered two Gmail labels to edit. The other backends get a short,
// truthful line instead, and iCloud keeps its own block below.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '../__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'get_account_settings': return { notes_label: 'Notes', meta_label: 'Notes-Meta' };
    case 'update_account_settings': return { notes_label: 'Notes', meta_label: 'Notes-Meta' };
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import AccountPage from './AccountPage.svelte';
import { accounts } from '../../stores/notes';
import { dupStats } from '../../stores/dupStats';
import type { SectionForm } from '../../settings/sectionForm';

const base = { email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', status: 'active' };
const GMAIL = { ...base, id: 'gmail:a@b.com', backend_kind: 'gmail' };
const LEGACY = { ...base, id: 'a@b.com' }; // no backend_kind: written before the field, so Gmail
const ICLOUD = { ...base, id: 'icloud:a@b.com', backend_kind: 'icloud' };
const OUTLOOK = { ...base, id: 'microsoft:a@b.com', backend_kind: 'microsoft' };
const SSH = {
  ...base, id: 'ssh:me@box', email: 'me@box', backend_kind: 'ssh',
  ssh_target: 'me@box', root_dir: '/home/me/notes',
};

let host: HTMLElement;
let comp: (ReturnType<typeof mount> & { forms(): SectionForm[] }) | undefined;

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

async function open(acct: Record<string, unknown>) {
  accounts.set([acct as never]);
  comp = mount(AccountPage, {
    target: host,
    props: { accountId: acct.id as string, leave: async () => true },
  }) as typeof comp;
  await vi.waitFor(() => expect(host.querySelector('.loading')).toBeNull());
  await settle();
}

const fieldLabels = () =>
  Array.from(host.querySelectorAll('.field-label')).map((l) => l.textContent!.trim());

beforeEach(() => {
  invoke.mockClear();
  dupStats.set({});
  host = document.createElement('div');
  document.body.appendChild(host);
});

afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
});

describe('AccountPage label fields', () => {
  it.each([['gmail', GMAIL], ['a legacy account with no backend_kind', LEGACY]])(
    'shows the Gmail labels for %s',
    async (_name, acct) => {
      await open(acct);
      expect(fieldLabels()).toEqual(expect.arrayContaining(['Notes label', 'Metadata label']));
      expect(invoke).toHaveBeenCalledWith('get_account_settings', { accountId: acct.id });
    },
  );

  it.each([
    ['iCloud', ICLOUD, 'the ones in your iCloud account'],
    ['Outlook', OUTLOOK, "this account's Outlook Notes folder"],
    ['SSH', SSH, 'files in this folder'],
  ])('offers no Gmail labels on %s, and says why', async (_name, acct, line) => {
    await open(acct);
    expect(fieldLabels()).not.toContain('Notes label');
    expect(fieldLabels()).not.toContain('Metadata label');
    expect(host.querySelectorAll('input[placeholder="Notes"], input[placeholder="Notes-Meta"]')).toHaveLength(0);
    expect(invoke).not.toHaveBeenCalledWith('get_account_settings', expect.anything());
    const panel = host.querySelector('.backend-fields')!;
    expect(panel.textContent).toContain(line);
    expect(panel.textContent).toContain('There are no labels to set.');
  });

  it('an SSH account shows its server and folder, read-only', async () => {
    await open(SSH);
    const panel = host.querySelector('.backend-fields')!;
    expect(fieldLabels()).toEqual(expect.arrayContaining(['Server', 'Folder on server']));
    expect(panel.textContent).toContain('me@box');
    expect(panel.textContent).toContain('/home/me/notes');
    expect(panel.querySelector('input')).toBeNull();
  });

  it('iCloud keeps its own block', async () => {
    await open(ICLOUD);
    const labels = Array.from(host.querySelectorAll('button')).map((b) => b.textContent!.trim());
    expect(labels).toContain('Reauthenticate');
    expect(labels).toContain('Check what Jodd can write here');
  });

  it('a non-Gmail page never saves labels', async () => {
    await open(OUTLOOK);
    const [fields] = comp!.forms();
    expect(fields.isDirty()).toBe(false);
    await fields.save();
    expect(invoke).not.toHaveBeenCalledWith('update_account_settings', expect.anything());
  });
});
