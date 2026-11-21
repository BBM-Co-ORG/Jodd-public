// @vitest-environment jsdom
//
// Move to and Copy to list every account (spec 2026-11-13-cross-account-move-
// copy §5.1). An account that cannot take notes is listed, disabled, and says
// why in text; a pick reports the folder and the account it is in.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

vi.mock('@tauri-apps/api/core', () => ({ invoke: () => Promise.resolve([]) }));

import MoveToTargets from './MoveToTargets.svelte';
import { accounts, capabilitiesByAccount, currentAccount, notes, setAccountCapabilities } from '../stores/notes';
import type { Note } from '../types';

const A = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-09-15T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const B = { id: 'microsoft:c@live.com', email: 'c@live.com', added_at: '2026-09-15T00:00:00Z', backend_kind: 'microsoft', status: 'active' };
const C = { id: 'icloud:d@icloud.com', email: 'd@icloud.com', added_at: '2026-09-15T00:00:00Z', backend_kind: 'icloud', status: 'active' };
const NOTE = { uuid: 'u-1', id: 'm-1', account_id: A.id, title: 'Plan', body_html: '<p>x</p>', date: '2026-10-20T00:00:00Z', label: 'Notes/Work' } as Note;
const FOLDERS = new Map([
  [A.id, ['Notes', 'Notes/Work']],
  [B.id, ['Notes']],
  [C.id, ['Notes', 'Notes/Trips']],
]);
const READ_ONLY = { has_trash: true, writes: { notes: false, relocate: false, folders: false, sidecars: false } };

let host: HTMLElement;
let component: ReturnType<typeof mount> | undefined;
afterEach(() => {
  if (component) unmount(component);
  component = undefined;
  host?.remove();
  capabilitiesByAccount.set({});
});

async function show(props: Record<string, unknown>) {
  accounts.set([A, B, C] as never);
  currentAccount.set(A.id);
  notes.set([]);
  host = document.createElement('div');
  document.body.appendChild(host);
  component = mount(MoveToTargets, { target: host, props: { note: NOTE, variant: 'list', folders: FOLDERS, ...props } as never });
  flushSync();
  await tick();
}

const folderButtons = () => Array.from(host.querySelectorAll<HTMLButtonElement>('button.item.folder'));

describe('Move to / Copy to: every account', () => {
  it('Move to lists the folders of every account that takes notes, and not the note’s own folder', async () => {
    const picks: [string, string][] = [];
    await show({ onPick: (path: string, acct: string) => picks.push([path, acct]) });
    expect(host.querySelectorAll('.row-note').length).toBe(0);
    const titles = folderButtons().map((b) => [b.title, b.disabled]);
    expect(titles).toContainEqual(['Notes/Work', true]);
    expect(titles).toContainEqual(['Notes/Trips', false]);
    folderButtons().find((b) => b.title === 'Notes/Trips')!.click();
    expect(picks).toEqual([['Notes/Trips', C.id]]);
  });

  it('an account that cannot take notes is disabled and says why', async () => {
    setAccountCapabilities(B.id, READ_ONLY as never);
    await show({ onPick: () => {} });
    const notesRows = host.querySelectorAll('.row-note');
    expect(notesRows.length).toBe(1);
    expect(notesRows[0].textContent).toBe('Jodd cannot write notes to this account.');
    // Its folders are not offered.
    expect(folderButtons().filter((b) => b.title === 'Notes').length).toBe(2);
  });

  it('Copy to may go to the note’s own folder, and names itself', async () => {
    const picks: [string, string][] = [];
    await show({ mode: 'copy', variant: 'cascade', onPick: (path: string, acct: string) => picks.push([path, acct]) });
    expect(host.querySelector('.item.has-submenu .label')!.textContent).toBe('Copy to');
    const own = folderButtons().find((b) => b.title === 'Notes/Work')!;
    expect(own.disabled).toBe(false);
    own.click();
    expect(picks).toEqual([['Notes/Work', A.id]]);
  });

  it('a read-only source account still moves within itself, as before', async () => {
    setAccountCapabilities(A.id, READ_ONLY as never);
    await show({ onPick: () => {} });
    expect(folderButtons().find((b) => b.title === 'Notes' && !b.disabled)).toBeTruthy();
    expect(host.querySelectorAll('.row-note').length).toBe(0);
  });
});
