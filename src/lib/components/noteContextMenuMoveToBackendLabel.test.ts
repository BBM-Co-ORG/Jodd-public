// @vitest-environment jsdom
//
// Task 6 fix (post-review): the cross-account "Move to" submenu named each
// candidate account with only accountDisplay() — the bare address — even
// though Account.id is now `{backend}:{email}` and two accounts CAN share an
// address on different backends (gmail:a@b.com / microsoft:a@b.com). That
// made the picker exactly the surface this task exists to fix, and it was
// missed in the original pass. Same mount/assert shape as Sidebar.test.ts:
// mount the real component, assert on rendered text — not a check that
// backendLabel() returns the right string in isolation.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { accounts } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';

// Same email, two different backends — the state Account.id qualification
// makes reachable.
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

const SOURCE_NOTE: Note = {
  uuid: 'uuid-under-test',
  id: 'msg-1',
  account_id: GMAIL_ACCOUNT.id,
  title: 'A note to move',
  body_html: '<p>body</p>',
  date: '2026-08-14T00:00:00Z',
  label: 'Notes',
} as Note;

describe('NoteContextMenu → Move-to submenu names the backend per account', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'list_folders') return Promise.resolve(['Notes']);
      return Promise.resolve([]);
    });
    accounts.set([GMAIL_ACCOUNT, MICROSOFT_ACCOUNT]);
  });

  it('shows Gmail and Outlook beside the two same-email accounts in the picker', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(ContextMenuHost, { target, props: { note: SOURCE_NOTE } });
    flushSync();
    await tick();
    await Promise.resolve(); // let the onMount list_folders fetch settle
    await tick();
    flushSync();

    // Two account rows render in the "Move to" submenu (always in the DOM;
    // only visibility is CSS-hover-gated) — both must be distinguishable by
    // backend since the address alone ("a@b.com") is identical on both.
    expect(target.textContent).toMatch(/Gmail/);
    expect(target.textContent).toMatch(/Outlook/);

    unmount(host);
  });
});
