// @vitest-environment jsdom
//
// The restore dialog (spec 2026-10-23 backup-restore §6.1): account → version
// → what → where → preview → restore. `invoke` is mocked, so what is pinned is
// which commands go out with which payloads, and what the dialog shows for the
// answers. The dialog keeps working state only; the notes store is never
// touched (the backend's `remote-changed` refreshes it).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '../__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readable } from 'svelte/store';

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  void cmd;
  return [];
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('../../stores/platform', () => ({ isAndroid: readable(false), isMacos: readable(true), supportsIcloud: readable(true) }));

import RestoreDialog from './RestoreDialog.svelte';
import { accounts, capabilitiesByAccount } from '../../stores/notes';

const SRC = 'gmail:a@x.com';
const OTHER = 'microsoft:b@y.com';
const STAMP = '2026-10-24T09-15-00Z';

const note = (uuid: string, title: string, folder: string, over: Record<string, unknown> = {}) => ({
  uuid, title, folder, modified: null, contentSha256: 'ab', attachments: 0, partial: null, ...over,
});
const manifest = (over: Record<string, unknown> = {}) => ({
  joddBackup: 1, accountId: SRC, backend: 'gmail', rootLabel: 'Notes', createdMs: 1_792_747_800_000,
  appVersion: '0.36.3', device: 'macos', trigger: 'manual', keep: false, zipBytes: 2048,
  zipSha256: 'cd', folders: ['Notes', 'Notes/Work'], skipped: [],
  notes: [
    note('n1', 'Plan', 'Notes/Work'),
    note('n2', 'Budget', 'Notes/Work', { partial: 'inline objects' }),
    note('n3', 'Hello', 'Notes'),
  ],
  ...over,
});
const version = (over: Record<string, unknown> = {}) => ({ slug: 'a-x-com', stamp: STAMP, manifest: manifest(over) });

const PLAN = {
  notes: [
    { sourceUuid: 'n1', title: 'Plan', targetFolder: 'Notes/Work', action: { kind: 'create' }, losses: [] },
    { sourceUuid: 'n2', title: 'Budget', targetFolder: 'Notes/Work', action: { kind: 'create' }, losses: [] },
    { sourceUuid: 'n3', title: 'Hello', targetFolder: 'Notes', action: { kind: 'create' }, losses: [] },
  ],
  foldersToCreate: [],
};
const REPORT = { created: 3, replaced: 0, skipped: 0, refused: [], losses: [] };

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;
const onclose = vi.fn();

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}
const buttons = () => Array.from(host.querySelectorAll('button'));
// "Restore" also finds "Restore 4 notes": the button names its count once the preview has one.
const button = (label: string) =>
  buttons().find((x) => {
    const t = x.textContent!.trim();
    return t === label || t.startsWith(`${label} `);
  }) as HTMLButtonElement | undefined;
const labelled = (text: string) =>
  Array.from(host.querySelectorAll('label')).find((l) => l.textContent!.includes(text))?.querySelector('input') as HTMLInputElement | undefined;
const click = async (label: string) => {
  const b = button(label);
  if (!b) throw new Error(`no button "${label}"; buttons: ${buttons().map((x) => x.textContent!.trim()).join(' | ')}`);
  b.click();
  await settle();
};
const tick_ = async (text: string) => {
  const i = labelled(text);
  if (!i) throw new Error(`no input labelled "${text}"`);
  i.click();
  await settle();
};
const calls = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);

function backend(over: Record<string, (args?: unknown) => unknown> = {}, versions = [version()]) {
  invoke.mockImplementation(async (cmd: string, args?: unknown) => {
    if (over[cmd]) return over[cmd](args);
    if (cmd === 'backup_list_slugs') return ['a-x-com'];
    if (cmd === 'backup_list_versions') return versions;
    if (cmd === 'restore_preview') return PLAN;
    if (cmd === 'restore_apply') return REPORT;
    return [];
  });
}

async function open(props: { open?: boolean } = {}) {
  comp = mount(RestoreDialog, { target: host, props: { destinationId: 'd1', open: props.open ?? true, onclose } });
  await settle();
}
async function toVersion() {
  await tick_('a@x.com');
  await click('Next');
}
async function toWhat() {
  await toVersion();
  await click('Next');
}
async function toWhere() {
  await toWhat();
  await click('Next');
}
async function toPreview() {
  await toWhere();
  await click('Review…');
}

