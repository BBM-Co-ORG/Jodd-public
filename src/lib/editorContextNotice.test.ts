// The editor's context notice: shown when the open note is not in the view
// the list shows, worded without raw paths. Galaxy Tab S7 live pass of
// v0.35.2 (2026-10-21) read "Browsing Notes/_test_. This editor remains on
// Notes/Inbox · kaiwan@bbmedia.co.th. Edits stay with this note." while the
// view was the tag #ai-agents — a stale folder, and raw paths.
import { describe, it, expect } from 'vitest';
import { contextNotice, type ContextNoticeInput } from './editorContextNotice';
import type { Note } from './types';

const ACCT = 'gmail:kaiwan@bbmedia.co.th';
const note = { uuid: 'n1', id: 'm1', account_id: ACCT, title: 'T', body_html: '', date: '', label: 'Notes/Inbox' } as Note;

function input(over: Partial<ContextNoticeInput> = {}): ContextNoticeInput {
  return {
    note,
    folder: 'Notes/_test_',
    smart: null,
    search: '',
    tags: new Set(),
    tagMatchMode: 'AND',
    tagScope: 'account',
    noteTags: new Map(),
    currentAccount: ACCT,
    viewTitle: '_test_',
    noteAccountName: 'kaiwan@bbmedia.co.th',
    ...over,
  };
}

const tagged = (tags: string[]) => new Map([[ACCT, new Map([['n1', tags]])]]);

describe('contextNotice', () => {
  it('a note from another folder: both named by their leaf, no raw path', () => {
    expect(contextNotice(input())).toBe("This note is in Inbox. You're viewing _test_.");
  });

  it('names the tag view, not the folder underneath it', () => {
    const text = contextNotice(input({ tags: new Set(['ai-agents']), viewTitle: '#ai-agents' }));
    expect(text).toBe("This note is in Inbox. You're viewing #ai-agents.");
    expect(text).not.toContain('_test_');
  });

  it('no notice when the open note carries the tag the view shows', () => {
    expect(contextNotice(input({ tags: new Set(['ai-agents']), viewTitle: '#ai-agents', noteTags: tagged(['ai-agents']) }))).toBeNull();
  });

  it('AND needs every tag; OR needs one', () => {
    const tags = new Set(['ai-agents', 'llm']);
    expect(contextNotice(input({ tags, noteTags: tagged(['ai-agents']), viewTitle: '#ai-agents #llm' }))).not.toBeNull();
    expect(contextNotice(input({ tags, tagMatchMode: 'OR', noteTags: tagged(['ai-agents']), viewTitle: '#ai-agents #llm' }))).toBeNull();
  });

  it('no notice when the open note is in the folder shown', () => {
    expect(contextNotice(input({ folder: 'Notes/Inbox', viewTitle: 'Inbox' }))).toBeNull();
  });

  it('no notice in the All view for a note of the current account', () => {
    expect(contextNotice(input({ folder: '__ALL__', viewTitle: 'All kaiwan' }))).toBeNull();
  });

  it('a note of another account names that account', () => {
    expect(contextNotice(input({ folder: 'Notes/Inbox', currentAccount: 'icloud:x@y.com', viewTitle: 'Inbox' })))
      .toBe("This note is in Inbox (kaiwan@bbmedia.co.th). You're viewing Inbox.");
  });

  it('two folders with the same leaf are told apart by their path below the root', () => {
    expect(contextNotice(input({ folder: 'Notes/Work/Inbox', viewTitle: 'Inbox' })))
      .toBe("This note is in Inbox. You're viewing Work › Inbox.");
  });

  it('a workflow folder is named without its markers', () => {
    expect(contextNotice(input({ note: { ...note, label: 'Notes/__Extracts__' } })))
      .toBe("This note is in Extracts. You're viewing _test_.");
  });

  it('no notice in a Smart Folder or a search, as before', () => {
    expect(contextNotice(input({ smart: { kind: 'orphaned' } }))).toBeNull();
    expect(contextNotice(input({ search: 'x' }))).toBeNull();
  });
});
