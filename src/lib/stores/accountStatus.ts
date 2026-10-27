import type { Account } from '../types';

/**
 * Only `active` accounts appear in the app. Absence of the field means active:
 * every accounts.json written before this feature has no `status`, and reading
 * absence as anything else would hide every account on first load.
 */
export function isActive(a: Account): boolean {
  return (a.status ?? 'active') === 'active';
}

/** Draining and inactive alike — both are gone from the user's view. */
export function isDismissed(a: Account): boolean {
  return !isActive(a);
}

export function partitionAccounts(list: Account[]): {
  active: Account[];
  dismissed: Account[];
} {
  return {
    active: list.filter(isActive),
    dismissed: list.filter(isDismissed),
  };
}
