// Which account the sidebar's Views and Tags show (spec 2026-11-12 §3), or all
// of them (spec 2026-11-13).
//
// One "For ⌄" picker above both names it, on every layout, and only with more
// than one active account. Choosing there changes what Views and Tags list,
// and nothing else: the open list, the open note and the current account stay
// where they are. Picking a view or a tag then opens it, and that pick makes
// its account current, as any pick does (under All accounts, the note that
// opens does: selection.ts).
//
// Until the user chooses, the scope is the current account (the 2026-11-10
// rule, kept as the default by the owner on 2026-11-13), and the picker says
// which. Once chosen it stays — across launches — until the user chooses
// again or it no longer applies: a chosen account that stops being active, or
// All accounts with fewer than two active.
import { derived, writable } from 'svelte/store';
import { activeAccounts, currentAccount } from './notes';

export const SCOPE_ACCOUNT_KEY = 'jodd.sidebar.scopeAccount';
/** The stored value for All accounts. Not an account id: ids are `{backend}:…`. */
export const ALL_ACCOUNTS = '*all';

/** What Views and Tags show. A type, not a sentinel id, so no caller can
 *  compare "all" against an account id by accident (spec 2026-11-13 §3.2). */
export type Scope = { kind: 'account'; accountId: string } | { kind: 'all' };

function load(): string | null {
  try {
    return localStorage.getItem(SCOPE_ACCOUNT_KEY);
  } catch {
    return null;
  }
}

/** The user's choice — an account id or `ALL_ACCOUNTS` — or null before one. */
export const scopeChoice = writable<string | null>(load());

function save(value: string): void {
  scopeChoice.set(value);
  try {
    localStorage.setItem(SCOPE_ACCOUNT_KEY, value);
  } catch {
    // Non-fatal: the choice holds for this session.
  }
}

export function chooseScopeAccount(accountId: string): void {
  save(accountId);
}

export function chooseAllAccounts(): void {
  save(ALL_ACCOUNTS);
}

/**
 * What Views and Tags show: All accounts when chosen and two or more accounts
 * are active; else the chosen account while it is active; else the current
 * account; else the first active account. Null with no account.
 */
export const scope = derived(
  [scopeChoice, activeAccounts, currentAccount],
  ([$choice, $active, $current]): Scope | null => {
    if ($choice === ALL_ACCOUNTS && $active.length > 1) return { kind: 'all' };
    const id = ($active.find((a) => a.id === $choice) ?? $active.find((a) => a.id === $current) ?? $active[0])?.id;
    return id ? { kind: 'account', accountId: id } : null;
  },
);

/** The one account Views and Tags show, or null under All accounts (or none). */
export const scopeAccount = derived(scope, ($s) => ($s?.kind === 'account' ? $s.accountId : null));
