import { describe, it, expect } from 'vitest';
import { groupConversations, statsLine, describeScope, type ConversationSummary } from './askHistory';

const DAY = 86_400_000;
const NOW = new Date(2026, 9, 14, 15, 0).getTime(); // local 15:00

function c(id: string, updated_at: number): ConversationSummary {
  return { id, title: id, scope: { kind: 'all_accounts' }, updated_at, archived: false, turn_count: 1 };
}

describe('groupConversations', () => {
  it('buckets by local day, keeps order, drops empty groups', () => {
    const startOfToday = new Date(2026, 9, 14).getTime();
    const groups = groupConversations(
      [c('a', NOW - 1000), c('b', startOfToday), c('c', startOfToday - 1), c('d', NOW - 8 * DAY)],
      NOW,
    );
    expect(groups.map((g) => [g.label, g.items.map((i) => i.id)])).toEqual([
      ['Today', ['a', 'b']],
      ['Last 7 days', ['c']],
      ['Older', ['d']],
    ]);
  });

  it('returns no groups for an empty list', () => {
    expect(groupConversations([], NOW)).toEqual([]);
  });
});

describe('statsLine', () => {
  it('matches the live answer line', () => {
    expect(statsLine({ notes_in_scope: 6655, notes_considered: 400, notes_used: 9, trimmed: true, dropped_citations: 2 }))
      .toBe('6,655 in scope → 400 considered → 9 read → trimmed to fit → 2 unverifiable citation(s) removed');
  });
});

describe('describeScope', () => {
  it('names each scope kind', () => {
    expect(describeScope({ kind: 'all_accounts' })).toBe('All accounts');
    expect(describeScope({ kind: 'account', account_id: 'gmail:a@x' })).toBe('gmail:a@x');
    expect(describeScope({ kind: 'folder', account_id: 'gmail:a@x', label: 'Notes/X' })).toBe('gmail:a@x · Notes/X');
  });

  it('names a note scope by title, with its related options when on', () => {
    const base = { kind: 'note' as const, account_id: 'a', uuid: 'u', title: 'Plan', links: false, tags: false, depth: 1 };
    expect(describeScope(base)).toBe('a · Plan');
    expect(describeScope({ ...base, links: true, tags: true, depth: 2 })).toBe('a · Plan · links, tags, depth 2');
    expect(describeScope({ ...base, links: true })).toBe('a · Plan · links, depth 1');
  });
});
