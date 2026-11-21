import { describe, expect, it } from 'vitest';
import { visibleSlice, type Box } from './visibleRanges';

// Items stand for highlight ranges in document order; `rectOf` is what
// Range.getBoundingClientRect would report. The view is the editor's client
// rect, margin 0 unless a test says otherwise.
const view: Box = { top: 100, bottom: 300, left: 0, right: 400 };

/** One item per line, 20px tall, starting at y = 0. */
function lines(n: number): Box[] {
  return Array.from({ length: n }, (_, i) => ({ top: i * 20, bottom: i * 20 + 16, left: 10, right: 60 }));
}

describe('visibleSlice', () => {
  it('keeps only the items whose box meets the view', () => {
    const items = lines(50);
    const out = visibleSlice(items, (b) => b, view, { margin: 0 });
    expect(out.map((b) => b.top)).toEqual([100, 120, 140, 160, 180, 200, 220, 240, 260, 280, 300]);
  });

  it('measures a logarithmic number of items before the view, not all of them', () => {
    // 2,000 matches above the view: a linear walk would measure every one.
    const items = lines(2_100);
    let measured = 0;
    const far: Box = { top: 40_000, bottom: 40_200, left: 0, right: 400 };
    const out = visibleSlice(items, (b) => { measured++; return b; }, far, { margin: 0 });
    expect(out.length).toBe(11);
    expect(measured).toBeLessThan(11 + 2 * 12);
  });

  it('skips matches on a visible line that sit left or right of the view', () => {
    // One long unwrapped <pre> line: every match has the same top.
    const items: Box[] = Array.from({ length: 100 }, (_, i) => ({ top: 150, bottom: 166, left: i * 50, right: i * 50 + 40 }));
    const out = visibleSlice(items, (b) => b, view, { margin: 0 });
    expect(out.map((b) => b.left)).toEqual([0, 50, 100, 150, 200, 250, 300, 350, 400]);
  });

  it('never returns more than maxPainted items', () => {
    const items: Box[] = Array.from({ length: 5_000 }, () => ({ top: 150, bottom: 166, left: 10, right: 20 }));
    expect(visibleSlice(items, (b) => b, view, { margin: 0, maxPainted: 200 }).length).toBe(200);
  });

  it('stops measuring after maxMeasured items even if few were visible', () => {
    // A horizontally scrolled long line: thousands of matches left of the view.
    const items: Box[] = Array.from({ length: 5_000 }, (_, i) => ({ top: 150, bottom: 166, left: i * 50 - 200_000, right: i * 50 - 199_960 }));
    let measured = 0;
    visibleSlice(items, (b) => { measured++; return b; }, view, { margin: 0, maxMeasured: 600 });
    expect(measured).toBeLessThanOrEqual(600);
  });

  it('treats an all-zero box (not rendered, e.g. inside a closed <details>) as invisible', () => {
    const hidden: Box = { top: 0, bottom: 0, left: 0, right: 0 };
    const shown: Box = { top: 150, bottom: 166, left: 10, right: 60 };
    const out = visibleSlice([shown, hidden, hidden], (b) => b, { top: -50, bottom: 300, left: -50, right: 400 }, { margin: 0 });
    expect(out).toEqual([shown]);
  });

  it('defaults the margin to half the view height, so a short scroll finds ranges already painted', () => {
    const items = lines(50);
    const out = visibleSlice(items, (b) => b, view);
    // view 100..300, margin 100 → 0..400
    expect(out[0].top).toBe(0);
    expect(out[out.length - 1].top).toBe(400);
  });

  it('without layout (a view with no size) paints in order up to the cap, measuring nothing', () => {
    let measured = 0;
    const items = lines(500);
    const none: Box = { top: 0, bottom: 0, left: 0, right: 0 };
    const out = visibleSlice(items, (b) => { measured++; return b; }, none, { maxPainted: 200 });
    expect(out).toEqual(items.slice(0, 200));
    expect(measured).toBe(0);
  });

  it('returns nothing for no items', () => {
    expect(visibleSlice([], (b: Box) => b, view)).toEqual([]);
  });
});
