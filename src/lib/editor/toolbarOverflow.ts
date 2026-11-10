// How many of the editor toolbar's formatting buttons do not fit on its single
// row (Task 6.4, spec 2026-10-20 §5.6 last bullet). Those move — from the
// right, so the left of the row never reshuffles — into the "More formatting"
// popover. Pure: NoteEditor measures and applies.
//
// `available` is the toolbar's inner width, `widths` each button's width
// (including the gap after it), and `reserved` the right side (save state and
// ⋯) plus the gap before it, which never overflows.

/** Buttons to move out of the row; always a suffix of `widths`. */
export function overflowCount(available: number, widths: readonly number[], reserved: number): number {
  // A hidden or detached toolbar measures 0 or NaN; hiding every button on
  // that reading would be wrong the moment it is shown again.
  if (!Number.isFinite(available) || !Number.isFinite(reserved)) return 0;
  let room = available - reserved;
  let fit = 0;
  for (const w of widths) {
    if (w > room) break;
    room -= w;
    fit++;
  }
  return widths.length - fit;
}

/**
 * `overflowCount` plus the "More formatting" button's own width: it exists
 * only when something overflows, so the first pass is without it and the
 * second, only if needed, makes room for it.
 */
export function overflowWithMore(
  available: number,
  widths: readonly number[],
  reserved: number,
  moreWidth: number,
): { count: number; more: boolean } {
  const without = overflowCount(available, widths, reserved);
  if (without === 0) return { count: 0, more: false };
  return { count: overflowCount(available, widths, reserved + moreWidth), more: true };
}
