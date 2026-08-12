import { describe, it, expect } from 'vitest';
import { isActive, isDismissed, partitionAccounts } from './accountStatus';
import type { Account } from '../types';

const acct = (id: string, status?: string): Account =>
  ({ id, email: id, added_at: '2026-01-01', status }) as Account;

describe('account status', () => {
  /// An accounts.json written before this feature has no `status` at all.
  /// Treating absence as anything but active would hide every account the
  /// first time an old config is loaded.
  it('treats a missing status as active', () => {
    expect(isActive(acct('a@x'))).toBe(true);
    expect(isDismissed(acct('a@x'))).toBe(false);
  });

  it('counts draining as dismissed, not active', () => {
    expect(isActive(acct('a@x', 'draining'))).toBe(false);
    expect(isDismissed(acct('a@x', 'draining'))).toBe(true);
  });

  it('counts inactive as dismissed', () => {
    expect(isActive(acct('a@x', 'inactive'))).toBe(false);
    expect(isDismissed(acct('a@x', 'inactive'))).toBe(true);
  });

  it('partitions a list without losing anyone', () => {
    const list = [acct('a@x'), acct('b@x', 'draining'), acct('c@x', 'inactive')];
    const { active, dismissed } = partitionAccounts(list);
    expect(active.map((a) => a.id)).toEqual(['a@x']);
    expect(dismissed.map((a) => a.id)).toEqual(['b@x', 'c@x']);
    expect(active.length + dismissed.length).toBe(list.length);
  });
});
