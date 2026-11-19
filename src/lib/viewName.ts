import type { SmartFolderKind } from './stores/notes';
// One copy of the marker rule, shared with the Sidebar (spec 2026-10-28 §6.5).
import { stripWorkflowMarkers } from './components/sidebar/folderTree';

/** The Sidebar's row names for the virtual Smart Folders. Said in the
 *  user's words, not the graph's: "Orphaned" and "Stale" were jargon to
 *  someone arriving from Apple Notes (2026-11-09 workspace review). */
export const SMART_FOLDER_NAMES: Record<SmartFolderKind, string> = {
  orphaned: 'Not linked',
  stale: 'Untouched',
  extracts: 'Made with AI',
  unreviewed: 'Unreviewed',
};

/** The one line under each view's name on the phone, where a tooltip never
 *  shows (touch has no hover; spec 2026-11-12 §1). Short enough for 360px. */
export const SMART_FOLDER_HINTS: Record<SmartFolderKind, string> = {
  orphaned: 'No other note links to them',
  stale: 'Not edited in 30 days',
  extracts: 'From a link, video or other source',
  unreviewed: 'Changed elsewhere, not yet reviewed',
};

/** What an empty Smart Folder says. Its own sentence, because a view is not
 *  a place a note sits in: "No notes in Not linked" reads as nonsense. */
export const SMART_FOLDER_EMPTY: Record<SmartFolderKind, string> = {
  orphaned: 'Every note is linked from another note.',
  stale: 'No notes left untouched for 30 days.',
  extracts: 'No notes made with AI yet.',
  unreviewed: 'Nothing left to review.',
};

/**
 * The name of a selection that is not a plain folder path — a Smart Folder or
 * one of the Sidebar's sentinels — or `null` for a real folder. A Smart
 * Folder wins: it is selected ON TOP of `selectedFolder`, which keeps the last
 * real folder underneath (Sidebar's selectSmartFolder does not clear it), so
 * reading the folder alone names the wrong view.
 */
function virtualViewName(
  smart: { kind: SmartFolderKind } | null,
  folder: string | null | undefined,
  accountDisplay: string,
): string | null {
  if (smart) return SMART_FOLDER_NAMES[smart.kind];
  if (folder === '__ALL__') return `All ${accountDisplay}`;
  if (folder === '__TRASH__') return 'Recently Deleted';
  return null;
}

/** What the list header calls the view: a folder by its leaf. */
export function viewName(
  smart: { kind: SmartFolderKind } | null,
  folder: string | null | undefined,
  accountDisplay: string,
): string {
  return virtualViewName(smart, folder, accountDisplay)
    ?? stripWorkflowMarkers(folder?.split('/').pop() || folder || '');
}

/**
 * What the empty editor pane calls the view: a folder by its full path, so a
 * nested folder is told apart from a same-named one elsewhere.
 */
export function viewPath(
  smart: { kind: SmartFolderKind } | null,
  folder: string | null | undefined,
  accountDisplay: string,
): string {
  return virtualViewName(smart, folder, accountDisplay) ?? folder ?? '';
}
