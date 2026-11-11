import { describe, it, expect } from 'vitest';
import { accountDotState, accountHeaderMark } from './accountDot';
import type { Account } from './types';
import type { PendingPushes } from './controllers/accountActions';

const acct = (over: Partial<Account> = {}): Account =>
  ({ id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', ...over });
const counts = (over: Partial<PendingPushes> = {}): PendingPushes =>
  ({ notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0, ...over });

describe('accountDotState', () => {
  it('is green only for an active account with nothing queued or refused', () => {
    expect(accountDotState(acct(), counts(), false)).toEqual({ kind: 'synced', glyph: '●', text: 'Synced' });
    // Absent status is active (accounts.json written before the feature).
    expect(accountDotState(acct({ status: undefined }), counts(), false).kind).toBe('synced');
  });

  it('counts every queue as pending, and says how many', () => {
    expect(accountDotState(acct(), counts({ notes: 1 }), false))
      .toEqual({ kind: 'pending', glyph: '●', text: 'Syncing — 1 change waiting' });
    expect(accountDotState(acct(), counts({ deletes: 1, pins: 1, folders: 1 }), false).text)
      .toBe('Syncing — 3 changes waiting');
  });

  it('a refused push outranks queued work (gotcha #14)', () => {
    const d = accountDotState(acct(), counts({ notes: 4, blocked: 2 }), false);
    expect(d.kind).toBe('blocked');
    expect(d.text).toBe("Sync blocked — 2 changes can't be sent");
  });

  it("a whole-account block names the backend's reason", () => {
    const d = accountDotState(acct({ blocked_reason: 'Advanced Data Protection is on.' }), counts(), false);
    expect(d).toEqual({ kind: 'blocked', glyph: '●', text: 'Sync blocked — Advanced Data Protection is on.' });
  });

  it('no account is signed out', () => {
    expect(accountDotState(undefined, undefined, false)).toEqual({ kind: 'signed-out', glyph: '○', text: 'Signed out' });
  });

  it('draining and inactive are hollow, whatever the counts say', () => {
    expect(accountDotState(acct({ status: 'draining' }), counts({ notes: 3 }), false))
      .toEqual({ kind: 'inactive', glyph: '○', text: 'Inactive — finishing sync' });
    expect(accountDotState(acct({ status: 'inactive' }), counts(), false).text).toBe('Inactive');
  });

  it('an unread or failed count is never shown as synced', () => {
    expect(accountDotState(acct(), undefined, false)).toEqual({ kind: 'unknown', glyph: '○', text: 'Checking sync…' });
    expect(accountDotState(acct(), counts(), true)).toEqual({ kind: 'unknown', glyph: '○', text: 'Sync status unavailable' });
  });
});

// The sidebar header shows state only when it needs the user (spec
// 2026-11-10 §3): nothing for synced or not-yet-known, a ring while changes
// wait, the alert dot when it is refused or signed out.
describe('accountHeaderMark', () => {
  it('stays quiet while synced or unknown', () => {
    expect(accountHeaderMark(accountDotState(acct(), counts(), false))).toBeNull();
    expect(accountHeaderMark(accountDotState(acct(), undefined, false))).toBeNull();
    expect(accountHeaderMark(accountDotState(acct(), undefined, true))).toBeNull();
  });

  it('rings while changes wait, alerts when refused or signed out', () => {
    expect(accountHeaderMark(accountDotState(acct(), counts({ notes: 2 }), false))).toBe('waiting');
    expect(accountHeaderMark(accountDotState(acct(), counts({ blocked: 1 }), false))).toBe('alert');
    expect(accountHeaderMark(accountDotState(acct({ blocked_reason: 'Signed out. Sign in again.' }), counts(), false)))
      .toBe('alert');
  });
});
