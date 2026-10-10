import type { SmartFolderKind } from './stores/notes';

/** The Sidebar's row names for the virtual Smart Folders. */
export const SMART_FOLDER_NAMES: Record<SmartFolderKind, string> = {
  orphaned: 'Orphaned',
  stale: 'Stale',
  extracts: 'Extracts',
  unreviewed: 'Unreviewed',
};

/**
 * Strip Jodd's reserved `__name__` markers from a workflow-folder segment
 * (`__Extracts__` → `Extracts`). Mirror of `strip_workflow_markers` in
 * src-tauri/src/lib.rs — a change to the marker pattern lands in both.
 */
function stripWorkflowMarkers(name: string): string {
  return name.startsWith('__') && name.endsWith('__') && name.length > 4
    ? name.slice(2, -2)
    : name;
}

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
 * What the empty editor pane calls the view: a folder by its full path, the
 * form the editor's "Browsing …" context notice uses beside it.
 */
export function viewPath(
  smart: { kind: SmartFolderKind } | null,
  folder: string | null | undefined,
  accountDisplay: string,
): string {
  return virtualViewName(smart, folder, accountDisplay) ?? folder ?? '';
}
