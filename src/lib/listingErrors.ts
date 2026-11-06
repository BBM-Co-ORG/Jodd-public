/**
 * Which listing failures the global `error` store is currently showing, so a
 * successful re-list can take its own message back down.
 *
 * ErrorBar's dismissal is manual on purpose: the same store carries messages
 * that stay true until the user acts ("Signed out — … sign in again"), so a
 * blanket clear on success would retire them. But a listing failure is
 * different — once the account lists again, "Jodd no longer has All files
 * access" is false, and on the device pass (2026-09-29) it stayed up after
 * recovery. The rule here: clear or narrow the bar ONLY while it still shows
 * exactly the text this tracker last put there. Anything else — another
 * message, or null because the user dismissed it — is left alone.
 *
 * `settle` returns the store's next value, or `LEAVE` to not touch it.
 */
export const LEAVE: unique symbol = Symbol('leave');

export interface ListingOutcome {
  accountId: string;
  /** Present when this account's listing failed (non-auth). */
  error?: string;
}

export function createListingErrors() {
  const byAccount = new Map<string, string>();
  let shown: string | null = null;

  return {
    /**
     * @param outcomes one entry per account this load listed
     * @param current  the error store's value right now
     * @param opts.complete the outcomes cover every active account, so any
     *   other tracked account was removed and is forgotten
     */
    settle(
      outcomes: ListingOutcome[],
      current: string | null,
      opts: { complete?: boolean } = {},
    ): string | null | typeof LEAVE {
      if (opts.complete) {
        const listed = new Set(outcomes.map((o) => o.accountId));
        for (const id of [...byAccount.keys()]) if (!listed.has(id)) byAccount.delete(id);
      }
      for (const o of outcomes) {
        if (o.error !== undefined) byAccount.set(o.accountId, o.error);
        else byAccount.delete(o.accountId);
      }
      const text = byAccount.size > 0 ? [...byAccount.values()].join('\n') : null;
      const failedNow = outcomes.some((o) => o.error !== undefined);
      // A fresh failure is shown whatever the bar holds — the behaviour
      // loadNotes/loadFolderNotes already had.
      const owned = shown !== null && current === shown;
      if (!failedNow && !owned) return LEAVE;
      if (text === current) {
        shown = text;
        return LEAVE;
      }
      shown = text;
      return text;
    },
  };
}
