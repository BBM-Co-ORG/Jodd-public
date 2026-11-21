// @vitest-environment jsdom
//
// Review of Task 4.5 fix round 1: App's tag watcher is keyed on the account
// too (a pick of the same tags in another account must paint that account),
// but an account change ALONE is not a navigation. Under the all-accounts tag
// scope, tapping another account's note moves `currentAccount` to it
// (NoteList.followNoteContext) after the phone already showed the note pane;
// the watcher must not push the list back on top of it.
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
const A_NOTE = mk('a-study', A.id, '2026-10-01T00:00:00Z');
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
      return args?.accountId === A.id ? [A_NOTE] : [];
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

describe('App on the phone: an account change alone does not push the list', () => {
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

  it('all-accounts tag scope: tapping another account\'s note stays on the note pane', async () => {
    history.replaceState(null, '', location.href);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await vi.waitFor(() => expect(get(currentAccount)).toBe(A.id));
    await settle();

    // The Tags pane's pick, then the list (what PhoneTagsPane does).
    selectOnlyTags(A.id, ['study'], 'AND');
    navigateToPane('list', '#study');
    await settle();
    tagScope.set('all');
    await vi.waitFor(() => {
      const rows = Array.from(host!.querySelectorAll('.note-item'));
      expect(rows.some((r) => r.textContent?.includes('B-STUDY'))).toBe(true);
    });
    expect(history.state).toMatchObject({ pane: 'list', depth: 1 });

    const row = Array.from(host!.querySelectorAll('.note-item')).find((r) => r.textContent?.includes('B-STUDY'))!;
    (row.querySelector('.note-btn') as HTMLElement).click();
    flushSync();
    await settle();

    expect(get(currentAccount)).toBe(B.id); // followNoteContext moved it
    expect(get(selectedNote)?.uuid).toBe('b-study');
    expect(get(activePane)).toBe('note');
    expect(history.state).toMatchObject({ pane: 'note', depth: 2 }); // [folders, list, note]
  });

  it('account scope: an account change alone repaints for the new account but pushes nothing', async () => {
    history.replaceState(null, '', location.href);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(App, { target: host }) as Record<string, unknown>;
    await vi.waitFor(() => expect(get(currentAccount)).toBe(A.id));
    await settle();
    selectOnlyTags(A.id, ['study'], 'AND');
    navigateToPane('list', '#study');
    await settle();
    selectedNote.set(A_NOTE);
    await settle();
    expect(history.state).toMatchObject({ pane: 'note', depth: 2 });

    invoke.mockClear();
    currentAccount.set(B.id); // e.g. a dismissed account's fallback
    await settle();
    expect(invoke).toHaveBeenCalledWith('list_cached_notes_with_tags', { accountId: B.id, tags: ['study'] });
    expect(get(activePane)).toBe('note');
    expect(history.state).toMatchObject({ pane: 'note', depth: 2 });
  });
});
