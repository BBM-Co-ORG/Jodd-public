// The sidebar's account names (spec 2026-11-10 §2): short, because a backend
// tag stands beside them, and widened only when two accounts on the SAME
// backend share a short name.
import { describe, it, expect } from 'vitest';
import { accountNameBesideBackend, accountNameWithBackend, accountShortName } from './listView';
import type { Account } from '../types';

const acct = (id: string, backend_kind?: string): Account => {
  const email = id.slice(id.indexOf(':') + 1);
  return { id, email, added_at: '2026-01-01T00:00:00Z', backend_kind } as Account;
};

describe('accountNameBesideBackend', () => {
  it('keeps the short name when the clash is across backends', () => {
    const icloud = acct('icloud:kai@icloud.com', 'icloud');
    const outlook = acct('microsoft:kai@live.com', 'microsoft');
    const active = [icloud, outlook];
    expect(accountNameBesideBackend(icloud, active)).toBe('kai');
    expect(accountNameWithBackend(outlook, active)).toBe('kai · Outlook');
    // The generic short name widens, which is what truncated to "kai@icl…".
    expect(accountShortName(icloud, active)).toBe('kai@icloud.com');
  });

  it('widens to the address on a same-backend clash', () => {
    const a = acct('gmail:kai@gmail.com', 'gmail');
    const b = acct('gmail:kai@bbmedia.co.th', 'gmail');
    expect(accountNameBesideBackend(a, [a, b])).toBe('kai@gmail.com');
    expect(accountNameWithBackend(b, [a, b])).toBe('kai@bbmedia.co.th · Gmail');
  });

  it('treats an account without backend_kind as Gmail', () => {
    const legacy = acct('gmail:kai@gmail.com');
    const b = acct('gmail:kai@bbmedia.co.th', 'gmail');
    expect(accountNameBesideBackend(legacy, [legacy, b])).toBe('kai@gmail.com');
    expect(accountNameWithBackend(legacy, [legacy])).toBe('kai · Gmail');
  });

  it('names a Local Folder by its folder', () => {
    const vault = acct('localfs:/Users/k/Notes Vault', 'local_fs');
    expect(accountNameBesideBackend(vault, [vault])).toBe('Notes Vault');
  });
});
