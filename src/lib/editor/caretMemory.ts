// Where the caret was in the note body, as a line and a column plus the
// scroll position, so focus can go back to the same place after a notice
// action or a dialog took it (re-critique after #295: "Keep my edits" and
// "Discard my edits" put the caret on line 1 and scrolled to the top, because
// focusing a contenteditable without a selection inside it starts at its
// beginning).
//
// A line and a column, not a Range: a save or a reload re-renders the body
// ({#key}), and a Range into the old nodes would point at nothing (the find
// bar keeps offsets for the same reason, gotcha #34). And not a character
// count from the start: after "Discard my edits" the body holds the other
// device's text, where the same count lands on an unrelated line, off screen
// (re-critique after #296). The same line and column is the nearest place.

interface Point {
  /** Index of the body's top-level node (a line) holding the point. */
  line: number;
  /** Characters into that line. */
  col: number;
}

export interface CaretMark {
  start: Point;
  end: Point;
  scrollTop: number;
}

function textLength(node: Node, upTo: Node, offset: number): number {
  const r = document.createRange();
  r.selectNodeContents(node);
  r.setEnd(upTo, offset);
  return r.toString().length;
}

function pointOf(root: HTMLElement, node: Node, offset: number): Point {
  if (node === root) {
    // Between top-level nodes: the start of the line at `offset`.
    return { line: Math.max(0, Math.min(offset, root.childNodes.length - 1)), col: 0 };
  }
  let top: Node = node;
  while (top.parentNode && top.parentNode !== root) top = top.parentNode;
  const line = Array.prototype.indexOf.call(root.childNodes, top) as number;
  return { line, col: textLength(top, node, offset) };
}

/** The selection's place in `root`, or null when it is not inside it. */
export function markCaret(root: HTMLElement): CaretMark | null {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const r = sel.getRangeAt(0);
  if (!root.contains(r.startContainer) || !root.contains(r.endContainer)) return null;
  return {
    start: pointOf(root, r.startContainer, r.startOffset),
    end: pointOf(root, r.endContainer, r.endOffset),
    scrollTop: root.scrollTop,
  };
}

/** The node and offset at `p`, clamped to the lines and text there now. */
function locate(root: HTMLElement, p: Point): { node: Node; offset: number } {
  const lines = root.childNodes;
  if (lines.length === 0) return { node: root, offset: 0 };
  const line = lines[Math.min(p.line, lines.length - 1)];
  if (line.nodeType === Node.TEXT_NODE) {
    return { node: line, offset: Math.min(p.col, (line as Text).data.length) };
  }
  const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
  let seen = 0;
  let last: Text | null = null;
  for (let t = walker.nextNode() as Text | null; t; t = walker.nextNode() as Text | null) {
    if (seen + t.data.length >= p.col) return { node: t, offset: p.col - seen };
    seen += t.data.length;
    last = t;
  }
  return last ? { node: last, offset: last.data.length } : { node: line, offset: 0 };
}

/** Focus `root` with the caret back at `mark` (the same line and column,
 * clamped to the text it now holds) and its scroll position restored,
 * without the browser scrolling to the top. */
export function restoreCaret(root: HTMLElement, mark: CaretMark): void {
  root.focus({ preventScroll: true });
  const sel = window.getSelection();
  if (sel) {
    const a = locate(root, mark.start);
    const same = mark.end.line === mark.start.line && mark.end.col === mark.start.col;
    const b = same ? a : locate(root, mark.end);
    const r = document.createRange();
    r.setStart(a.node, a.offset);
    r.setEnd(b.node, b.offset);
    sel.removeAllRanges();
    sel.addRange(r);
  }
  root.scrollTop = mark.scrollTop;
}
