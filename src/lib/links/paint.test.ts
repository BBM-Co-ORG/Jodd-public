// @vitest-environment jsdom
//
// jsdom has neither CSS.highlights nor layout, so both are stubbed: a Map and a
// Highlight class for painting, and hand-made client rects for hit-testing.
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { LINK_HL, LINK_HL_GONE, LINK_HL_PRIORITY, createLinkPainter, hitTest } from './paint';
import type { LinkSpan } from './scan';

class FakeHighlight {
  ranges: Range[];
  /** Unset unless the painter sets it — a real Highlight defaults to 0. */
  priority?: number;
  constructor(...ranges: Range[]) {
    this.ranges = ranges;
  }
}

let highlights: Map<string, FakeHighlight>;
let deleteSpy: (key: string) => void;

beforeEach(() => {
  highlights = new Map();
  deleteSpy = vi.fn() as unknown as (key: string) => void;
  const original = highlights.delete.bind(highlights);
  highlights.delete = ((k: string) => {
    deleteSpy(k);
    return original(k);
  }) as typeof highlights.delete;
  vi.stubGlobal('CSS', { highlights });
  vi.stubGlobal('Highlight', FakeHighlight);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const range = () => document.createRange();

describe('createLinkPainter', () => {
  it('paints the ranges as jodd-link', () => {
    const a = range();
    createLinkPainter().paint([a]);
    expect(highlights.get(LINK_HL)?.ranges).toEqual([a]);
    expect(highlights.get(LINK_HL_GONE)?.ranges).toEqual([]);
  });

  it('paint moves previous ranges into gone', () => {
    const p = createLinkPainter();
    const a = range();
    const b = range();
    p.paint([a]);
    p.paint([b]);
    expect(highlights.get(LINK_HL)?.ranges).toEqual([b]);
    expect(highlights.get(LINK_HL_GONE)?.ranges).toEqual([a]);
  });

  it('clear routes through gone, never delete', () => {
    const p = createLinkPainter();
    const a = range();
    p.paint([a]);
    p.clear();
    expect(highlights.get(LINK_HL)?.ranges).toEqual([]);
    expect(highlights.get(LINK_HL_GONE)?.ranges).toEqual([a]);
    expect(deleteSpy as unknown as ReturnType<typeof vi.fn>).not.toHaveBeenCalled();
    // and the next paint empties gone again — the stale colour has had its one repaint
    p.paint([]);
    expect(highlights.get(LINK_HL_GONE)?.ranges).toEqual([]);
  });

  it('paints below Find: both link highlights carry an explicit priority under find (1)', () => {
    const p = createLinkPainter();
    p.paint([range()]);
    expect(LINK_HL_PRIORITY).toBe(0);
    expect(LINK_HL_PRIORITY).toBeLessThan(1); // Find's jodd-find / jodd-find-gone
    for (const name of [LINK_HL, LINK_HL_GONE]) {
      expect(highlights.get(name)?.priority).toBe(LINK_HL_PRIORITY);
    }
  });

  it('a range present in two consecutive paints ends in jodd-link (registered after gone)', () => {
    const p = createLinkPainter();
    const a = range();
    p.paint([a]);
    p.paint([a]);
    // Equal priority: the highlight registered later paints on top, so the
    // registry order must be GONE then LINK.
    expect([...highlights.keys()]).toEqual([LINK_HL_GONE, LINK_HL]);
    expect(highlights.get(LINK_HL)?.ranges).toEqual([a]);
  });

  it("keeps its own copy of the ranges, not the caller's array", () => {
    const p = createLinkPainter();
    const a = range();
    const b = range();
    const mine = [a];
    p.paint(mine);
    mine.push(b); // the caller reuses its array
    p.paint([]);
    expect(highlights.get(LINK_HL_GONE)?.ranges).toEqual([a]);
  });

  it('is a no-op when CSS.highlights is missing', () => {
    vi.stubGlobal('CSS', {});
    const p = createLinkPainter();
    expect(() => {
      p.paint([range()]);
      p.clear();
    }).not.toThrow();
    expect(highlights.size).toBe(0);
  });

  it('is a no-op when Highlight is missing', () => {
    vi.stubGlobal('Highlight', undefined);
    expect(() => createLinkPainter().paint([range()])).not.toThrow();
    expect(highlights.size).toBe(0);
  });
});

describe('hitTest', () => {
  const span = (start: number, end: number): LinkSpan => ({
    kind: 'url',
    text: 'https://a.co',
    start,
    end,
    insideAnchor: false,
  });
  const rect = (left: number, top: number, right: number, bottom: number) =>
    ({ left, top, right, bottom, width: right - left, height: bottom - top }) as DOMRect;
  /** A Range stand-in: only getClientRects is used by hitTest. */
  const withRects = (...rects: DOMRect[]) => ({ getClientRects: () => rects }) as unknown as Range;

  it('hit inside a rect', () => {
    const s = span(0, 10);
    const r = withRects(rect(0, 0, 50, 20));
    expect(hitTest(25, 10, [s], () => r)).toBe(s);
  });

  it('no hit right of a link at line end', () => {
    const s = span(0, 10);
    const r = withRects(rect(0, 0, 50, 20));
    expect(hitTest(80, 10, [s], () => r)).toBeNull();
  });

  it('checks every rect of a wrapped link and picks the right span', () => {
    const a = span(0, 10);
    const b = span(20, 30);
    const ranges = new Map<LinkSpan, Range>([
      [a, withRects(rect(0, 0, 50, 20), rect(0, 20, 30, 40))],
      [b, withRects(rect(60, 0, 90, 20))],
    ]);
    const toRange = (s: LinkSpan) => ranges.get(s) ?? null;
    expect(hitTest(10, 30, [a, b], toRange)).toBe(a);
    expect(hitTest(70, 10, [a, b], toRange)).toBe(b);
    expect(hitTest(40, 30, [a, b], toRange)).toBeNull();
  });

  it('treats the left/top edge as inside and the right/bottom edge as outside', () => {
    const s = span(0, 10);
    const r = withRects(rect(0, 0, 50, 20));
    expect(hitTest(0, 0, [s], () => r)).toBe(s);
    expect(hitTest(50, 10, [s], () => r)).toBeNull();
    expect(hitTest(25, 20, [s], () => r)).toBeNull();
  });

  it('skips a span with no range', () => {
    expect(hitTest(1, 1, [span(0, 5)], () => null)).toBeNull();
  });

  it('returns null for no spans', () => {
    expect(hitTest(1, 1, [], () => null)).toBeNull();
  });
});
