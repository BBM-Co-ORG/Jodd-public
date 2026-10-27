// @vitest-environment jsdom
//
// The single-note actions, called directly — no menu mounted. The note
// context menu, the editor's ⋯ menu and the phone ⋯ sheet all call these same
// functions, so what each action does to the stores and the backend is
// asserted here once. What stays in the component tests is what only a
// mounted menu can show: which items render, that the confirm dialog appears,
// that the menu closes.
//
// Cases moved from noteContextMenuDeleteConfirm.test.ts (single-note) and
// noteContextMenuSuggestFolder.test.ts, assertions kept.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
  notes, selectedNote, currentAccount, error, folderSuggestions, folderSuggestionKey,
  setAccountCapabilities,
} from '../stores/notes';
import { AI_CONSENT_NEEDED, answerConsent, consentRequest } from '../aiConsent';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import {
  togglePin, moveNoteTo, duplicateNote, refetchNote, linkIntoWiki, suggestFolder,
  deleteNoteWithConfirm,
} from './noteActions';

const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  notes.set([]);
  selectedNote.set(null);
  currentAccount.set(null);
  error.set(null);
  vi.stubGlobal('alert', vi.fn());
});
afterEach(() => vi.unstubAllGlobals());

// ─── Delete (moved from noteContextMenuDeleteConfirm.test.ts) ────────────
describe('deleteNoteWithConfirm → permanent-delete confirm (has_trash: false)', () => {
  const ACCOUNT_ID = 'microsoft:test@example.com';
  const SOURCE_NOTE: Note = {
    uuid: 'uuid-under-test',
    id: 'msg-1',
    account_id: ACCOUNT_ID,
    title: 'A note with no undo',
    body_html: '<p>body</p>',
    date: '2026-08-14T00:00:00Z',
    label: 'Notes',
  } as Note;

  beforeEach(() => setAccountCapabilities(ACCOUNT_ID, { has_trash: false }));

  it('does not call delete_note when the user cancels', async () => {
    const confirm = vi.fn().mockResolvedValue(false);
    const onDecided = vi.fn();
    await deleteNoteWithConfirm(SOURCE_NOTE, confirm, onDecided);

    // The dialog names the note, says the delete is permanent, and says what
    // Jodd itself cannot do about it. It deliberately does NOT claim the note
    // is destroyed: Microsoft's delete really is unrecoverable, iCloud's files
    // it in Apple's own Recently Deleted, and one capability covers both — so
    // the sentence has to be true of each.
    expect(confirm).toHaveBeenCalledTimes(1);
    const [title, message] = confirm.mock.calls[0];
    expect(title).toContain('A note with no undo');
    expect(title).toMatch(/permanent/i);
    expect(message).toMatch(/cannot undo this/i);

    expect(invoke.mock.calls.find((c) => c[0] === 'delete_note')).toBeUndefined();
    // A cancel settles the caller's menu exactly once.
    expect(onDecided).toHaveBeenCalledTimes(1);
  });

  it('calls delete_note when the user confirms', async () => {
    const confirm = vi.fn().mockResolvedValue(true);
    await deleteNoteWithConfirm(SOURCE_NOTE, confirm, () => {});

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_note');
    expect(call, `delete_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({ accountId: ACCOUNT_ID, uuid: 'uuid-under-test' });
  });

  it('lets the caller close only after the optimistic removal began', async () => {
    notes.set([SOURCE_NOTE]);
    const confirm = vi.fn().mockResolvedValue(true);
    let rowsWhenDecided: Note[] | null = null;
    let deleteSentWhenDecided = true;
    await deleteNoteWithConfirm(SOURCE_NOTE, confirm, () => {
      rowsWhenDecided = get(notes);
      deleteSentWhenDecided = invoke.mock.calls.some((c) => c[0] === 'delete_note');
    });
    expect(rowsWhenDecided).toEqual([]);
    // deleteNoteOptimistic sends delete_note synchronously, before its first
    // await — the menu closes after the row is gone, never after the network.
    expect(deleteSentWhenDecided).toBe(true);
  });
});

describe('deleteNoteWithConfirm (has_trash: true, unchanged one-click behavior)', () => {
  const ACCOUNT_ID = 'microsoft:test@example.com';
  const SOURCE_NOTE: Note = {
    uuid: 'uuid-under-test', id: 'msg-1', account_id: ACCOUNT_ID, title: 'A note with no undo',
    body_html: '<p>body</p>', date: '2026-08-14T00:00:00Z', label: 'Notes',
  } as Note;

  beforeEach(() => setAccountCapabilities(ACCOUNT_ID, { has_trash: true }));

  it('calls delete_note directly with no confirm dialog', async () => {
    const confirm = vi.fn().mockResolvedValue(false);
    const onDecided = vi.fn();
    await deleteNoteWithConfirm(SOURCE_NOTE, confirm, onDecided);

    // No confirm asked — the one-click Gmail/LocalFs path (a real Trash
    // exists) must stay exactly as it was.
    expect(confirm).not.toHaveBeenCalled();

    const call = invoke.mock.calls.find((c) => c[0] === 'delete_note');
    expect(call, `delete_note never invoked; calls: ${JSON.stringify(invoke.mock.calls)}`)
      .toBeDefined();
    expect(call![1]).toMatchObject({ accountId: ACCOUNT_ID, uuid: 'uuid-under-test' });
    expect(onDecided).toHaveBeenCalledTimes(1);
  });
});

// ─── Suggest folder (moved from noteContextMenuSuggestFolder.test.ts) ─────
// "Suggest folder" is the EXPLICIT caller of suggest_note_folder, so unlike
// the automatic post-Extract call it must say what happened — including that
// no provider is configured (spec "Suggestion" table).
describe('suggestFolder', () => {
  const NOTE: Note = {
    uuid: 'uuid-under-test', id: 'msg-1', account_id: 'gmail:a@b.com', title: 'A note',
    body_html: '<p>body</p>', date: '2026-09-15T00:00:00Z', label: 'Notes/Inbox',
  } as Note;

  async function runSuggest(answer: () => Promise<unknown>) {
    invoke.mockImplementation((cmd: string) => (cmd === 'suggest_note_folder' ? answer() : Promise.resolve([])));
    // Not awaited: a consent refusal parks the action on the in-app prompt
    // until answerConsent() — the same fire-and-forget the menu click does.
    void suggestFolder(NOTE);
    await flush();
    const call = invoke.mock.calls.find((c) => c[0] === 'suggest_note_folder');
    expect(call?.[1]).toMatchObject({ accountId: 'gmail:a@b.com', uuid: 'uuid-under-test' });
  }

  beforeEach(() => folderSuggestions.set({}));

  it('says when no LLM provider is configured', async () => {
    await runSuggest(() => Promise.reject('provider not configured: no LLM provider configured for this account'));
    expect(globalThis.alert).toHaveBeenCalledWith('No LLM provider is configured for this account');
  });

  it('says when nothing better was found', async () => {
    await runSuggest(() => Promise.resolve({ kind: 'none_fits' }));
    expect(globalThis.alert).toHaveBeenCalledWith('No better folder found');
  });

  it('feeds the editor chip, silently, when the note is open', async () => {
    selectedNote.set(NOTE);
    await runSuggest(() => Promise.resolve({ kind: 'suggested', uuid: 'uuid-under-test', path: 'Notes/Trading', reason: 'r' }));
    expect(get(folderSuggestions)[folderSuggestionKey('gmail:a@b.com', 'uuid-under-test')])
      .toEqual({ path: 'Notes/Trading', reason: 'r' });
    expect(globalThis.alert).not.toHaveBeenCalled();
  });

  it('treats the note as open when the outcome names a rekeyed uuid, and records under both', async () => {
    // Gotcha #16: the editor holds the uuid the menu asked about; the backend
    // rekeyed the row mid-call, so the outcome carries a different one.
    selectedNote.set(NOTE);
    await runSuggest(() => Promise.resolve({ kind: 'suggested', uuid: '<rekeyed@exchange>', path: 'Notes/Trading', reason: null }));
    expect(globalThis.alert).not.toHaveBeenCalled();
    const m = get(folderSuggestions);
    expect(m[folderSuggestionKey('gmail:a@b.com', 'uuid-under-test')]).toEqual({ path: 'Notes/Trading', reason: null });
    expect(m[folderSuggestionKey('gmail:a@b.com', '<rekeyed@exchange>')]).toEqual({ path: 'Notes/Trading', reason: null });
  });

  it('a consent refusal opens the in-app consent prompt instead of alerting', async () => {
    await runSuggest(() => Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`));
    expect(get(consentRequest)?.accountId).toBe('gmail:a@b.com');
    expect(globalThis.alert).not.toHaveBeenCalled();
    answerConsent(false);
  });
});

