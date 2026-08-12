import { describe, it, expect } from 'vitest';
import { askScope } from './askScope';

describe('askScope', () => {
  it('sends a real folder label for an ordinary folder selection', () => {
    expect(askScope('folder', 'acct@x', 'Notes/Work')).toEqual({
      kind: 'folder',
      account_id: 'acct@x',
      label: 'Notes/Work',
    });
  });

  it('falls back to account scope when selectedFolder is the "__ALL__" sentinel', () => {
    // '__ALL__' is set by the per-account "All" row (Sidebar.svelte:1155). It
    // is truthy but not a real folder label — sending it to the backend as a
    // `label` would match zero rows and produce a false "no notes found".
    expect(askScope('folder', 'acct@x', '__ALL__')).toEqual({
      kind: 'account',
      account_id: 'acct@x',
    });
  });

  it('falls back to account scope when selectedFolder is the "__TRASH__" sentinel', () => {
    // '__TRASH__' is set by the Trash row (Sidebar.svelte:1247). Same failure
    // mode as '__ALL__'.
    expect(askScope('folder', 'acct@x', '__TRASH__')).toEqual({
      kind: 'account',
      account_id: 'acct@x',
    });
  });

  it('falls back to account scope when selectedFolder is empty', () => {
    expect(askScope('folder', 'acct@x', '')).toEqual({
      kind: 'account',
      account_id: 'acct@x',
    });
  });

  it('all_accounts scope ignores selectedFolder entirely', () => {
    expect(askScope('all', 'acct@x', '__ALL__')).toEqual({ kind: 'all_accounts' });
  });

  it('account scope ignores selectedFolder entirely', () => {
    expect(askScope('account', 'acct@x', 'Notes/Work')).toEqual({
      kind: 'account',
      account_id: 'acct@x',
    });
  });

  // `currentAccount` is `string | null` and starts null (stores/notes.ts), so
  // an absent account is a reachable state, not a theoretical one. Every
  // account-anchored AskScope variant requires an account_id that Rust
  // deserializes into a String — serde rejects null outright, so there is no
  // degraded request to fall back to. Returning null is how the caller learns
  // to not ask at all.
  describe('with no current account', () => {
    for (const [label, account] of [
      ['null', null],
      ['undefined', undefined],
      ['empty string', ''],
    ] as const) {
      it(`refuses account scope when currentAccount is ${label}`, () => {
        expect(askScope('account', account, 'Notes/Work')).toBeNull();
      });

      it(`refuses folder scope when currentAccount is ${label}`, () => {
        expect(askScope('folder', account, 'Notes/Work')).toBeNull();
      });

      it(`still allows all_accounts scope when currentAccount is ${label}`, () => {
        // 'all_accounts' carries no account_id, so it is unaffected — this is
        // the escape hatch the modal's hint points the user at.
        expect(askScope('all', account, 'Notes/Work')).toEqual({ kind: 'all_accounts' });
      });
    }
  });
});
