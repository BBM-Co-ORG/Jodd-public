import { accountDisplay } from './stores/notes';
import type { Account } from './types';

/**
 * The body of Sidebar's "Remove account?" confirmation.
 *
 * Names the account with `accountDisplay`, the same label the sidebar shows.
 * It used to interpolate `a.id`, which for a Local Folder or SSH account is
 * `localfs:<UUID>` / `ssh:<UUID>` — two vaults could not be told apart in the
 * one dialog that destroys one of them (device pass, 2026-09-29).
 *
 * `unsent` is `totalUnsent(count_pending_pushes)`, or undefined when that
 * count could not be read. It is ignored for a draining account: the backend
 * queues that removal until the queue is empty, so nothing is deleted unsent.
 */
export function removeAccountMessage(a: Account, unsent: number | undefined): string {
  const name = accountDisplay(a);
  if (a.status === 'draining') {
    return `Remove ${name} from Jodd? It's still finishing a sync — removal will happen automatically once that's done. Anything still syncing will be sent first, not lost.`;
  }
  const warning =
    unsent === undefined
      ? ' This account may still have unsent edits — the pending count could not be confirmed. Anything unsent will be deleted with it.'
      : unsent > 0
        ? ` ${unsent} unsent edit(s) have not reached the server yet and will be deleted with it.`
        : '';
  return `Remove ${name} from Jodd?${warning}`;
}
