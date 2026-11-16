// @vitest-environment jsdom
//
// The sidebar's stored fold choices and its Views/Tags scope (spec 2026-11-12).
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { get } from 'svelte/store';

vi.mock('@tauri-apps/api/core', () => ({ invoke: async () => [] }));

import {
  loadTagsOpen, saveTagsOpen, loadViewsOpen, saveViewsOpen,
  loadCollapsedAccounts, toggleCollapsedAccount,
  TAGS_OPEN_KEY, LEGACY_TAGS_EXPANDED_KEY, ACCOUNTS_COLLAPSED_KEY,
} from './sidebarFolds';
import { scope, scopeAccount, scopeChoice, chooseScopeAccount, chooseAllAccounts, SCOPE_ACCOUNT_KEY } from './stores/sidebarScope';
import { accounts, currentAccount } from './stores/notes';

const acct = (id: string, status = 'active') => ({ id, email: id, added_at: '', backend_kind: 'gmail', status });

beforeEach(() => {
  localStorage.clear();
  scopeChoice.set(null);
});

describe('sidebarFolds', () => {
  it('Views and Tags start closed and remember being opened', () => {
    expect(loadViewsOpen()).toBe(false);
    expect(loadTagsOpen()).toBe(false);
    saveViewsOpen(true);
    saveTagsOpen(true);
    expect(loadViewsOpen()).toBe(true);
    expect(loadTagsOpen()).toBe(true);
  });

  it('an upgrade keeps Tags open if any account had it open under the old per-account key', () => {
    localStorage.setItem(LEGACY_TAGS_EXPANDED_KEY, JSON.stringify(['gmail:a']));
    expect(loadTagsOpen()).toBe(true);
    localStorage.setItem(LEGACY_TAGS_EXPANDED_KEY, '[]');
    expect(loadTagsOpen()).toBe(false);
    // The new key wins once written.
    localStorage.setItem(LEGACY_TAGS_EXPANDED_KEY, JSON.stringify(['gmail:a']));
    localStorage.setItem(TAGS_OPEN_KEY, 'false');
    expect(loadTagsOpen()).toBe(false);
  });

  it('folded accounts round-trip, and corrupt storage reads as none', () => {
    let ids = toggleCollapsedAccount(loadCollapsedAccounts(), 'a');
    expect([...ids]).toEqual(['a']);
    expect([...loadCollapsedAccounts()]).toEqual(['a']);
    ids = toggleCollapsedAccount(ids, 'a');
    expect([...loadCollapsedAccounts()]).toEqual([]);
    localStorage.setItem(ACCOUNTS_COLLAPSED_KEY, '{nope');
    expect([...loadCollapsedAccounts()]).toEqual([]);
  });
});

describe('sidebarScope', () => {
  it('follows the current account until chosen, then stays', () => {
    accounts.set([acct('a'), acct('b')] as never);
    currentAccount.set('a');
    expect(get(scopeAccount)).toBe('a');
    currentAccount.set('b');
    expect(get(scopeAccount)).toBe('b');
    chooseScopeAccount('a');
    expect(localStorage.getItem(SCOPE_ACCOUNT_KEY)).toBe('a');
    currentAccount.set('b');
    expect(get(scopeAccount)).toBe('a');
  });

  it('a chosen account that stops being active falls back to the current one', () => {
    accounts.set([acct('a'), acct('b')] as never);
    currentAccount.set('b');
    chooseScopeAccount('a');
    accounts.set([acct('a', 'inactive'), acct('b')] as never);
    expect(get(scopeAccount)).toBe('b');
  });

  it('All accounts is a scope of its own, stored, with no one account', () => {
    accounts.set([acct('a'), acct('b')] as never);
    currentAccount.set('a');
    chooseAllAccounts();
    expect(get(scope)).toEqual({ kind: 'all' });
    expect(get(scopeAccount)).toBeNull();
    expect(localStorage.getItem(SCOPE_ACCOUNT_KEY)).toBe('*all');
  });

  it('All accounts falls back to the current account below two active accounts', () => {
    accounts.set([acct('a'), acct('b', 'inactive')] as never);
    currentAccount.set('a');
    chooseAllAccounts();
    expect(get(scope)).toEqual({ kind: 'account', accountId: 'a' });
    accounts.set([acct('a'), acct('b')] as never);
    expect(get(scope)).toEqual({ kind: 'all' });
  });

  it('no accounts, no scope', () => {
    accounts.set([] as never);
    currentAccount.set(null);
    expect(get(scopeAccount)).toBeNull();
  });
});
