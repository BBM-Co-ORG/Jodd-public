// Backspace at the start of a block whose previous line is empty: remove the
// empty line and leave the block alone. WebKit's own merge keeps the FIRST
// paragraph's block, so a heading merging backward into a blank line comes out
// as plain text — re-measured 2026-09-14 in WKWebView with real key events:
// native Backspace turns <div><br></div><h2>Heading</h2> into <div>Heading</div>.
//
// This used to removeChild the empty line, which WebKit's undo history never
// saw: Cmd+Z could not bring the blank line back. Both paths below change the
// DOM only through execCommand (see markdownTriggers.ts for why), and each was
// measured to keep the heading and to undo/redo coherently:
//
// - A block exists above the empty line: delete from the END of that block to
//   the end of the empty line. The merge then keeps the block above, and the
//   current block is never part of it — its tag and attributes survive
//   (measured with an &nbsp;-only line, a <ul> above, a styled <h2>).
// - No block above, or it is an image, a rule or a table: re-tag the empty
//   line to the heading's tag with formatBlock, so the merge that follows
//   keeps the right tag, and never touch the block above. Headings only —
//   with nothing above, native Backspace already keeps <blockquote> and <ul>
//   intact (measured), and formatBlock cannot produce a list anyway.
//
// Merging two blocks that both have real content is left to the browser.

const EMBEDDED = 'img, input, object, embed, video, audio, iframe, hr, table, canvas, svg';
const HEADINGS = new Set(['H1', 'H2', 'H3', 'H4', 'H5', 'H6']);

function isBlank(s: string): boolean {
  return s.replace(/[ \s]+/g, '') === '';
}

// Whitespace text between blocks (serialised HTML is full of "\n") and
// comments render as nothing; they are not the "previous line".
function isInvisible(n: Node): boolean {
  return n.nodeType === 8 || (n.nodeType === 3 && /^[\t\n\r ]*$/.test(n.textContent || ''));
}

function previousVisible(n: Node): Node | null {
  let p = n.previousSibling;
  while (p && isInvisible(p)) p = p.previousSibling;
  return p;
}

export type BackspaceMergePlan =
  | { kind: 'deleteFromAbove'; block: Node; empty: Element; above: Node }
  | { kind: 'retagEmpty'; block: Element; empty: Element };

// What tryBackspaceMergeEmptyPrevious would do for the current caret, or null
// when Backspace should be left to the browser. Reads the DOM, never changes it.
export function planBackspaceMergeEmpty(editorEl: HTMLElement | null): BackspaceMergePlan | null {
  if (!editorEl) return null;
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!range.collapsed) return null;

  const node: Node = range.startContainer;
  const offset = range.startOffset;

  // Find the direct child of editorEl that contains the caret.
  let block: Node | null = node;
  while (block && block.parentNode !== editorEl) block = block.parentNode;
  if (!block) return null;

  // Only handle a caret at the block's absolute start.
  const probe = document.createRange();
  probe.setStart(block, 0);
  probe.setEnd(node, offset);
  if (!isBlank(probe.toString())) return null;

  const prev = previousVisible(block);
  if (!prev || prev.nodeType !== 1) return null;
  const empty = prev as Element;
  // An image or a checkbox has no text either — that line is not empty.
  if (!isBlank(empty.textContent || '') || empty.querySelector(EMBEDDED)) return null;

  // Deleting from the end of the block above needs that block to have an end
  // a caret can sit at. An image, a rule or a table directly under the editor
  // has no inside: WebKit moves the range start to before it, and the delete
  // takes it too (measured: an <img> and an <hr> above were deleted).
  const above = previousVisible(empty);
  if (above && !(above.nodeType === 1 && (above as Element).matches(EMBEDDED))) {
    return { kind: 'deleteFromAbove', block, empty, above };
  }
  if (block.nodeType === 1 && HEADINGS.has((block as Element).tagName)) {
    return { kind: 'retagEmpty', block: block as Element, empty };
  }
  return null;
}

function select(range: Range) {
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(range);
}

export function tryBackspaceMergeEmptyPrevious(editorEl: HTMLElement | null): boolean {
  const plan = planBackspaceMergeEmpty(editorEl);
  if (!plan) return false;

  if (plan.kind === 'deleteFromAbove') {
    const r = document.createRange();
    r.selectNodeContents(plan.above);
    r.collapse(false);
    r.setEnd(plan.empty, plan.empty.childNodes.length);
    select(r);
    document.execCommand('delete');
    // The delete leaves the caret at the end of the block above; the user
    // pressed Backspace at the start of this one.
    const caret = document.createRange();
    caret.setStart(plan.block, 0);
    caret.collapse(true);
    select(caret);
  } else {
    const inEmpty = document.createRange();
    inEmpty.selectNodeContents(plan.empty);
    inEmpty.collapse(true);
    select(inEmpty);
    document.execCommand('formatBlock', false, `<${plan.block.tagName.toLowerCase()}>`);
    const caret = document.createRange();
    caret.setStart(plan.block, 0);
    caret.collapse(true);
    select(caret);
    document.execCommand('delete');
  }
  return true;
}