beforeEach(() => {
  host = document.createElement('div');
  document.body.append(host);
  accounts.set([
    { id: SRC, email: 'a@x.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' },
    { id: OTHER, email: 'b@y.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'microsoft', status: 'active' },
  ] as never);
  capabilitiesByAccount.set({});
  invoke.mockReset();
  onclose.mockReset();
});
afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
});

describe('RestoreDialog', () => {
  it('walks account → version → selection → preview → apply', async () => {
    backend();
    await open();

    expect(calls('backup_list_slugs')).toEqual([['backup_list_slugs', { destinationId: 'd1' }]]);
    expect(calls('backup_list_versions')).toEqual([['backup_list_versions', { destinationId: 'd1', slug: 'a-x-com' }]]);
    // The account's name, not the slug or the raw id (live pass 2026-10-24).
    expect(host.textContent).toContain('a@x.com');
    expect(host.textContent).not.toContain('gmail:a@x.com');
    expect(host.textContent).not.toContain('a-x-com');

    await toVersion();
    expect(host.textContent).toContain('Manual');
    expect(host.textContent).toContain('3 notes');
    expect(host.textContent).toContain('2 KB');

    await click('Next'); // the newest version is preselected
    expect(host.textContent).toContain('Plan');
    expect(host.textContent).toContain('Budget');
    expect(host.textContent).toContain('Hello');
    expect(host.textContent).toContain('3 of 3 notes selected');

    await click('Next');
    expect(host.textContent).toContain('Original folders');
    await click('Review…');

    const req = {
      destinationId: 'd1', slug: 'a-x-com', stamp: STAMP, sourceUuids: ['n1', 'n2', 'n3'],
      targetAccount: SRC, placement: 'original', existing: 'keepBoth',
    };
    expect(calls('restore_preview')).toEqual([['restore_preview', { req }]]);
    expect(calls('restore_apply')).toHaveLength(0);
    expect(host.textContent).toContain('3 notes will be created');

    await click('Restore');
    expect(calls('restore_apply')).toEqual([['restore_apply', { req }]]);
    expect(host.textContent).toContain('Created 3');

    await click('Done');
    expect(onclose).toHaveBeenCalledOnce();
  });

  it('ticking a folder selects its notes', async () => {
    backend();
    await open();
    await toWhat();

    await click('Select none');
    expect(labelled('Plan')!.checked).toBe(false);
    expect(host.textContent).toContain('0 of 3 notes selected');
    expect(button('Next')!.disabled).toBe(true);

    await tick_('Work');
    expect(labelled('Plan')!.checked).toBe(true);
    expect(labelled('Budget')!.checked).toBe(true);
    expect(labelled('Hello')!.checked).toBe(false);
    expect(host.textContent).toContain('2 of 3 notes selected');
    expect(host.textContent).toContain('partial'); // Budget's badge

    await click('Next');
    await click('Review…');
    expect((calls('restore_preview')[0][1] as { req: { sourceUuids: string[] } }).req.sourceUuids).toEqual(['n1', 'n2']);
  });

  it('search narrows the tree to matching notes', async () => {
    backend();
    await open();
    await toWhat();

    const search = host.querySelector('input[type="search"]') as HTMLInputElement;
    search.value = 'budg';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();

    expect(labelled('Budget')).toBeDefined();
    expect(labelled('Plan')).toBeUndefined();
    expect(labelled('Hello')).toBeUndefined();
  });

  it('replace option hidden for cross-account restore', async () => {
    backend();
    await open();
    await toWhere();

    expect(labelled('Replace')).toBeDefined();
    expect(labelled('Keep both')!.checked).toBe(true);

    const target = host.querySelector('select[aria-label="Restore into"]') as HTMLSelectElement;
    target.value = OTHER;
    target.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();

    expect(labelled('Replace')).toBeUndefined();
    expect(labelled('Keep both')).toBeUndefined();

    await click('Review…');
    const req = (calls('restore_preview')[0][1] as { req: Record<string, unknown> }).req;
    expect(req.targetAccount).toBe(OTHER);
    expect(req.existing).toBe('keepBoth');
  });

  it('offers only accounts that can write notes', async () => {
    backend();
    capabilitiesByAccount.set({ [OTHER]: { has_trash: false, writes: { notes: false, relocate: true, folders: false, sidecars: false } } });
    await open();
    await toWhere();
    const target = host.querySelector('select[aria-label="Restore into"]') as HTMLSelectElement;
    expect(Array.from(target.options).map((o) => o.value)).toEqual([SRC]);
  });

  it('placement under a new folder sends the dated name', async () => {
    backend();
    await open();
    await toWhere();
    await tick_('Under a new folder');
    await click('Review…');
    const req = (calls('restore_preview')[0][1] as { req: { placement: { underNewFolder: string } } }).req;
    expect(req.placement.underNewFolder).toMatch(/^Restored a@x\.com \d{4}-\d{2}-\d{2}$/);
  });

  it('loss list is shown before confirm', async () => {
    let release!: (p: unknown) => void;
    const pending = new Promise((r) => (release = r));
    backend({ restore_preview: () => pending });
    await open();
    await toPreview();

    // While the preview is computing there is nothing to confirm.
    expect(button('Restore')!.disabled).toBe(true);

    release({
      notes: [
        {
          sourceUuid: 'n1', title: 'Plan', targetFolder: 'Notes', action: { kind: 'create' },
          losses: [{ kind: 'folderFlattened', from: 'Notes/Work' }, { kind: 'pin' }, { kind: 'attachment', filename: 'a.png' }, { kind: 'tagsAsKeywords' }, { kind: 'partial', reason: 'inline objects' }],
        },
      ],
      foldersToCreate: [],
    });
    await settle();

    const text = host.textContent!;
    expect(text).toContain('Notes/Work will be placed in the top folder — this account has no folders');
    expect(text).toContain("a.png can't be stored in this account");
    expect(text).toContain("Pin won't carry over");
    expect(text).toContain('Tags will be added as a Keywords line');
    expect(text).toContain("Backed up without the parts Jodd can't read (inline objects)");
    expect(button('Restore')!.disabled).toBe(false);
    expect(button('Restore')!.textContent!.trim()).toMatch(/^Restore \d+ notes?$/);
  });

  it('shows refused notes with their reason and counts each action', async () => {
    backend({
      restore_preview: () => ({
        notes: [
          { sourceUuid: 'n1', title: 'Plan', targetFolder: 'Notes', action: { kind: 'create' }, losses: [] },
          { sourceUuid: 'n2', title: 'Budget', targetFolder: 'Notes', action: { kind: 'keepBoth', existingUuid: 'x' }, losses: [] },
          { sourceUuid: 'n3', title: 'Hello', targetFolder: 'Notes', action: { kind: 'refuse', reason: 'too large for this account' }, losses: [] },
        ],
        foldersToCreate: ['Notes/Restored'],
      }),
    });
    await open();
    await toPreview();
    const text = host.textContent!;
    expect(text).toContain('1 note will be created');
    expect(text).toContain('Hello');
    expect(text).toContain('too large for this account');
    expect(text).toContain('Notes/Restored');
  });

  it('an empty backup says so and cannot be taken further', async () => {
    backend({}, [version({ notes: [], folders: ['Notes'] })]);
    await open();
    await toVersion();
    expect(host.textContent).toContain('Empty');
    expect(host.textContent).toContain('This backup is empty');
    expect(button('Next')!.disabled).toBe(true);
  });

  it('the Keep both hint shows only when another choice could restore something', async () => {
    const skipPlan = (kind: string) => ({
      notes: [{ sourceUuid: 'n1', title: 'Plan', targetFolder: 'Notes/Work', action: { kind }, losses: [] }],
      foldersToCreate: [],
    });
    backend({ restore_preview: () => skipPlan('skipAlreadyRestored') });
    await open();
    await toPreview();
    expect(host.textContent).toContain('Nothing to restore');
    expect(host.textContent).not.toContain('choose Keep both');
    if (comp) unmount(comp);
    comp = undefined;
    host.innerHTML = '';

    backend({ restore_preview: () => skipPlan('skipExisting') });
    await open();
    await toWhere();
    await tick_('Skip');
    await click('Review…');
    expect(host.textContent).toContain('choose Keep both');
  });

  it('no Keep both hint where Keep both is already chosen or not offered', async () => {
    const skipPlan = () => ({
      notes: [{ sourceUuid: 'n1', title: 'Plan', targetFolder: 'Notes', action: { kind: 'skipContentMatch' }, losses: [] }],
      foldersToCreate: [],
    });
    // Same account, Keep both already chosen (the default).
    backend({ restore_preview: skipPlan });
    await open();
    await toPreview();
    expect(host.textContent).toContain('Nothing to restore');
    expect(host.textContent).not.toContain('choose Keep both');
    if (comp) unmount(comp);
    comp = undefined;
    host.innerHTML = '';

    // Another account: the choice is not on the Where step at all.
    backend({ restore_preview: skipPlan });
    await open();
    await toWhere();
    const target = host.querySelector('select[aria-label="Restore into"]') as HTMLSelectElement;
    target.value = OTHER;
    target.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    await click('Review…');
    expect(host.textContent).toContain('already in this account with the same text');
    expect(host.textContent).not.toContain('choose Keep both');
  });

  it('opened for one account, it starts on that account\'s backups', async () => {
    backend();
    comp = mount(RestoreDialog, { target: host, props: { destinationId: 'd1', open: true, onclose, initialAccountId: SRC } });
    await settle();
    expect(host.textContent).toContain('Which backup of a@x.com?');
  });

  it('the where step offers any account and a way to add one', async () => {
    backend();
    await open();
    await toWhere();
    expect(host.textContent).toContain('Any of your accounts');
    expect(button('add an account…')).toBeTruthy();
    // The new-folder name uses the account's name, made safe for a folder.
    expect(host.textContent).toMatch(/Restored a@x\.com \d{4}-\d{2}-\d{2}/);
  });

  it('the where step says Review only checks', async () => {
    backend();
    await open();
    await toWhere();
    expect(button('Review…')).toBeTruthy();
    expect(host.textContent).toContain('Nothing changes until you press Restore');
  });

  it('target waiting for first sync shows message and disables Restore', async () => {
    backend({
      restore_preview: () => {
        throw 'TARGET_NOT_SYNCED_YET';
      },
    });
    await open();
    await toPreview();
    expect(host.textContent).toContain("Waiting for this account's first full sync");
    expect(button('Restore')!.disabled).toBe(true);
    expect(calls('restore_apply')).toHaveLength(0);
  });

  it('shows an apply failure as it came and stays on the preview', async () => {
    backend({
      restore_apply: () => {
        throw 'Restore stopped: the safety backup of the notes it would replace failed (disk full).';
      },
    });
    await open();
    await toPreview();
    await click('Restore');
    expect(host.textContent).toContain('Restore stopped: the safety backup of the notes it would replace failed (disk full).');
    expect(button('Restore')!.disabled).toBe(false);
  });

  it('lists what a backup skipped on the version step', async () => {
    backend({}, [version({ skipped: [['u9', 'unreadable body']] })]);
    await open();
    await toVersion();
    expect(host.textContent).toContain('1 note skipped');
    expect(host.textContent).toContain('unreadable body');
  });

  it('lists every skip of a note that was skipped more than once', async () => {
    // One entry per vanished attachment, so a uuid can repeat.
    backend({}, [version({ skipped: [['u9', 'attachment a changed during backup'], ['u9', 'attachment b changed during backup']] })]);
    await open();
    await toVersion();
    // Counted by note, listed by entry.
    expect(host.textContent).toContain('1 note skipped');
    expect(host.textContent).toContain('attachment a changed during backup');
    expect(host.textContent).toContain('attachment b changed during backup');
  });

  it('shows how many notes a version holds only partially', async () => {
    // Spec §11: the count is visible before the owner picks the version.
    const plain = { ...version({ notes: [note('n1', 'Plan', 'Notes')] }), stamp: '2026-10-23T09-15-00Z' };
    backend({}, [version(), plain]);
    await open();
    await toVersion();
    const rows = Array.from(host.querySelectorAll('.version')).map((r) => r.textContent!.replace(/\s+/g, ' '));
    expect(rows).toHaveLength(2);
    expect(rows[0]).toContain('3 notes · 1 partial ·');
    expect(rows[1]).not.toContain('partial');
  });

  it('deletes a version only after a confirm, then reloads the list', async () => {
    let list = [version()];
    backend({
      backup_list_versions: () => list,
      backup_delete_version: () => {
        list = [];
      },
    });
    await open();
    await toVersion();

    await click('Delete');
    expect(calls('backup_delete_version')).toHaveLength(0);
    await click('Delete backup');

    expect(calls('backup_delete_version')).toEqual([
      ['backup_delete_version', { destinationId: 'd1', slug: 'a-x-com', stamp: STAMP }],
    ]);
    expect(host.textContent).toContain('No backups');
  });

  it('shows a failure to read the folder', async () => {
    backend({
      backup_list_slugs: () => {
        throw 'cannot read the backup folder';
      },
    });
    await open();
    expect(host.textContent).toContain('cannot read the backup folder');
  });

  it('renders nothing while closed and loads nothing', async () => {
    backend();
    await open({ open: false });
    expect(host.querySelector('dialog')).toBeNull();
    expect(invoke).not.toHaveBeenCalled();
  });
});
