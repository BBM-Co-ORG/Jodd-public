import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  FIND_HL,
  FIND_HL_CURRENT,
  FIND_HL_GONE,
  FIND_INPUT_DELAY_MS,
  FindPainter,
  createDebounce,
  overlapsAny,
  recordBodyReplacements,
  recordReplaced,
  replaceAllInTitle,
} from './findReplace';

describe('recordReplaced', () => {
  it('remembers the inserted text and shifts the spans after it', () => {
    // "line line": the second "line" was replaced earlier, then the first
    // grows to "LINES" (+1) — the remembered span moves right by one.
    const spans = recordReplaced([[5, 9]], 0, 4, 5);
    expect(spans).toEqual([[6, 10], [0, 5]]);
  });

  it('leaves spans before the replacement alone', () => {
    expect(recordReplaced([[0, 2]], 5, 9, 2)).toEqual([[0, 2], [5, 7]]);
  });

  it('records nothing for an empty replacement, but still shifts', () => {
    expect(recordReplaced([[10, 12]], 0, 4, 0)).toEqual([[6, 8]]);
  });
});

describe('overlapsAny', () => {
  it('treats spans as half-open', () => {
    expect(overlapsAny([[2, 5]], 5, 7)).toBe(false);
    expect(overlapsAny([[2, 5]], 4, 6)).toBe(true);
    expect(overlapsAny([[2, 5]], 0, 2)).toBe(false);
  });
});

describe('replaceAllInTitle', () => {
  it('replaces left to right and remembers each replacement where it now sits', () => {
    // "note and note" → "notes and notes": the second span sits one further right.
    const r = replaceAllInTitle('note and note', [{ start: 0, end: 4 }, { start: 9, end: 13 }], ['notes', 'notes'], []);
    expect(r.title).toBe('notes and notes');
    expect(r.spans).toEqual([[0, 5], [10, 15]]);
    // The bug this exists for: the next find must skip both, so "note" inside
    // "notes" is not matched again ("notess").
    expect(overlapsAny(r.spans, 0, 4)).toBe(true);
    expect(overlapsAny(r.spans, 10, 14)).toBe(true);
  });

  it('keeps text outside the matches', () => {
    const r = replaceAllInTitle('a-b-c', [{ start: 1, end: 2 }, { start: 3, end: 4 }], ['+', ''], []);
    expect(r.title).toBe('a+bc');
    expect(r.spans).toEqual([[1, 2]]);
  });
});

describe('recordBodyReplacements', () => {
  it('shifts each match by the replacements before it in the same pass', () => {
    const spans = recordBodyReplacements([], [{ absStart: 0, absEnd: 4 }, { absStart: 10, absEnd: 14 }], ['LINES', 'L']);
    // First grows by one, so the second's [10,14) is now [11,15), replaced by one char.
    expect(spans).toEqual([[0, 5], [11, 12]]);
  });
});

