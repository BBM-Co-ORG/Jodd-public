// What the editor's CSS highlights may hold: the ranges on screen, never all
// of them.
//
// WebKit re-validates EVERY range in `CSS.highlights` on EVERY rendering
// update (Document::updateHighlightPositions), whether or not it is visible
// and whether or not it changed — a caret blink is enough. Each check walks
// character clusters over the range's whole text node. On 2026-11-18 Find in
// an ingested note (one 23,000-character <pre> transcript, a one-letter query)
// put 97% of the webview's main thread there and froze Jodd. Painting only
// what the user can see bounds that cost whatever the note holds.
//
// Used by Find (NoteEditor `paintVisibleHighlights`) and by the link painter.

export interface Box {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

/** Most ranges one highlight may hold — more than a screen ever shows. */
export const MAX_PAINTED = 200;
/** Most ranges measured per pass: a measurement lays out text too. */
export const MAX_MEASURED = 600;

/** getBoundingClientRect of something not rendered (closed <details>, display:none). */
const notRendered = (b: Box) => b.top === 0 && b.bottom === 0 && b.left === 0 && b.right === 0;

export interface VisibleSliceOptions {
  /** Distance beyond each edge of the view that still counts; default half the view's height. */
  margin?: number;
  maxPainted?: number;
  maxMeasured?: number;
}

/**
 * The items whose box meets `view` (plus the margin), in order.
 *
 * `items` are in document order, so their boxes run down the page: a binary
 * search finds the first one not wholly above the view, then a walk collects
 * until one starts below it. Matches on a visible line but left or right of
 * the view (an unwrapped <pre> line scrolls sideways) are skipped, not
 * painted. Both caps hold whatever the layout: a pathological note paints
 * fewer highlights, never freezes.
 */
export function visibleSlice<T>(items: readonly T[], boxOf: (item: T) => Box, view: Box, opts: VisibleSliceOptions = {}): T[] {
  const margin = opts.margin ?? (view.bottom - view.top) / 2;
  const maxPainted = opts.maxPainted ?? MAX_PAINTED;
  const maxMeasured = opts.maxMeasured ?? MAX_MEASURED;
  // A view with no size has no layout to measure against (not attached yet,
  // or a DOM without layout): paint in document order, still capped.
  if (view.bottom <= view.top || view.right <= view.left) return items.slice(0, maxPainted);
  const top = view.top - margin;
  const bottom = view.bottom + margin;
  const left = view.left - margin;
  const right = view.right + margin;

  let measured = 0;
  const measure = (i: number) => {
    measured++;
    return boxOf(items[i]);
  };

  // First item not wholly above the window. A box that is not rendered could
  // sit anywhere; counting it as "not above" can only start the walk early.
  let lo = 0;
  let hi = items.length;
  while (lo < hi) {
    const mid = (lo + hi) >>> 1;
    const b = measure(mid);
    if (!notRendered(b) && b.bottom < top) lo = mid + 1;
    else hi = mid;
  }

  const out: T[] = [];
  for (let i = lo; i < items.length && out.length < maxPainted && measured < maxMeasured; i++) {
    const b = measure(i);
    if (notRendered(b)) continue;
    if (b.top > bottom) break;
    if (b.bottom < top || b.right < left || b.left > right) continue;
    out.push(items[i]);
  }
  return out;
}

/**
 * Run `cb` once the browser has painted the current state: the second
 * animation frame from now comes after the rendering update of the first.
 * Where there are no animation frames (tests without a DOM) it never runs.
 */
export function afterNextPaint(cb: () => void): void {
  if (typeof requestAnimationFrame === 'undefined') return;
  requestAnimationFrame(() => requestAnimationFrame(cb));
}