// ─── Pin, move, duplicate, refetch, link into wiki ───────────────────────
// No component test covered these bodies before the extraction; these pin
// the optimistic → rollback shape each one carried in the menu.
const ACCT = 'gmail:a@b.com';
const NOTE: Note = {
  uuid: 'u1', id: 'm1', account_id: ACCT, title: 'Title', body_html: '<p>b</p>',
  date: '2026-10-20T00:00:00Z', label: 'Notes', pinned: false,
} as Note;

describe('togglePin', () => {
  it('flips the pin optimistically and calls set_pin', async () => {
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    const pending = togglePin(NOTE);
    expect(get(notes)[0].pinned).toBe(true);
    expect(get(selectedNote)?.pinned).toBe(true);
    await pending;
    expect(invoke).toHaveBeenCalledWith('set_pin', { accountId: ACCT, uuid: 'u1', pinned: true });
  });

  it('rolls the pin back and reports when set_pin fails', async () => {
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    invoke.mockRejectedValue('boom');
    await togglePin(NOTE);
    expect(get(notes)[0].pinned).toBe(false);
    expect(get(selectedNote)?.pinned).toBe(false);
    expect(get(error)).toBe('Failed to pin note: boom');
  });
});

describe('moveNoteTo', () => {
  it('does nothing when the target is the current folder', async () => {
    await moveNoteTo(NOTE, 'Notes');
    expect(invoke).not.toHaveBeenCalled();
  });

  it('relabels before its first await and moves through move_notes_batch', async () => {
    notes.set([NOTE]);
    const pending = moveNoteTo(NOTE, 'Notes/Work');
    expect(get(notes)[0].label).toBe('Notes/Work');
    await pending;
    const call = invoke.mock.calls.find((c) => c[0] === 'move_notes_batch');
    expect(call?.[1]).toMatchObject({ accountId: ACCT, uuids: ['u1'], targetLabel: 'Notes/Work' });
    expect(invoke.mock.calls.find((c) => c[0] === 'save_note')).toBeUndefined();
  });
});

