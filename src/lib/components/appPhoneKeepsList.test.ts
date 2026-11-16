// @vitest-environment jsdom
//
// The phone keeps the note list mounted while a note is open (2026-11-09
// workspace review: "phone panes remount on Back"). Opening a note used to
// destroy the list, so Back rebuilt every row, lost the scroll position and
// reset the tag scope to "This account" (NoteList's onDestroy). Desktop
// and tablet keep the list beside the editor all along; the phone now keeps
// it too, hidden, and frozen so typing in the note does not re-derive it.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get, writable } from 'svelte/store';
import type { Note } from '../types';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(true),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('phone'),
}));

const A = { id: 'gmail:a@example.com', email: 'a@example.com', backend_kind: 'gmail', status: 'active', added_at: '' };
const B = { id: 'gmail:b@example.com', email: 'b@example.com', backend_kind: 'gmail', status: 'active', added_at: '' };
const mk = (uuid: string, account: string, date: string): Note => ({
  uuid, id: `id-${uuid}`, account_id: account, title: uuid.toUpperCase(), label: 'Notes', date,
  body_html: `<html><head></head><body><div>${uuid.toUpperCase()}</div></body></html>`,
  x_mail_created_date: null, local_version: 1,
} as Note);
const NOTES = [1, 2, 3, 4, 5].map((k) => mk(`n${k}`, A.id, `2026-10-0${k}T00:00:00Z`));
const A_NOTE = NOTES[0];
const B_NOTE = mk('b-study', B.id, '2026-10-05T00:00:00Z');

const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>): Promise<unknown> => {
  switch (cmd) {
    case 'is_authenticated': return true;
    case 'platform_name': return 'android';
    case 'list_accounts': return [A, B];
    case 'list_folders': return ['Notes'];
    case 'list_folder_kinds': return [];
    case 'backend_capabilities':
      return { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0 };
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'list_note_tags':
      return args?.accountId === A.id ? [{ uuid: A_NOTE.uuid, tag: 'study' }] : [{ uuid: B_NOTE.uuid, tag: 'study' }];
    case 'list_cached_notes_with_tags':
      // null account = the all-accounts scope's cross-account read.
      return args?.accountId == null ? [A_NOTE, B_NOTE] : args.accountId === A.id ? [A_NOTE] : [B_NOTE];
    case 'list_cached_notes':
    case 'list_notes':
    case 'list_notes_in_folder':
      return args?.accountId === A.id ? NOTES : [];
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ onFocusChanged: async () => () => {}, setTitle: async () => {}, label: 'main' }),
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: async () => '0.0.0' }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));

import App from '../../App.svelte';
import { selectedNote, notes, selectedTags, currentAccount, tagScope, accounts } from '../stores/notes';
import { activePane, navigateToPane } from '../stores/phoneNav';
import { selectOnlyTags } from '../controllers/viewSelection';

async function settle(rounds = 20) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}


describe('App on the phone: the list survives an open note', () => {
  let app: Record<string, unknown> | null = null;
  let host: HTMLElement | null = null;
  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
    selectedNote.set(null);
    selectedTags.set(new Set());
    tagScope.set('account');
    notes.set([]);
    accounts.set([]);
    currentAccount.set(null);
  });

  async function openList() {
    history.replaceState(null, '', location.href);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await vi.waitFor(() => expect(get(currentAccount)).toBe(A.id));
    navigateToPane('list', 'Notes');
    await settle();
    await vi.waitFor(() => expect(host!.querySelectorAll('.note-item').length).toBe(NOTES.length));
    return host!.querySelector<HTMLElement>('.note-list ul')!;
  }

  it('Back from a note returns to the same list, scrolled where it was', async () => {
    const list = await openList();
    list.scrollTop = 120;
    tagScope.set('all');
    await settle();

    (host!.querySelector('.note-btn') as HTMLElement).click();
    await settle();
    expect(get(activePane)).toBe('note');
    expect(host!.contains(list), 'the list was destroyed when the note opened').toBe(true);
    expect(list.closest('[hidden]'), 'the list is still showing over the note').not.toBeNull();

    activePane.set('list');
    await settle();
    expect(host!.querySelector('.note-list ul')).toBe(list);
    expect(list.closest('[hidden]')).toBeNull();
    expect(list.scrollTop).toBe(120);
    expect(get(tagScope)).toBe('all');
  });

  it('the hidden list does not follow the notes until it shows again', async () => {
    await openList();
    (host!.querySelector('.note-btn') as HTMLElement).click();
    await settle();
    expect(get(activePane)).toBe('note');

    notes.update((ns) => [...ns, mk('late', A.id, '2026-10-09T00:00:00Z')]);
    await settle();
    expect(host!.querySelectorAll('.note-item').length).toBe(NOTES.length);

    activePane.set('list');
    await settle();
    expect(host!.querySelectorAll('.note-item').length).toBe(NOTES.length + 1);
  });
});
