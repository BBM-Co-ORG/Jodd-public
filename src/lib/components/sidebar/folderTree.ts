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
  return hasWorkflowMarkers(name) ? name.slice(2, -2) : name;
}

function hasWorkflowMarkers(name: string): boolean {
  return name.startsWith('__') && name.endsWith('__') && name.length > 4;
}

// ─── Agent folders (spec 2026-10-28 §6) ─────────────────────────────────
// What the Sidebar's "Agents" group holds. In production only agents write
// `__name__` folders — the agent workspace (`Notes/__Agent__`) and whatever
// folder jodd-mcp's allowlist names (`Notes/__Claude__` by convention); none
// of Jodd's own AI workflows file there (they use the ordinary Notes/Inbox).
//
// The leaf pattern is the contract (gotcha #4): `kind = 'system_workflow'`
// is derived from it on every reconciliation, never migrated.

// Folders Jodd once wrote to and no longer does: ordinary folders now, the
// user's to rename or delete, and listed with the other folders.
// `__Extracts__` retired with extract filing (2026-09-15).
export const RETIRED_WORKFLOW_FOLDERS: readonly string[] = ['__Extracts__'];

/** A leaf that names an agent folder by its pattern alone. */
export function isAgentLeaf(leaf: string): boolean {
  return hasWorkflowMarkers(leaf) && !RETIRED_WORKFLOW_FOLDERS.includes(leaf);
}

/**
 * The agent folder `path` is in: itself, or its nearest ancestor, that is
 * kind `system_workflow` AND has a `__name__` leaf that is not retired.
 * `null` when there is none. A legacy plain-named `system_workflow` row
 * (`Notes/Lessons` — the kind is never demoted) is not an agent folder.
 */
export function agentFolderRoot(kinds: Record<string, string>, path: string): string | null {
  const segs = path.split('/');
  for (let i = segs.length; i >= 1; i--) {
    const p = segs.slice(0, i).join('/');
    if (kinds[p] === 'system_workflow' && isAgentLeaf(segs[i - 1])) return p;
  }
  return null;
}

/** In the Agents group: an agent folder or anything inside one. */
export function isAgentFolder(kinds: Record<string, string>, path: string): boolean {
  return agentFolderRoot(kinds, path) !== null;
}

/**
 * Rename, Move to and Delete are hidden: an agent folder itself, not what is
 * inside it (`__Agent__/Projects` stays the user's). Renaming one would cut
 * it off from the agents that write there — jodd-mcp's allowlist names it.
 */
export function isProtectedAgentFolder(kinds: Record<string, string>, path: string): boolean {
  return agentFolderRoot(kinds, path) === path;
}