describe('FindPainter', () => {
  const set = new Map<string, { ranges: unknown[]; priority: number }>();

  function stubHighlights() {
    class FakeHighlight {
      ranges: unknown[];
      priority = 0;
      constructor(...ranges: unknown[]) {
        this.ranges = ranges;
      }
    }
    vi.stubGlobal('Highlight', FakeHighlight);
    vi.stubGlobal('CSS', { highlights: { set: (k: string, v: { ranges: unknown[]; priority: number }) => set.set(k, v), delete: (k: string) => set.delete(k) } });
  }

  afterEach(() => {
    set.clear();
    vi.unstubAllGlobals();
  });

  it('passes what it painted last time through the transparent gone highlight (gotcha #34)', () => {
    stubHighlights();
    const a = {} as Range;
    const b = {} as Range;
    const p = new FindPainter();
    p.paint([a, b], b);
    expect(set.get(FIND_HL)?.ranges).toEqual([a, b]);
    expect(set.get(FIND_HL_CURRENT)?.ranges).toEqual([b]);
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([]);

    p.paint([b], undefined);
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([a, b]);
    expect(set.get(FIND_HL)?.ranges).toEqual([b]);
    expect(set.get(FIND_HL_CURRENT)?.ranges).toEqual([]);
  });

  it('ranks the current match above the others, both above links', () => {
    stubHighlights();
    new FindPainter().paint([], undefined);
    expect(set.get(FIND_HL)?.priority).toBe(1);
    expect(set.get(FIND_HL_CURRENT)?.priority).toBe(2);
  });

  it('dispose removes all three highlights', () => {
    stubHighlights();
    const p = new FindPainter();
    p.paint([], undefined);
    p.dispose();
    expect(set.size).toBe(0);
  });

  it('does nothing where the Highlight API is missing', () => {
    vi.stubGlobal('Highlight', undefined);
    expect(() => new FindPainter().paint([{} as Range], undefined)).not.toThrow();
  });

  // Every range in a highlight is re-validated by WebKit on EVERY rendering
  // update (Document::updateHighlightPositions — 97% of a frozen webview's
  // main thread, 2026-11-18). Gone needs its ranges for one paint only.
  it('empties gone once the paint that needed it has happened', () => {
    stubHighlights();
    const queued: Array<() => void> = [];
    const a = {} as Range;
    const p = new FindPainter((cb) => queued.push(cb));
    p.paint([a], undefined);
    p.paint([], undefined);
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([a]);
    queued.forEach((cb) => cb());
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([]);
    expect(set.get(FIND_HL_GONE)?.priority).toBe(1);
  });

  it('dispose also cancels an emptying still waiting for its frame', () => {
    stubHighlights();
    const queued: Array<() => void> = [];
    const p = new FindPainter((cb) => queued.push(cb));
    p.paint([{} as Range], undefined);
    p.dispose();
    queued.forEach((cb) => cb());
    expect(set.size).toBe(0);
  });

  it('a later paint keeps its own gone: an earlier scheduled emptying does nothing', () => {
    stubHighlights();
    const queued: Array<() => void> = [];
    const a = {} as Range;
    const b = {} as Range;
    const p = new FindPainter((cb) => queued.push(cb));
    p.paint([a], undefined);
    p.paint([b], undefined); // gone = [a], emptying #2 queued
    const first = queued.shift()!;
    p.paint([], undefined); // gone = [b], emptying #3 queued
    first();
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([b]);
  });

  it('passes the old current match through gone even when it was not in the body list', () => {
    // Only on-screen matches are painted; the current one is painted wherever it is.
    stubHighlights();
    const off = {} as Range;
    const p = new FindPainter(() => {});
    p.paint([], off);
    p.paint([], undefined);
    expect(set.get(FIND_HL_GONE)?.ranges).toEqual([off]);
  });
});

describe('createDebounce', () => {
  afterEach(() => vi.useRealTimers());

  it('runs once, after the typing stops', () => {
    vi.useFakeTimers();
    const run = vi.fn();
    const d = createDebounce(run, FIND_INPUT_DELAY_MS);
    d.call();
    vi.advanceTimersByTime(FIND_INPUT_DELAY_MS - 50);
    d.call();
    vi.advanceTimersByTime(FIND_INPUT_DELAY_MS - 50);
    d.call();
    expect(run).not.toHaveBeenCalled();
    vi.advanceTimersByTime(FIND_INPUT_DELAY_MS);
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('cancel drops the pending run', () => {
    vi.useFakeTimers();
    const run = vi.fn();
    const d = createDebounce(run, FIND_INPUT_DELAY_MS);
    d.call();
    d.cancel();
    vi.advanceTimersByTime(FIND_INPUT_DELAY_MS * 2);
    expect(run).not.toHaveBeenCalled();
  });

  it('waits a short pause, not seconds: Find must still feel immediate', () => {
    expect(FIND_INPUT_DELAY_MS).toBeGreaterThanOrEqual(100);
    expect(FIND_INPUT_DELAY_MS).toBeLessThanOrEqual(300);
  });
});
