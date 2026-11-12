// What the app does when an account's refresh token cannot be read.
//
// It used to remove the account. `remove_account` wipes the account's local
// rows, unsent edits included, and the trigger is not only a token that is
// really gone: on macOS a Keychain prompt that was denied, dismissed or left
// unanswered (every rebuilt binary asks again, gotcha #15) reads the same.
// A sign-in problem must never cost a note (PRODUCT.md principle 1).
//
// Now the account stays, with its notes readable and editable here. The
// backend marks it signed out (`blocked_reason`, shown as a row under the
// account) and stops reading the Keychain for it until the next sign-in, so
// no prompt repeats on every sync tick. Signing in to the same account again
// goes through `complete_oauth`, which keeps the existing account and its
// rows and clears the mark.
import type { Account } from '../types';
import type { AddAccountKind } from './accountActions';

const AUTH_LOST = /no refresh token in keychain/i;

/**
 * The start of `SIGNED_OUT_REASON` (lib.rs), the `blocked_reason` the backend
 * sets when it cannot read an account's sign-in. The other producer of a
 * `blocked_reason` (iCloud's Advanced Data Protection) never starts this way.
 */
export const SIGNED_OUT_PREFIX = 'Signed out.';

export function isSignedOut(account: Account): boolean {
  return !!account.blocked_reason?.startsWith(SIGNED_OUT_PREFIX);
}

/**
 * The add-account flow that signs a signed-out account back in, or null when
 * there is none to offer. Signing in to the same Gmail or Microsoft account
 * keeps its rows and clears the mark (`complete_oauth`).
 */
export function reconnectKind(account: Account): AddAccountKind | null {
  if (!isSignedOut(account)) return null;
  if (account.backend_kind === 'gmail' || account.backend_kind === undefined) return 'gmail';
  if (account.backend_kind === 'microsoft') return 'microsoft';
  return null;
}

export function isAuthLostError(e: unknown): boolean {
  const s = typeof e === 'string' ? e : e instanceof Error ? e.message : String(e);
  return AUTH_LOST.test(s);
}

const PROVIDER: Record<string, { name: string; add: string }> = {
  gmail: { name: 'Gmail', add: 'Add Gmail account' },
  microsoft: { name: 'Microsoft', add: 'Add Microsoft account' },
};

export function authLostMessage(account: Account | undefined, _accountId: string): string {
  const p = account?.backend_kind ? PROVIDER[account.backend_kind] : undefined;
  const who = account ? (p ? `${account.email} (${p.name})` : account.email) : 'an account';
  const how = p
    ? `To reconnect, sign in again with ${p.add} in Settings › Accounts.`
    : 'To reconnect, sign in to it again from Settings › Accounts.';
  return `Jodd lost its sign-in for ${who}. Its notes and unsent changes are safe on this device. ${how}`;
}

export interface AuthLossDeps {
  /** Re-read `list_accounts`, so the account's signed-out row appears. */
  refreshAccounts: () => Promise<void>;
  getAccounts: () => Account[];
  setError: (message: string) => void;
}

/**
 * Deliberately given no way to remove an account or drop notes: what the
 * handler can reach is what it can do.
 */
export function createAuthLossHandler(deps: AuthLossDeps): (accountId: string) => Promise<void> {
  // Once per account per session: every failed read reports the same loss,
  // and a bar that reappears after Dismiss on each refresh is noise. The row
  // under the account stays as the persistent signal.
  const announced = new Set<string>();
  return async (accountId: string) => {
    console.warn(`[jodd] auth lost for ${accountId}: keeping the account and its notes`);
    await deps.refreshAccounts();
    if (announced.has(accountId)) return;
    announced.add(accountId);
    const account = deps.getAccounts().find((a) => a.id === accountId);
    deps.setError(authLostMessage(account, accountId));
  };
}
