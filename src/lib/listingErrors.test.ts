import { describe, expect, it } from 'vitest';
import { createListingErrors, LEAVE } from './listingErrors';

const LOST = 'transient: Jodd no longer has All files access — turn it on in Settings → Apps → Jodd.';

describe('listingErrors', () => {
  it('shows a failure, then clears it when that account lists again', () => {
    const le = createListingErrors();
    const shown = le.settle([{ accountId: 'localfs:a', error: LOST }], null) as string;
    expect(shown).toBe(LOST);
    expect(le.settle([{ accountId: 'localfs:a' }], shown)).toBeNull();
  });

  it('never clears a message it did not set (e.g. Signed out)', () => {
    const le = createListingErrors();
    le.settle([{ accountId: 'localfs:a', error: LOST }], null);
    const signedOut = 'Signed out — Keychain credentials were removed. Please sign in again.';
    expect(le.settle([{ accountId: 'localfs:a' }], signedOut)).toBe(LEAVE);
  });

  it('leaves a clean store alone when nothing failed', () => {
    const le = createListingErrors();
    expect(le.settle([{ accountId: 'gmail:x' }], null)).toBe(LEAVE);
  });

  it('does not resurrect a dismissed message on a later success', () => {
    const le = createListingErrors();
    le.settle([{ accountId: 'localfs:a', error: LOST }, { accountId: 'localfs:b', error: 'b broke' }], null);
    // user pressed × (store null), then a recovers while b still fails
    expect(le.settle([{ accountId: 'localfs:a' }], null)).toBe(LEAVE);
  });

  it('re-shows a failure that happens again after dismissal', () => {
    const le = createListingErrors();
    le.settle([{ accountId: 'localfs:a', error: LOST }], null);
    expect(le.settle([{ accountId: 'localfs:a', error: LOST }], null)).toBe(LOST);
  });

  it('narrows to the accounts still failing when one of two recovers', () => {
    const le = createListingErrors();
    const both = le.settle([{ accountId: 'localfs:a', error: LOST }, { accountId: 'localfs:b', error: 'b broke' }], null) as string;
    expect(both).toBe(`${LOST}\nb broke`);
    expect(le.settle([{ accountId: 'localfs:a' }], both)).toBe('b broke');
  });

  it('a folder load for one account does not drop another account\'s failure', () => {
    const le = createListingErrors();
    const shown = le.settle([{ accountId: 'localfs:a', error: LOST }], null) as string;
    // unchanged text: the bar keeps showing LOST, untouched
    expect(le.settle([{ accountId: 'gmail:x' }], shown)).toBe(LEAVE);
    expect(le.settle([{ accountId: 'localfs:a' }], shown)).toBeNull();
  });

  it('a complete settle forgets accounts that are no longer listed (removed)', () => {
    const le = createListingErrors();
    const shown = le.settle([{ accountId: 'localfs:a', error: LOST }], null) as string;
    expect(le.settle([{ accountId: 'gmail:x' }], shown, { complete: true })).toBeNull();
  });
});
