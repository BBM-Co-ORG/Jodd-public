import { describe, it, expect } from 'vitest';
import { mergeAccountListings } from './mergeListings';

// The All view's refresh (App.svelte `loadNotes`) fans `list_notes` out over
// every account with Promise.allSettled. Before this helper, a rejected
// listing went only to console.error and the merge kept only the fulfilled
// accounts — so an unreadable Local Folder vault's notes vanished from the
// list with no message at all (gotcha #6: the failure never reached a pixel).
const isAuthLost = (e: unknown) => String(e).includes('AUTH_LOST');
const notRecent = () => false;
const note = (account_id: string, uuid: string, title = uuid) => ({ account_id, uuid, title });

describe('mergeAccountListings', () => {
  it('keeps a failed account\'s existing notes and reports why', () => {
    const before = [note('localfs:v', 'A'), note('localfs:v', 'B'), note('gmail:x', 'G-old')];
    const r = mergeAccountListings(
      ['localfs:v', 'gmail:x'],
      [
        { status: 'rejected', reason: 'Jodd no longer has All files access' },
        { status: 'fulfilled', value: [note('gmail:x', 'G-new')] },
      ],
      before,
      isAuthLost,
      notRecent,
    );
    expect(r.merged.map((n) => n.uuid).sort()).toEqual(['A', 'B', 'G-new']);
    expect(r.errors).toEqual(['Jodd no longer has All files access']);
    expect(r.authLost).toEqual([]);
  });

  it('leaves auth loss as it was: no error message, notes not kept, account queued for recovery', () => {
    const before = [note('gmail:x', 'G1'), note('localfs:v', 'A')];
    const r = mergeAccountListings(
      ['gmail:x', 'localfs:v'],
      [
        { status: 'rejected', reason: 'AUTH_LOST: token revoked' },
        { status: 'fulfilled', value: [note('localfs:v', 'A')] },
      ],
      before,
      isAuthLost,
      notRecent,
    );
    expect(r.merged.map((n) => n.uuid)).toEqual(['A']);
    expect(r.errors).toEqual([]);
    expect(r.authLost).toEqual(['gmail:x']);
  });

  it('still protects tmp: blanks and recently-saved notes, without duplicating kept ones', () => {
    const before = [note('localfs:v', 'A'), note('gmail:x', 'tmp:1'), note('gmail:x', 'R')];
    const r = mergeAccountListings(
      ['localfs:v', 'gmail:x'],
      [
        { status: 'rejected', reason: new Error('boom') },
        { status: 'fulfilled', value: [note('gmail:x', 'G')] },
      ],
      before,
      isAuthLost,
      (n) => n.uuid === 'R',
    );
    expect(r.merged.map((n) => n.uuid).sort()).toEqual(['A', 'G', 'R', 'tmp:1']);
    expect(r.errors).toEqual(['Error: boom']);
  });

  it('a fully successful refresh reports nothing and replaces the list', () => {
    const r = mergeAccountListings(
      ['gmail:x'],
      [{ status: 'fulfilled', value: [note('gmail:x', 'N')] }],
      [note('gmail:x', 'OLD')],
      isAuthLost,
      notRecent,
    );
    expect(r.merged.map((n) => n.uuid)).toEqual(['N']);
    expect(r.errors).toEqual([]);
  });
});
