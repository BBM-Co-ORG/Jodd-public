import { describe, it, expect, vi } from 'vitest';
import { createAuthLossHandler, isAuthLostError, authLostMessage, isSignedOut, reconnectKind } from './authLoss';
import type { Account } from '../types';

const gmail = { id: 'gmail:a@x.com', email: 'a@x.com', backend_kind: 'gmail' } as Account;
const outlook = { id: 'microsoft:b@y.com', email: 'b@y.com', backend_kind: 'microsoft' } as Account;

function setup(accounts: Account[] = [gmail]) {
  const refreshAccounts = vi.fn(async () => {});
  const setError = vi.fn();
  const handle = createAuthLossHandler({ refreshAccounts, getAccounts: () => accounts, setError });
  return { handle, refreshAccounts, setError };
}

describe('isAuthLostError', () => {
  it('matches the backend string in any shape the IPC hands back', () => {
    expect(isAuthLostError('no refresh token in keychain for gmail:a@x.com')).toBe(true);
    expect(isAuthLostError(new Error('No refresh token in keychain for gmail:a@x.com'))).toBe(true);
    expect(isAuthLostError('refresh failed: 400 — invalid_grant')).toBe(false);
  });
});

describe('auth loss keeps the account and its notes', () => {
  it('refreshes accounts so the signed-out row appears', async () => {
    const { handle, refreshAccounts } = setup();
    // The handler is given no way to remove an account: what it can reach is
    // what it can do. lib.rs's signed_out_tests pins that nothing on this
    // path calls remove_account.
    await handle(gmail.id);
    expect(refreshAccounts).toHaveBeenCalledTimes(1);
  });

  it('says the notes are safe and how to reconnect, by email and provider', async () => {
    const { handle, setError } = setup();
    await handle(gmail.id);
    const msg = setError.mock.calls[0][0] as string;
    expect(msg).toContain('a@x.com');
    expect(msg).toContain('Gmail');
    expect(msg).toMatch(/notes and unsent changes are safe on this device/);
    expect(msg).toContain('Add Gmail account');
    // The raw account id is an internal key, not a name the user knows.
    expect(msg).not.toContain('gmail:a@x.com');
  });

  it('names Microsoft accounts by their own provider', () => {
    expect(authLostMessage(outlook, outlook.id)).toContain('Add Microsoft account');
  });

  it('announces each account once per session, however many reads fail', async () => {
    const { handle, setError, refreshAccounts } = setup();
    await handle(gmail.id);
    await handle(gmail.id);
    await handle(gmail.id);
    expect(setError).toHaveBeenCalledTimes(1);
    // Still refreshes: the row must stay current even when the bar is quiet.
    expect(refreshAccounts).toHaveBeenCalledTimes(3);
  });

  it('falls back to a generic sentence for an account the list no longer has', () => {
    const msg = authLostMessage(undefined, 'gmail:gone@x.com');
    expect(msg).toMatch(/safe on this device/);
    expect(msg).not.toContain('gmail:gone@x.com');
  });
});

// The sidebar's "Sign in again" (spec 2026-11-10 §3): offered only for a
// signed-out account whose backend has a sign-in Jodd can restart. iCloud's
// other blocked reason (Advanced Data Protection) is not a sign-out.
describe('reconnectKind', () => {
  const base = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z' } as Account;
  const out = 'Signed out. Sign in again to sync.';

  it('names the sign-in for a signed-out Gmail or Microsoft account', () => {
    expect(reconnectKind({ ...base, backend_kind: 'gmail', blocked_reason: out })).toBe('gmail');
    // accounts.json written before backend_kind existed is Gmail.
    expect(reconnectKind({ ...base, backend_kind: undefined, blocked_reason: out })).toBe('gmail');
    expect(reconnectKind({ ...base, backend_kind: 'microsoft', blocked_reason: out })).toBe('microsoft');
  });

  it('offers nothing for any other blocked reason or backend', () => {
    expect(reconnectKind({ ...base, backend_kind: 'gmail' })).toBeNull();
    expect(reconnectKind({ ...base, backend_kind: 'icloud', blocked_reason: 'Advanced Data Protection is on.' })).toBeNull();
    expect(isSignedOut({ ...base, blocked_reason: 'Advanced Data Protection is on.' })).toBe(false);
    expect(reconnectKind({ ...base, backend_kind: 'ssh', blocked_reason: out })).toBeNull();
  });
});
