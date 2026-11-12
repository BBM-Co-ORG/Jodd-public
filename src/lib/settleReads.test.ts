import { describe, it, expect } from 'vitest';
import { settleReads, openNoteFolder } from './settleReads';

const ACCT = 'gmail:a@example.com';
const note = (label: string | null, uuid = 'U-1', account_id: string | null = ACCT) => ({ uuid, account_id, label });

describe('settleReads', () => {
  // The 2026-11-12 conflict copy: "All" selected on Android, a Gmail note open.
  // The settle used to read `$selectedFolder` only — a sentinel — so nothing.
  it('with All selected, reads the open note\'s own folder', () => {
    expect(settleReads('__ALL__', ACCT, note('Personal/Creator/Charactor ref'))).toEqual([
      { accountId: ACCT, folder: 'Personal/Creator/Charactor ref' },
    ]);
  });

  it('with All selected and no note open, reads nothing (as before)', () => {
    expect(settleReads('__ALL__', ACCT, null)).toEqual([]);
  });

  it('a real folder is still read, and the open note\'s folder is not read twice', () => {
    expect(settleReads('Notes', ACCT, note('Notes'))).toEqual([{ accountId: ACCT, folder: 'Notes' }]);
  });

  // A tag or smart-folder view leaves the last folder selected; the note open
  // can live anywhere.
  it('a note from another folder is read beside the selected one', () => {
    expect(settleReads('Notes', ACCT, note('Work'))).toEqual([
      { accountId: ACCT, folder: 'Notes' },
      { accountId: ACCT, folder: 'Work' },
    ]);
  });

  it('the note\'s folder is read in the note\'s own account', () => {
    expect(settleReads('__ALL__', ACCT, note('Notes', 'U-2', 'icloud:b@example.com'))).toEqual([
      { accountId: 'icloud:b@example.com', folder: 'Notes' },
    ]);
  });

  it('Recently Deleted reads nothing', () => {
    expect(settleReads('__TRASH__', ACCT, note('Notes'))).toEqual([]);
  });
});

describe('openNoteFolder', () => {
  it('a never-saved note has nothing remote to read', () => {
    expect(openNoteFolder(note('Notes', 'tmp:1'), ACCT)).toBeNull();
  });

  it('a note without a real label is not read', () => {
    expect(openNoteFolder(note(null), ACCT)).toBeNull();
    expect(openNoteFolder(note('__ALL__'), ACCT)).toBeNull();
  });

  it('falls back to the current account when the note names none', () => {
    expect(openNoteFolder(note('Notes', 'U-1', null), ACCT)).toEqual({ accountId: ACCT, folder: 'Notes' });
  });
});
