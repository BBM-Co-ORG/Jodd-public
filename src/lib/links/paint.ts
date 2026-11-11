// Painting and hit-testing for links drawn over a note without touching its
// markup: the CSS Custom Highlight API paints, client rects decide what a click
// landed on. Same shape as Find's `paintFindHighlights` in NoteEditor.svelte,
// and for the same reason (CLAUDE.md gotcha #34): WebKit does not repaint a
// range that leaves EVERY highlight, so ranges that stop being links spend one
// paint in the transparent `jodd-link-gone` highlight before they disappear.
// Never "simplify" that to `CSS.highlights.delete`.

import type { LinkSpan } from './scan';

export const LINK_HL = 'jodd-link';
export const LINK_HL_GONE = 'jodd-link-gone';
/**
 * Both link highlights paint UNDER Find's (NoteEditor's `paintFindHighlights`:
 * find and find-gone 1, find-current 2), so a match inside a link still shows.
 * Between the two link highlights the priority is equal on purpose: a range in
 * both paints as whichever was registered later, and `paint` registers GONE
 * before LINK, so a range that is still a link stays a link.
 */
export const LINK_HL_PRIORITY = 0;

const canHighlight = () =>
  typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';

export interface LinkPainter {
  /** Paint these ranges as links; whatever was painted before moves into `LINK_HL_GONE`. */
  paint(ranges: Range[]): void;
  /** Un-paint everything — through `LINK_HL_GONE`, never `delete`. */
  clear(): void;
}

export function createLinkPainter(): LinkPainter {
  let painted: Range[] = [];
  const highlight = (ranges: Range[]) => {
    const h = new Highlight(...ranges);
    h.priority = LINK_HL_PRIORITY;
    return h;
  };
  const paint = (ranges: Range[]) => {
    if (!canHighlight()) return;
    CSS.highlights.set(LINK_HL_GONE, highlight(painted));
    CSS.highlights.set(LINK_HL, highlight(ranges));
    painted = ranges.slice(); // the caller may reuse its array
  };
  return { paint, clear: () => paint([]) };
}

/**
 * The span under the point `(x, y)` (client coordinates), or null. A span is hit
 * only when one of its Range's client rects contains the point: a range's
 * bounding box would claim the empty space to the right of a link that ends a
 * line, and a click there is the caret's, not the link's.
 */
export function hitTest(
  x: number,
  y: number,
  spans: LinkSpan[],
  toRange: (s: LinkSpan) => Range | null,
): LinkSpan | null {
  for (const s of spans) {
    const range = toRange(s);
    if (!range) continue;
    for (const r of Array.from(range.getClientRects())) {
      if (x >= r.left && x < r.right && y >= r.top && y < r.bottom) return s;
    }
  }
  return null;
}
