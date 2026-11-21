// @vitest-environment jsdom
//
// A note may still be moved into an agent folder — a deliberate, one-note
// act — but the picker says where it is going (spec 2026-10-28 §6.4): the
// folder's clean name, tagged "Agents". Retired `__Extracts__` is ordinary.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

vi.mock('@tauri-apps/api/core', () => ({ invoke: () => Promise.resolve([]) }));

import MoveToTargets from './MoveToTargets.svelte';
import { accounts, currentAccount, notes } from '../stores/notes';
import type { Note } from '../types';

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const NOTE = { uuid: 'u-1', id: 'm-1', account_id: ACCOUNT.id, title: 'Plan', body_html: '<p>x</p>', date: '2026-10-20T00:00:00Z', label: 'Notes/Work' } as Note;

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;
afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  host?.remove();
});

describe('note Move to: agent folders', () => {
  it('shows the clean name, tagged Agents', async () => {
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    notes.set([]);
    host = document.createElement('div');
    document.body.appendChild(host);
    const folders = new Map([[ACCOUNT.id, ['Notes', 'Notes/Work', 'Notes/__Claude__', 'Notes/__Extracts__']]]);
    component = mount(MoveToTargets, { target: host, props: { note: NOTE, variant: 'list', folders, onPick: () => {} } });
    flushSync();
    await tick();
    const rows = Array.from(host.querySelectorAll('button.item.folder')).map((b) => ({
      label: b.querySelector('.label')?.textContent?.trim(),
      tag: b.querySelector('.agent-tag')?.textContent?.trim() ?? '',
    }));
    expect(rows).toContainEqual({ label: 'Claude', tag: 'Agents' });
    expect(rows).toContainEqual({ label: 'Extracts', tag: '' });
    expect(rows).toContainEqual({ label: 'Work', tag: '' });
    expect(rows.map((r) => r.label)).not.toContain('__Claude__');
  });
});
