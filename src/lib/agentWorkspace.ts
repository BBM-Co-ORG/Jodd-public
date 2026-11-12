// Settings → Agent workspace (spec 2026-10-06 §5.1, §5.3). Pure helpers, so
// the rules the component follows are tested without mounting it.

export interface EligibleAccount {
  account_id: string;
  email: string;
}

export interface AgentWorkspaceStatus {
  account_id: string | null;
  folder: string;
  eligible: EligibleAccount[];
  hidden: Record<string, string[]>;
  scope_path: string;
  error: string | null;
}

/** Subtree test with the '/' boundary — the same rule as Rust's `folder_scope`. */
export function inSubtree(label: string, scope: string): boolean {
  return label === scope || label.startsWith(`${scope}/`);
}

/** Folders a user may hide: never the root, never the workspace or its
 * ancestors — an agent that cannot read its own memory has none. */
export function hideableFolders(folders: string[], workspace: string): string[] {
  return folders
    .filter(f => f !== 'Notes' && !inSubtree(f, workspace) && !inSubtree(workspace, f))
    .sort((a, b) => a.localeCompare(b));
}

/** Whether `folder` is hidden, directly or through a hidden ancestor. */
export function isHidden(hidden: Record<string, string[]>, accountId: string, folder: string): boolean {
  return (hidden[accountId] ?? []).some(h => inSubtree(folder, h));
}

/** The hidden map after one toggle — a new object, for an optimistic update
 * that can be rolled back by restoring the previous one. */
export function withHidden(
  hidden: Record<string, string[]>,
  accountId: string,
  folder: string,
  hide: boolean,
): Record<string, string[]> {
  const current = hidden[accountId] ?? [];
  const next = hide
    ? (current.includes(folder) ? current : [...current, folder])
    : current.filter(f => f !== folder);
  return { ...hidden, [accountId]: next };
}

export const HOOK_SNIPPET = `{ "hooks": { "SessionStart": [ { "hooks": [
  { "type": "command", "command": "jodd-mcp brief --cwd \\"$CLAUDE_PROJECT_DIR\\"" } ] } ] } }`;