describe('duplicateNote', () => {
  it('saves a copy that inherits provenance and selects it', async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === 'save_note' ? Promise.resolve({ id: 'm2', uuid: 'u2' }) : Promise.resolve([]));
    await duplicateNote(NOTE);
    const call = invoke.mock.calls.find((c) => c[0] === 'save_note');
    expect(call?.[1]).toMatchObject({
      accountId: ACCT, title: 'Title copy', existingUuid: null, label: 'Notes', duplicateOf: 'u1',
    });
    expect(get(notes)[0]).toMatchObject({ uuid: 'u2', id: 'm2', title: 'Title copy' });
    expect(get(selectedNote)?.uuid).toBe('u2');
  });

  it('does nothing for an unsaved note', async () => {
    await duplicateNote({ ...NOTE, id: '' });
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe('refetchNote', () => {
  it('replaces the in-memory copy and the open note', async () => {
    const fresh = { ...NOTE, body_html: '<p>fresh</p>' };
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    invoke.mockImplementation((cmd: string) =>
      cmd === 'refetch_note' ? Promise.resolve(fresh) : Promise.resolve([]));
    await refetchNote(NOTE);
    expect(invoke).toHaveBeenCalledWith('refetch_note', { accountId: ACCT, id: 'm1' });
    expect(get(notes)[0].body_html).toBe('<p>fresh</p>');
    expect(get(selectedNote)?.body_html).toBe('<p>fresh</p>');
  });
});

describe('linkIntoWiki', () => {
  it('hands proposed appends to the caller', async () => {
    const appends = [{ uuid: 'u9', title: 'Other', addition_text: 'see [[Title]]' }];
    invoke.mockImplementation((cmd: string) =>
      cmd === 'suggest_wiki_links'
        ? Promise.resolve({ ai_result_id: 'r', auto_links: [], proposed_appends: appends })
        : Promise.resolve([]));
    const onLinkSuggestions = vi.fn();
    await linkIntoWiki(NOTE, onLinkSuggestions);
    expect(onLinkSuggestions).toHaveBeenCalledWith(ACCT, appends);
    expect(invoke.mock.calls.find((c) => c[0] === 'save_note')).toBeUndefined();
  });
});
