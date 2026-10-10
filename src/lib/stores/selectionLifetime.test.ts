import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import {
  accounts,
  currentAccount,
  selectedFolder,
  selectedNote,
  selectedSmartFolder,
} from './notes';
import type { Account, Note } from '../types';

function acct(id: string, status: Account['status'] = 'active'): Account {
  return {
    id,
    email: id.split(':')[1] ?? id,
    added_at: '',
    backend_kind: id.startsWith('icloud') ? 'icloud' : 'gmail',
    root_dir: null,
    status,
  } as Account;
}

/**
 * An account leaves the active set in two ways and only one is a click: the
 * user deactivates it, or the sync worker finishes draining it and flips it on
 * its own. Either way the selection has to let go, or the editor renders a note
 * nothing can load and the folder header keeps a path that only existed in the
 * account that just went away.
 */
describe('a selection does not outlive its account', () => {
  beforeEach(() => {
    accounts.set([]);
    currentAccount.set(null);
    selectedFolder.set('Notes');
    selectedNote.set(null);
    selectedSmartFolder.set(null);
  });

  // PR #140 review: a Smart Folder is per account, and the current account is
  // not the only one it can belong to. Left behind, it hid every folder row's
  // highlight and kept listing notes from an account that is no longer active.
  it('clears a Smart Folder whose own account is deactivated, even when another account is current', () => {
    accounts.set([acct('gmail:a@b.com'), acct('microsoft:c@d.com')]);
    currentAccount.set('microsoft:c@d.com');
    selectedSmartFolder.set({ account: 'gmail:a@b.com', kind: 'stale' });

    accounts.set([acct('gmail:a@b.com', 'inactive'), acct('microsoft:c@d.com')]);

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(currentAccount)).toBe('microsoft:c@d.com');
  });

  it('keeps a Smart Folder whose account is still active', () => {
    accounts.set([acct('gmail:a@b.com'), acct('microsoft:c@d.com')]);
    currentAccount.set('gmail:a@b.com');
    selectedSmartFolder.set({ account: 'gmail:a@b.com', kind: 'stale' });

    accounts.set([acct('gmail:a@b.com'), acct('microsoft:c@d.com', 'inactive')]);

    expect(get(selectedSmartFolder)).toEqual({ account: 'gmail:a@b.com', kind: 'stale' });
  });

  it('clears the open note and resets the folder when its account is deactivated', () => {
    accounts.set([acct('icloud:a@me.com'), acct('gmail:b@c.com')]);
    currentAccount.set('icloud:a@me.com');
    selectedFolder.set('Notes/FolderforJoddTesting');
    selectedNote.set({ uuid: 'u1', account_id: 'icloud:a@me.com' } as Note);

    accounts.set([acct('icloud:a@me.com', 'inactive'), acct('gmail:b@c.com')]);

    expect(get(selectedNote)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes');
  });

  it('leaves a selection belonging to an account that is still active alone', () => {
    accounts.set([acct('icloud:a@me.com'), acct('gmail:b@c.com')]);
    currentAccount.set('gmail:b@c.com');
    selectedFolder.set('Notes/Personal');
    selectedNote.set({ uuid: 'u2', account_id: 'gmail:b@c.com' } as Note);

    accounts.set([acct('icloud:a@me.com', 'inactive'), acct('gmail:b@c.com')]);

    expect(get(selectedNote)).not.toBeNull();
    expect(get(selectedFolder)).toBe('Notes/Personal');
  });

  /** An empty list means "not loaded yet", not "everything went away". */
  it('does not clear a selection made before the account list has loaded', () => {
    currentAccount.set('gmail:b@c.com');
    selectedFolder.set('Notes/Personal');
    accounts.set([]);
    expect(get(selectedFolder)).toBe('Notes/Personal');
  });
});
