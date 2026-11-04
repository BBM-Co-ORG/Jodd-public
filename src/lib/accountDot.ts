// The dot on the Sidebar's account chip. It used to be a hard-coded green
// `●` that said "synced" for every account in every state — including one
// whose notes Apple's Advanced Data Protection makes unreadable, one with
// edits the backend refused for good, and none at all.
//
// It now says what the frontend actually knows, in this order: no account,
// an account leaving (Draining / Inactive), a whole-account block
// (`blocked_reason`), pushes the backend refused (`PendingPushes.blocked`,
// gotcha #14), work still queued, then synced. An unread or failed count is
// its own state, never "synced": a dot that cannot tell is not green.
//
// Colour is never the only signal (WCAG 1.4.1): `text` is the dot's
// accessible name and title, and the dot is hollow when there is no live
// sync to report.
import type { Account } from './types';
import { totalPending, type PendingPushes } from './controllers/accountActions';

export type AccountDotKind =
  | 'synced' | 'pending' | 'blocked' | 'signed-out' | 'inactive' | 'unknown';

export interface AccountDot {
  kind: AccountDotKind;
  /** The glyph: filled while the account syncs, hollow when it does not. */
  glyph: '●' | '○';
  /** Short text equivalent: the dot's aria-label and title. */
  text: string;
}

/**
 * @param account the current account, or undefined when there is none
 * @param counts  its `count_pending_pushes`, undefined while unread
 * @param countFailed the last read of `counts` failed
 */
export function accountDotState(
  account: Account | undefined,
  counts: PendingPushes | undefined,
  countFailed: boolean,
): AccountDot {
  if (!account) return { kind: 'signed-out', glyph: '○', text: 'Signed out' };
  const status = account.status ?? 'active';
  if (status === 'draining') return { kind: 'inactive', glyph: '○', text: 'Inactive — finishing sync' };
  if (status !== 'active') return { kind: 'inactive', glyph: '○', text: 'Inactive' };
  if (account.blocked_reason) return { kind: 'blocked', glyph: '●', text: `Sync blocked — ${account.blocked_reason}` };
  if (countFailed) return { kind: 'unknown', glyph: '○', text: 'Sync status unavailable' };
  if (!counts) return { kind: 'unknown', glyph: '○', text: 'Checking sync…' };
  if (counts.blocked > 0) {
    return {
      kind: 'blocked', glyph: '●',
      text: `Sync blocked — ${counts.blocked} ${counts.blocked === 1 ? 'change' : 'changes'} can't be sent`,
    };
  }
  const n = totalPending(counts);
  if (n > 0) {
    return { kind: 'pending', glyph: '●', text: `Syncing — ${n} ${n === 1 ? 'change' : 'changes'} waiting` };
  }
  return { kind: 'synced', glyph: '●', text: 'Synced' };
}
