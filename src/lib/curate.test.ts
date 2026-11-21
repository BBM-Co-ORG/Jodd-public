import { describe, expect, it } from 'vitest';
import { appendPreview, choices, summaryLine, type Proposal } from './curate';

const dup: Proposal = {
  id: 1, kind: 'duplicate', status: 'pending', created_at: 0, error: null,
  payload: {
    notes: [
      { uuid: 'A', title: 'Course v1', label: 'Notes/Inbox', local_version: 1, date: '', chars: 10 },
      { uuid: 'B', title: 'Course v2', label: 'Notes/Inbox', local_version: 1, date: '', chars: 99 },
    ],
    action: { type: 'append', into: 'B' }, reason: 'Same video.', evidence: ['both cite youtu.be/x'],
  },
};

describe('curate helpers', () => {
  it('offers keep and append for every note, recommended first', () => {
    const c = choices(dup);
    expect(c).toHaveLength(4);
    expect(c[0].action).toEqual({ type: 'append', into: 'B' });
    expect(c.map(x => x.action)).toContainEqual({ type: 'keep', keep: 'A' });
  });

  it('misfiled and secret proposals offer their single action', () => {
    const mis: Proposal = { ...dup, kind: 'misfiled', payload: { ...dup.payload, notes: [dup.payload.notes[0]], action: { type: 'move', to: 'Notes/Work' } } };
    expect(choices(mis).map(c => c.label)).toEqual(['Move to Work']);
    const sec: Proposal = { ...mis, kind: 'secret', payload: { ...mis.payload, action: { type: 'hide' } } };
    expect(choices(sec)[0].action).toEqual({ type: 'hide' });
  });

  it('builds the append result the way apply does: kept first, then each merged note', () => {
    const texts = [{ uuid: 'A', title: 'Course v1', label: 'Notes', text: 'alpha' }, { uuid: 'B', title: 'Course v2', label: 'Notes', text: 'beta' }];
    expect(appendPreview(texts, 'B')).toBe('beta\n\n———\nMerged from: Course v1\nalpha');
  });

  it('summarises a scan in plain words', () => {
    expect(summaryLine({ duplicates: 1, misfiled: 2, secrets: 0, skipped: 1, notes: [] }))
      .toBe('Found 1 duplicate group, 2 misfiled notes. 1 could not be checked; try again later.');
    expect(summaryLine({ duplicates: 0, misfiled: 0, secrets: 0, skipped: 0, notes: ['No AI provider is set up.'] }))
      .toBe('Nothing new to fix. No AI provider is set up.');
  });
});
