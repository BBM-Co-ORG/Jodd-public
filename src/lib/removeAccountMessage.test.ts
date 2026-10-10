import { describe, expect, it } from 'vitest';
import { removeAccountMessage } from './removeAccountMessage';
import type { Account } from './types';

const vault = { id: 'localfs:4A33B704-4C3E-4C27-8209-7237E26D1C1A', email: 'JoddVaultCopy', backend_kind: 'local_fs' } as Account;

describe('removeAccountMessage', () => {
  it('names the account the way the sidebar does, not by its id', () => {
    const m = removeAccountMessage(vault, 0);
    expect(m).toBe('Remove localfs:JoddVaultCopy from Jodd?');
    expect(m).not.toContain('4A33B704');
  });

  it('warns about a confirmed unsent count', () => {
    expect(removeAccountMessage(vault, 3)).toBe(
      'Remove localfs:JoddVaultCopy from Jodd? 3 unsent edit(s) have not reached the server yet and will be deleted with it.',
    );
  });

  it('warns when the unsent count could not be confirmed', () => {
    expect(removeAccountMessage(vault, undefined)).toContain('could not be confirmed');
  });

  it('uses the display name for a draining account too', () => {
    const m = removeAccountMessage({ ...vault, status: 'draining' } as Account, undefined);
    expect(m.startsWith('Remove localfs:JoddVaultCopy from Jodd? It\'s still finishing a sync')).toBe(true);
  });
});
