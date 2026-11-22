// The note list's per-keystroke cost (2026-11-09 workspace review, P0).
//
// Every editor keystroke writes `$notes`, which re-runs `listedNotes` and
// re-renders every row. NoteList is a legacy-mode component, where an object
// always counts as changed, so stable identity alone does not skip a row:
// the row's work must also be cached. Measured before the fix on 3,000
// notes of ~4 KB: ~350 ms of row work per keystroke on an M-series Mac —
// notePreview twice per row (~300 ms) and a new Intl.DateTimeFormat per
// older row (~90 ms). These tests pin the mechanisms, not timings.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import { listedNotes, type ListingInput } from './noteListing';
import { rowView } from './noteRowView';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';
import { dupStats, refreshDupStats, scheduleDupStatsRefresh } from './stores/dupStats';

const note = (k: number, extra: Record<string, unknown> = {}) =>
  ({
    uuid: `u${k}`, id: `m${k}`, account_id: 'gmail:a@x.com', label: 'Notes', title: `Note ${k}`,
    body_html: `<div>Note ${k}</div><div>body ${k}</div>`, date: '2026-01-0' + ((k % 9) + 1) + 'T10:00:00Z',
    pinned: false, local_version: 1, ...extra,
  }) as any;

const input = (notes: any[]): ListingInput => ({
  query: '', searchResults: [], smartFolder: null, smartFolderNotes: [], selectedTags: new Set(), tagMatchMode: 'AND',
  noteTags: new Map(), tagScope: 'account' as any, crossAccountTagNotes: { key: '', notes: [] } as any,
  notes, currentAccount: 'gmail:a@x.com', selectedFolder: '__ALL__', pendingDeleted: new Set(),
});

describe('listedNotes keeps row identity across re-runs', () => {
  it('returns the same row object for a note that did not change', () => {
    const notes = [note(1), note(2), note(3)];
    const a = listedNotes(input(notes));
    const b = listedNotes(input([...notes]));
    for (const row of b) expect(a).toContain(row);
  });

  it('returns a new row object only for the note that changed', () => {
    const notes = [note(1), note(2), note(3)];
    const a = listedNotes(input(notes));
    const edited = { ...notes[1], body_html: '<div>Note 2</div><div>typed</div>' };
    const b = listedNotes(input([notes[0], edited, notes[2]]));
    const byUuid = (rows: any[], u: string) => rows.find((r) => r.uuid === u);
    expect(byUuid(b, 'u1')).toBe(byUuid(a, 'u1'));
    expect(byUuid(b, 'u3')).toBe(byUuid(a, 'u3'));
    expect(byUuid(b, 'u2')).not.toBe(byUuid(a, 'u2'));
    expect(byUuid(b, 'u2').body_html).toContain('typed');
  });

  it('still hands out copies, never the store objects themselves', () => {
    const notes = [note(1)];
    expect(listedNotes(input(notes))[0]).not.toBe(notes[0]);
  });
});

describe('rowView caches a row\'s text by row object', () => {
  it('computes once per row object and again for a new one', () => {
    const row = note(7, { body_html: '<div>Title</div><div>Hello &amp; welcome</div>' });
    const first = rowView(row);
    expect(rowView(row)).toBe(first);
    expect(first.preview).toContain('Hello & welcome');
    expect(first.title).toBe('Note 7');
    expect(rowView({ ...row })).not.toBe(first);
  });

  it('falls back to Untitled for a blank title', () => {
    expect(rowView(note(8, { title: '' })).title).toBe('Untitled');
  });
});

describe('duplicate counts', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.mocked(invoke).mockReset();
    dupStats.set({});
  });
  afterEach(() => vi.useRealTimers());

  it('does not rewrite the store when the count is unchanged', async () => {
    vi.mocked(invoke).mockResolvedValue({ collapsed: 2, uuids_affected: 1 });
    const seen: unknown[] = [];
    const stop = dupStats.subscribe((v) => seen.push(v));
    await refreshDupStats('gmail:a@x.com');
    await refreshDupStats('gmail:a@x.com');
    stop();
    // initial value + one real change
    expect(seen).toHaveLength(2);
  });

  it('coalesces a burst of note changes into one read per account', async () => {
    vi.mocked(invoke).mockResolvedValue({ collapsed: 0, uuids_affected: 0 });
    for (let k = 0; k < 20; k++) scheduleDupStatsRefresh(['gmail:a@x.com', 'microsoft:b@y.com']);
    expect(invoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke).toHaveBeenCalledTimes(2);
    expect(get(dupStats)['gmail:a@x.com']).toEqual({ collapsed: 0, uuids_affected: 0 });
  });
});
