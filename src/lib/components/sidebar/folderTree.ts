// The Sidebar's folder-tree helpers, shared by its desktop/tablet markup and
// the phone Folders pane (PhoneFolders.svelte).

// Each unique label path → its own row. Depth = number of '/' in the path,
// which the template uses as the indent multiplier. The displayed name is
// the last path segment ("mySubNotes") not the full path ("Notes/myNotes/mySubNotes").
export type FolderRow = { path: string; name: string; depth: number; count: number };

// A row is visible only if every ancestor path is expanded. The implicit
// "Notes" row itself is always visible (top of every account section).
export function isRowVisible(path: string, expanded: Record<string, boolean>): boolean {
  if (!path.includes('/')) return true;
  const segs = path.split('/');
  for (let i = 1; i < segs.length; i++) {
    const ancestor = segs.slice(0, i).join('/');
    if (!expanded[ancestor]) return false;
  }
  return true;
}

export function rowHasChildren(rows: FolderRow[], path: string): boolean {
  const prefix = path + '/';
  return rows.some((r) => r.path.startsWith(prefix));
}

// Strip Jodd's reserved `__name__` markers from a system-folder segment so
// the user sees a clean display name (e.g. `__Extracts__` → `Extracts`) in
// the sidebar. Mirror of strip_workflow_markers in src-tauri/src/lib.rs —
// any change to the marker pattern needs to land in both places.
export function stripWorkflowMarkers(name: string): string {
  return name.startsWith('__') && name.endsWith('__') && name.length > 4
    ? name.slice(2, -2)
    : name;
}
