// @vitest-environment jsdom
//
// Task 6 fix (round 2): the editor breadcrumb named the open note's account
// with accountDisplay() alone — no backend label — even though Account.id is
// now `{backend}:{email}` and two accounts CAN share an address on different
// backends (gmail:a@b.com / microsoft:a@b.com). This is the same defect
// class as the Move-to picker fixed in noteContextMenuMoveToBackendLabel.test.ts,
// just on the breadcrumb surface instead. Same mount/assert shape: mount the
// real component, assert on rendered text.
//
// Unlike NoteContextMenu, NoteEditor takes no props: it reads $selectedNote
// directly, so the fixture is just the stores + the component (same shape as
// noteEditorDeleteConfirm.test.ts).
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { accounts, selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

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
  title: 'A note in a same-email account',
  body_html: '<p>body</p>',
  date: '2026-08-14T00:00:00Z',
  label: 'Notes',
} as Note;

// NoteEditor fetches more than one command on mount (note_connections,
// note_citations, get_note_attachments) — a blanket resolved([]) breaks
// note_connections, whose caller destructures { outgoing, backlinks } and
// throws on an array. Same stub as noteEditorDeleteConfirm.test.ts.
function stubInvoke() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
}

describe('NoteEditor breadcrumb names the backend beside the account', () => {
  beforeEach(() => {
    invoke.mockReset();
    stubInvoke();
    accounts.set([GMAIL_ACCOUNT, MICROSOFT_ACCOUNT]);
    notes.set([SOURCE_NOTE]);
    selectedNote.set(SOURCE_NOTE);
  });

  it('shows Gmail beside the address for a note in the gmail: account', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();
    await Promise.resolve();
    flushSync();

    // The breadcrumb's account text alone ("a@b.com") is identical to what
    // a microsoft:a@b.com note would show — the backend label is what makes
    // this note's account identifiable.
    expect(target.textContent).toMatch(/Gmail/);

    unmount(host);
  });

  it('shows Outlook beside the address for a note in the microsoft: account with the same email', async () => {
    selectedNote.set({ ...SOURCE_NOTE, account_id: MICROSOFT_ACCOUNT.id });

    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await tick();
    await Promise.resolve();
    flushSync();

    expect(target.textContent).toMatch(/Outlook/);

    unmount(host);
  });
});
