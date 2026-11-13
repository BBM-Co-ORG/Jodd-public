// The sidebar's stored fold choices, one per device, shared by the
// desktop/tablet Sidebar and the phone's Folders pane (spec 2026-11-12), so a
// fold made on one layout holds on the other after a rotation.
//
// - Views: closed by default, so the folders start near the top. The stored
//   value is whether it was OPENED, so a fresh install (or unreadable
//   storage) reads as closed. Same key PR #342 used for the phone fold.
// - Tags: one choice for the whole column, closed by default. It used to be
//   per account, which made the section flip open or shut whenever the
//   account it showed changed (2026-11-12 critique).
// - Accounts: the account ids the user folded away. Tracks COLLAPSED ids,
//   because the default is open: an account nobody has touched must start
//   open.
//
// Corrupt or unavailable storage must never take the sidebar down: every
// read falls back to the default, every write is best-effort.

export const VIEWS_OPEN_KEY = 'jodd.sidebar.viewsOpen';
export const TAGS_OPEN_KEY = 'jodd.sidebar.tagsOpen';
/** Before 2026-11-12: the account ids whose Tags section was open. */
export const LEGACY_TAGS_EXPANDED_KEY = 'jodd.sidebar.tagsExpanded';
export const ACCOUNTS_COLLAPSED_KEY = 'jodd.sidebar.accountsCollapsed';

function read(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function write(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Non-fatal: the toggle still works for this session.
  }
}

export function loadViewsOpen(): boolean {
  return read(VIEWS_OPEN_KEY) === 'true';
}

export function saveViewsOpen(open: boolean): void {
  write(VIEWS_OPEN_KEY, String(open));
}

/** Open if the user opened it, or if they had opened any account's Tags
 *  under the old per-account key (so an upgrade keeps what they saw). */
export function loadTagsOpen(): boolean {
  const v = read(TAGS_OPEN_KEY);
  if (v !== null) return v === 'true';
  try {
    const legacy = JSON.parse(read(LEGACY_TAGS_EXPANDED_KEY) ?? '[]');
    return Array.isArray(legacy) && legacy.length > 0;
  } catch {
    return false;
  }
}

export function saveTagsOpen(open: boolean): void {
  write(TAGS_OPEN_KEY, String(open));
}

export function loadCollapsedAccounts(): Set<string> {
  try {
    const parsed = JSON.parse(read(ACCOUNTS_COLLAPSED_KEY) ?? '[]');
    return Array.isArray(parsed) ? new Set(parsed.filter((v) => typeof v === 'string')) : new Set();
  } catch {
    return new Set();
  }
}

export function saveCollapsedAccounts(ids: Set<string>): void {
  write(ACCOUNTS_COLLAPSED_KEY, JSON.stringify([...ids]));
}

/** The set with `id` folded or unfolded, saved. */
export function toggleCollapsedAccount(ids: Set<string>, id: string): Set<string> {
  const next = new Set(ids);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  saveCollapsedAccounts(next);
  return next;
}
