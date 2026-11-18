import { describe, expect, it } from 'vitest';
import { anchorPosition, caretPanelPosition, ANCHOR_GAP, VIEWPORT_MARGIN } from './anchorPosition';

const rect = (left: number, top: number, width: number, height: number) =>
  ({ left, top, width, height, right: left + width, bottom: top + height }) as DOMRect;
const view = { width: 800, height: 600 };
const pop = { width: 200, height: 100 };

describe('anchorPosition', () => {
  it('bottom-start aligns the left edges, below the anchor', () => {
    expect(anchorPosition(rect(100, 50, 40, 20), pop, view, 'bottom-start')).toEqual({
      left: 100,
      top: 70 + ANCHOR_GAP,
    });
  });

  it('bottom-end aligns the right edges', () => {
    expect(anchorPosition(rect(500, 50, 40, 20), pop, view, 'bottom-end')).toEqual({
      left: 540 - 200,
      top: 70 + ANCHOR_GAP,
    });
  });

  it('clamps horizontally inside the viewport margin', () => {
    expect(anchorPosition(rect(750, 50, 40, 20), pop, view, 'bottom-start').left).toBe(
      800 - 200 - VIEWPORT_MARGIN,
    );
    expect(anchorPosition(rect(0, 50, 20, 20), pop, view, 'bottom-end').left).toBe(VIEWPORT_MARGIN);
  });

  it('flips above the anchor when it would overflow the bottom and there is room', () => {
    const p = anchorPosition(rect(100, 540, 40, 20), pop, view, 'bottom-start');
    expect(p.top).toBe(540 - ANCHOR_GAP - 100);
  });

  it('stays below and clamps when neither side fits', () => {
    const tall = { width: 200, height: 580 };
    const p = anchorPosition(rect(100, 300, 40, 20), tall, view, 'bottom-start');
    expect(p.top).toBe(Math.max(VIEWPORT_MARGIN, 600 - 580 - VIEWPORT_MARGIN));
    expect(p.top).toBeGreaterThanOrEqual(VIEWPORT_MARGIN);
  });

  it('never returns a negative position when the popover is wider than the viewport', () => {
    const wide = { width: 900, height: 100 };
    expect(anchorPosition(rect(100, 50, 40, 20), wide, view, 'bottom-start').left).toBe(VIEWPORT_MARGIN);
  });

  it('right-start sits beside the anchor, top edges aligned (a submenu)', () => {
    expect(anchorPosition(rect(100, 50, 40, 20), pop, view, 'right-start')).toEqual({
      left: 140 + ANCHOR_GAP,
      top: 50,
    });
  });

  it('right-start flips to the left of the anchor when the right would overflow', () => {
    expect(anchorPosition(rect(650, 50, 40, 20), pop, view, 'right-start').left).toBe(650 - ANCHOR_GAP - 200);
  });

  it('right-start clamps its top inside the viewport', () => {
    expect(anchorPosition(rect(100, 580, 40, 20), pop, view, 'right-start').top).toBe(600 - 100 - VIEWPORT_MARGIN);
  });
});

// Task 6.1 fix round 1: the `[[` picker (and Task 6.3's HashtagPicker) at
// the caret. On a phone the bottom of the viewport is the format bar plus any
// keyboard overlap (`bottomInset`), which the panel must not go under.
describe('caretPanelPosition', () => {
  const phone = { width: 360, height: 740 };
  const panel = { width: 340, maxHeight: 240 };
  const caret = (left: number, top: number, height = 20) => ({ left, top, bottom: top + height });

  it('goes below the caret when it fits above the bottom inset', () => {
    expect(caretPanelPosition(caret(40, 100), panel, phone, 48)).toEqual({
      left: 360 - 340 - VIEWPORT_MARGIN,
      top: 120 + ANCHOR_GAP,
      maxHeight: 240,
    });
  });

  it('caret near the bottom: above the caret, anchored by its bottom edge so a short list hugs the caret', () => {
    const p = caretPanelPosition(caret(40, 600), panel, phone, 48);
    expect(p.top).toBeUndefined();
    expect(p.bottom).toBe(740 - (600 - ANCHOR_GAP));
    expect(p.maxHeight).toBe(240);
  });

  it('the bottom inset counts: what fits with no inset flips with the bar and keyboard', () => {
    const c = caret(40, 300);
    expect(caretPanelPosition(c, panel, phone, 0).top).toBe(320 + ANCHOR_GAP);
    // 48px bar + 300px keyboard overlap: 740 - 348 - 8 = 384 is the visible
    // bottom, leaving 60px below the caret and 288px above it.
    const p = caretPanelPosition(c, panel, phone, 348);
    expect(p.top).toBeUndefined();
    expect(p.bottom).toBe(740 - (300 - ANCHOR_GAP));
  });

  it('clamps the height to the room on the roomier side when neither fits', () => {
    const short = { width: 360, height: 300 };
    // Visible 8..(300-48-8=244); caret at 150..170: 70 below, 138 above.
    const p = caretPanelPosition(caret(10, 150), panel, short, 48);
    expect(p.bottom).toBe(300 - (150 - ANCHOR_GAP));
    expect(p.maxHeight).toBe(150 - ANCHOR_GAP - VIEWPORT_MARGIN);
    // Caret high up: below has more room.
    const q = caretPanelPosition(caret(10, 30), panel, short, 48);
    expect(q.top).toBe(50 + ANCHOR_GAP);
    expect(q.maxHeight).toBe(244 - 54);
  });

  it('a caret hidden under the bar still puts the panel above the bar', () => {
    const p = caretPanelPosition(caret(10, 720), panel, phone, 48);
    // Anchored no lower than the visible bottom (740 - 48 - 8 = 684).
    expect(p.bottom).toBe(740 - 684);
    expect(740 - p.bottom!).toBeLessThanOrEqual(740 - 48);
  });

  it('clamps horizontally, narrowing the panel to the viewport', () => {
    expect(caretPanelPosition(caret(0, 100), panel, phone, 0).left).toBe(VIEWPORT_MARGIN);
    const tiny = { width: 300, height: 740 };
    expect(caretPanelPosition(caret(200, 100), panel, tiny, 0).left).toBe(VIEWPORT_MARGIN);
  });
});
