// Settings › AI › Notes on disk for Claude Code (spec 2026-11-05 §5.7).
// Pure helpers, so the rules the component follows are tested without
// mounting it.

export interface MirrorAccount {
  account_id: string;
  email: string;
}

export interface MirrorAccountStatus {
  account_id: string;
  notes: number;
  written: number;
  deleted: number;
  masked_notes: number;
  skipped_attachments: number;
  overwritten_local_edits: number;
  held_back: boolean;
  skipped?: string | null;
}

export interface MirrorStatus {
  finished_at: string;
  outcome: 'ok' | 'partial' | 'failed' | string;
  path: string;
  accounts: MirrorAccountStatus[];
  problems: string[];
}

export interface AgentFilesView {
  enabled: boolean;
  path: string;
  default_path: string;
  accounts: string[];
  choices: MirrorAccount[];
  last: MirrorStatus | null;
  error: string | null;
  notice: string | null;
}

/** Emitted by the app's live sync after every background run (Rust
 * `agent_files::live::EVENT`): the route by which a run nobody clicked reaches
 * the screen (gotcha #6). */
export const AGENT_FILES_SYNCED = 'agent-files-synced';

/** Listen for {@link AGENT_FILES_SYNCED}; the returned function stops, even
 * when called before the listener was registered. */
export function onAgentFilesSynced(listen: (event: string, cb: () => void) => Promise<() => void>, cb: () => void): () => void {
  let stop: (() => void) | null = null;
  let stopped = false;
  listen(AGENT_FILES_SYNCED, cb)
    .then(fn => { if (stopped) fn(); else stop = fn; })
    .catch(() => {});
  return () => { stopped = true; stop?.(); };
}

/** One run of `sync_agent_files_now`, part by part. */
export interface SyncOutcome {
  mirror: MirrorStatus | null;
  mirror_error: string | null;
  skills: import('./agentSkills').SkillsStatus | null;
  skills_error: string | null;
}

/** The SessionStart hook. With the mirror on, the sync runs first, so the
 * briefing describes the files as they now are. */
export function hookSnippet(mirror: boolean): string {
  const brief = '{ "type": "command", "command": "jodd-mcp brief --cwd \\"$CLAUDE_PROJECT_DIR\\"" }';
  const sync = '{ "type": "command", "command": "jodd-mcp sync-agent-files --quiet" },\n  ';
  return `{ "hooks": { "SessionStart": [ { "hooks": [\n  ${mirror ? sync : ''}${brief} ] } ] } }`;
}

/** Lets Claude Code read the mirror without asking each time. */
export function permissionSnippet(path: string): string {
  return JSON.stringify({ permissions: { additionalDirectories: [path] } }, null, 2);
}

/** The account list after one checkbox — a new array, for an optimistic
 * update that can be rolled back. */
export function withAccount(accounts: string[], id: string, on: boolean): string[] {
  if (on) return accounts.includes(id) ? accounts : [...accounts, id];
  return accounts.filter(a => a !== id);
}

/** One line about the last run, or '' when there was none. */
export function summarize(last: MirrorStatus | null): string {
  if (!last) return '';
  const notes = last.accounts.reduce((n, a) => n + a.notes, 0);
  const written = last.accounts.reduce((n, a) => n + a.written, 0);
  const when = formatWhen(last.finished_at);
  if (last.outcome === 'failed') return `Last sync failed${when ? ` (${when})` : ''}.`;
  const tail = last.outcome === 'partial' ? ' Not finished yet; the next sync continues.' : '';
  return `${notes} notes on disk, ${written} updated in the last sync${when ? ` (${when})` : ''}.${tail}`;
}

/** Whether any account's deletions were held back (spec §4.3). */
export function heldBack(last: MirrorStatus | null): boolean {
  return !!last?.accounts.some(a => a.held_back);
}

function formatWhen(iso: string): string {
  const t = new Date(iso);
  return Number.isNaN(t.getTime()) ? '' : t.toLocaleString();
}
