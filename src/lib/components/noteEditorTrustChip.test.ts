// @vitest-environment jsdom
//
// The trust chip: shown only for an unreviewed note outside the agent
// workspace, read from the store in one reactive expression keyed by the open
// note (gotcha #28), re-rendered when a remote-changed refresh updates the store.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes, unreviewedTrust, unreviewedKey, selectedSmartFolder, smartFolderNotes, type Trust } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';

function noteOf(uuid: string, label: string): Note {
  return {
    uuid, id: `msg-${uuid}`, account_id: ACCOUNT_ID, title: `Title ${uuid}`,
    body_html: `<div>Title ${uuid}</div><div>body</div>`, date: '2026-09-15T00:00:00Z', label,
  } as Note;
}

const A = noteOf('uuid-a', 'Notes/Inbox');

const T: Trust = { tier: 'unreviewed', by: 'claude-code/2.1.0', at: '2026-10-07T01:00:00Z', event_id: 7, unreviewed_by: 'claude-code/2.1.0', unreviewed_at: '2026-10-07T01:00:00Z', created_by: 'claude-code/2.1.0', unrecorded_change: false, note_local_version: 3 };
const put = (t: Trust, n: Note = A) => unreviewedTrust.set({ [unreviewedKey(n.account_id!, n.uuid)]: { account: n.account_id!, trust: t } });

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

const chip = (host: HTMLElement) => host.querySelector('[data-testid="trust-chip"]');

describe('trust chip', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_cached_note') return Promise.resolve(A); // SQLite holds what is on screen
      if (cmd === 'verify_note') return Promise.resolve({ kind: 'verified', trust: { ...T, tier: 'human_reviewed' } });
      return Promise.resolve([]);
    });
    unreviewedTrust.set({});
    notes.set([A]);
    selectedNote.set(A);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await settle();
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    unreviewedTrust.set({});
    selectedSmartFolder.set(null);
    smartFolderNotes.set([]);
    vi.clearAllMocks();
  });

  // R14: the chip names who wrote what is unreviewed, not the latest writer.
  it('names the workflow, not the user who fixed a typo afterwards', async () => {
    put({ ...T, by: 'human:owner', unreviewed_by: 'jodd-extract/claude-opus-5-5' });
    await settle();
    expect(chip(host)?.textContent).toContain('jodd-extract');
    expect(chip(host)?.textContent).not.toContain('human');
  });

  it('a remote change then a local edit still says changed elsewhere', async () => {
    put({ ...T, by: 'human:owner', unreviewed_by: null });
    await settle();
    expect(chip(host)?.textContent).toContain('changed elsewhere');
  });

  // I6: in the Unreviewed view the row leaves with the chip, before the IPC.
  function inUnreviewedView() {
    const X = noteOf('uuid-x', 'Notes');
    const Y = noteOf('uuid-y', 'Notes');
    selectedSmartFolder.set({ account: ACCOUNT_ID, kind: 'unreviewed' });
    smartFolderNotes.set([X, A, Y]);
    let settleVerify!: (v: unknown) => void;
    let failVerify!: (e: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'verify_note') return new Promise((res, rej) => { settleVerify = res; failVerify = rej; });
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_cached_note') return Promise.resolve(A);
      return Promise.resolve([]);
    });
    return { resolve: (v: unknown) => settleVerify(v), reject: (e: unknown) => failVerify(e) };
  }
  const uuids = () => get(smartFolderNotes).map((n) => n.uuid);

  it('Mark reviewed removes the Unreviewed row optimistically', async () => {
    const verify = inUnreviewedView();
    put(T);
    await settle();
    (chip(host)!.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    expect(uuids()).toEqual(['uuid-x', 'uuid-y']); // before verify_note settles
    verify.resolve({ kind: 'verified', trust: { ...T, tier: 'human_reviewed' } });
    await settle();
    expect(uuids()).toEqual(['uuid-x', 'uuid-y']);
  });

  it('a stale Mark reviewed puts the row back where it was', async () => {
    const verify = inUnreviewedView();
    put(T);
    await settle();
    (chip(host)!.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    expect(uuids()).toEqual(['uuid-x', 'uuid-y']);
    verify.resolve({ kind: 'stale', trust: { ...T, event_id: 9, unreviewed_by: 'other/2' } });
    await settle();
    expect(uuids()).toEqual(['uuid-x', 'uuid-a', 'uuid-y']);
    expect(chip(host)?.textContent).toContain('other');
  });

  it('a failed Mark reviewed puts the row back where it was', async () => {
    const verify = inUnreviewedView();
    put(T);
    await settle();
    (chip(host)!.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    verify.reject('refused');
    await settle();
    expect(uuids()).toEqual(['uuid-x', 'uuid-a', 'uuid-y']);
  });

  it('shows the chip only for an unreviewed note outside the agent workspace', async () => {
    put(T);
    await settle();
    expect(chip(host)?.textContent).toContain('claude-code');
    selectedNote.set({ ...A, label: 'Notes/__Agent__/Projects' });
    await settle();
    expect(chip(host)).toBeNull();
  });

  it('re-renders when the store changes (a remote-changed refresh)', async () => {
    await settle();
    expect(chip(host)).toBeNull();
    put({ ...T, by: null, unreviewed_by: null });
    await settle();
    expect(chip(host)?.textContent).toContain('changed elsewhere');
  });

  it('says so when the change has no record', async () => {
    put({ ...T, unrecorded_change: true });
    await settle();
    expect(chip(host)?.textContent).toContain('changed without a record');
  });

  it('hides the chip in the same reactive pass that opens another note', async () => {
    const B = noteOf('uuid-b', 'Notes');
    put(T);
    flushSync();
    expect(chip(host)).not.toBeNull();
    selectedNote.set(B);
    flushSync(); // no await: the gap is what gotcha #28 is about
    expect(chip(host)).toBeNull();
  });

  it('Mark reviewed removes the chip and calls verify_note with what was shown', async () => {
    put(T);
    await settle();
    (chip(host)!.querySelector('button') as HTMLButtonElement).click();
    flushSync();
    expect(chip(host)).toBeNull(); // optimistic, before the IPC settles
    await settle();
    expect(invoke).toHaveBeenCalledWith('verify_note', { accountId: ACCOUNT_ID, uuid: A.uuid, seenEventId: 7, seenLocalVersion: 3 });
  });

  it('a failed Mark reviewed brings the chip back with an error', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'verify_note') return Promise.reject('refused');
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_cached_note') return Promise.resolve(A);
      return Promise.resolve([]);
    });
    put(T);
    await settle();
    (chip(host)!.querySelector('button') as HTMLButtonElement).click();
    await settle();
    expect(chip(host)).not.toBeNull();
    expect(chip(host)?.textContent).toContain('Could not mark reviewed');
  });
});
