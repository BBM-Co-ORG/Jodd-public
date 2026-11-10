// Where an anchored, non-modal panel (Popover) goes: below its anchor, edge
// aligned per `placement`, flipped above when the bottom would overflow, and
// clamped so it never leaves the viewport. `right-start` is a submenu's
// placement instead: beside the anchor, top edges aligned, flipped to the
// anchor's left when the right side would overflow. Pure: callers measure and
// apply.
// `caretPanelPosition` (below) is the caret-anchored variant: the editor's
// `[[` picker on a phone, and Task 6.3's #-autocomplete.

export type AnchorPlacement = 'bottom-start' | 'bottom-end' | 'right-start';

export const ANCHOR_GAP = 4;
export const VIEWPORT_MARGIN = 8;

export function anchorPosition(
  anchorRect: DOMRect,
  popRect: { width: number; height: number },
  viewport: { width: number; height: number },
  placement: AnchorPlacement,
): { left: number; top: number } {
  if (placement === 'right-start') {
    const right = anchorRect.right + ANCHOR_GAP;
    const leftSide = anchorRect.left - ANCHOR_GAP - popRect.width;
    const fitsRight = right + popRect.width <= viewport.width - VIEWPORT_MARGIN;
    const wantLeft = fitsRight || leftSide < VIEWPORT_MARGIN ? right : leftSide;
    const maxLeft = viewport.width - popRect.width - VIEWPORT_MARGIN;
    const maxTop = viewport.height - popRect.height - VIEWPORT_MARGIN;
    return {
      left: Math.max(VIEWPORT_MARGIN, Math.min(wantLeft, maxLeft)),
      top: Math.max(VIEWPORT_MARGIN, Math.min(anchorRect.top, maxTop)),
    };
  }
  const wantLeft = placement === 'bottom-end' ? anchorRect.right - popRect.width : anchorRect.left;
  const maxLeft = viewport.width - popRect.width - VIEWPORT_MARGIN;
  const left = Math.max(VIEWPORT_MARGIN, Math.min(wantLeft, maxLeft));

  const below = anchorRect.bottom + ANCHOR_GAP;
  const above = anchorRect.top - ANCHOR_GAP - popRect.height;
  const fitsBelow = below + popRect.height <= viewport.height - VIEWPORT_MARGIN;
  const fitsAbove = above >= VIEWPORT_MARGIN;
  const wantTop = fitsBelow || !fitsAbove ? below : above;
  const maxTop = viewport.height - popRect.height - VIEWPORT_MARGIN;
  const top = Math.max(VIEWPORT_MARGIN, Math.min(wantTop, maxTop));

  return { left, top };
}

/**
 * Where a list anchored at the text caret goes (the `[[` picker; Task 6.3's
 * HashtagPicker): below the caret when its full `maxHeight` fits above the
 * visible bottom, otherwise on whichever side has more room — above is
 * anchored by its BOTTOM edge, so a short list still sits against the caret
 * without measuring it first. `bottomInset` is what covers the bottom of the
 * viewport: on a phone, the format bar plus the keyboard's overlap
 * (`--kb-inset`). The height is clamped to the room on the chosen side, and
 * the left edge so a panel `min(width, viewport − 2 × margin)` wide stays in.
 */
export function caretPanelPosition(
  caret: { left: number; top: number; bottom: number },
  panel: { width: number; maxHeight: number },
  viewport: { width: number; height: number },
  bottomInset = 0,
): { left: number; top?: number; bottom?: number; maxHeight: number } {
  const width = Math.min(panel.width, viewport.width - 2 * VIEWPORT_MARGIN);
  const maxLeft = viewport.width - width - VIEWPORT_MARGIN;
  const left = Math.max(VIEWPORT_MARGIN, Math.min(caret.left, maxLeft));

  const visibleBottom = viewport.height - bottomInset - VIEWPORT_MARGIN;
  const belowTop = caret.bottom + ANCHOR_GAP;
  const roomBelow = visibleBottom - belowTop;
  // A caret already under the bar (it should not be — scroll-padding keeps it
  // above) still gets a panel above the bar.
  const aboveBottom = Math.min(caret.top - ANCHOR_GAP, visibleBottom);
  const roomAbove = aboveBottom - VIEWPORT_MARGIN;

  if (roomBelow >= panel.maxHeight || roomBelow >= roomAbove) {
    return { left, top: belowTop, maxHeight: Math.max(0, Math.min(panel.maxHeight, roomBelow)) };
  }
  return {
    left,
    bottom: viewport.height - aboveBottom,
    maxHeight: Math.max(0, Math.min(panel.maxHeight, roomAbove)),
  };
}
