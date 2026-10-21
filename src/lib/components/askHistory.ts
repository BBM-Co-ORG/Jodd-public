// Ask Jodd conversation history — shapes from src-tauri/src/ask/history.rs
// and ask_history_commands.rs (pinned there by wire-shape tests), plus the
// pure helpers the rail and the modal share.
import type { AskScope } from './askScope';
import type { CitedNote } from './askCitations';

export type TurnStats = {
  notes_in_scope: number;
  notes_considered: number;
  notes_used: number;
  trimmed: boolean;
  dropped_citations: number;
};
export type ConversationSummary = {
  id: string;
  title: string;
  scope: AskScope;
  updated_at: number;
  archived: boolean;
  turn_count: number;
};
export type StoredTurn = {
  seq: number;
  question: string;
  answer_markdown: string;
  cited: CitedNote[];
  read: { uuid: string; account_id: string }[];
  stats: TurnStats;
  created_at: number;
};
export type StoredConversation = {
  id: string;
  title: string;
  scope: AskScope;
  destination: string;
  created_at: number;
  updated_at: number;
  archived: boolean;
  turns: StoredTurn[];
};
export type AskSessionInfo = { session_id: string; destination: string; scope: string };
export type AskResume =
  | { kind: 'ready'; session: AskSessionInfo }
  | { kind: 'needs_confirm'; previous: string; current: string }
  | { kind: 'read_only'; reason: string };

export function statsLine(s: TurnStats): string {
  const bits = [
    `${s.notes_in_scope.toLocaleString()} in scope`,
    `${s.notes_considered.toLocaleString()} considered`,
    `${s.notes_used} read`,
  ];
  if (s.trimmed) bits.push('trimmed to fit');
  if (s.dropped_citations > 0) bits.push(`${s.dropped_citations} unverifiable citation(s) removed`);
  return bits.join(' → ');
}

type Group = { label: 'Today' | 'Last 7 days' | 'Older'; items: ConversationSummary[] };

/** Input is newest-first (as the backend lists it); order is kept. */
export function groupConversations(list: ConversationSummary[], now: number): Group[] {
  const d = new Date(now);
  const startOfToday = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const weekAgo = startOfToday - 6 * 86_400_000;
  const groups: Group[] = [
    { label: 'Today', items: [] },
    { label: 'Last 7 days', items: [] },
    { label: 'Older', items: [] },
  ];
  for (const c of list) {
    const g = c.updated_at >= startOfToday ? 0 : c.updated_at >= weekAgo ? 1 : 2;
    groups[g].items.push(c);
  }
  return groups.filter((g) => g.items.length > 0);
}

export function describeScope(scope: AskScope): string {
  switch (scope.kind) {
    case 'all_accounts': return 'All accounts';
    case 'account': return scope.account_id;
    case 'folder': return `${scope.account_id} · ${scope.label}`;
  }
}
