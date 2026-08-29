// WebKit's native contenteditable paragraph-split misfires specifically for
// the very first block in an editable region: pressing Enter with the caret
// at that block's absolute start should leave a blank block where the caret
// was and move the existing content into a new block below it (the behavior
// it correctly performs for every other block, and for the caret sitting
// mid-block or at the end). Instead it appends an empty block AFTER the
// original, leaving the caret's block untouched — so the content never
// visibly moves. Every other block position already splits correctly via
// the browser's native handling, so this only intervenes for that one
// boundary case.
//
// The "first block" itself isn't always an element: WebKit inserts the very
// first line typed into an empty contenteditable as a bare, unwrapped text
// node directly under the root (no <div>/<p> wrapper), unlike Chromium.
// Confirmed live — see splitFirstBlock.test.ts.

function isBlank(s: string): boolean {
  return s.replace(/[ \s]+/g, '') === '';
}

export function tryFixFirstBlockEnter(editorEl: HTMLElement | null): boolean {
  if (!editorEl) return false;
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return false;
  const range = sel.getRangeAt(0);
  if (!range.collapsed) return false;

  const node: Node = range.startContainer;
  const offset = range.startOffset;

  // Find the direct child of editorEl that contains the caret — may be an
  // element (e.g. <h2>) or a bare text node.
  let block: Node | null = node;
  while (block && block.parentNode !== editorEl) block = block.parentNode;
  if (!block) return false;

  // Every block except the first already splits correctly natively.
  if (block.previousSibling !== null) return false;

  // Only handle a caret at the block's absolute start (not just the start
  // of a line inside a multi-line block).
  const probe = document.createRange();
  probe.setStart(block, 0);
  probe.setEnd(node, offset);
  if (!isBlank(probe.toString())) return false;

  // Nothing to split if the block is already empty.
  if (block.textContent === '') return false;

  // Preserve the block's own tag on the moved content when it's an element
  // (e.g. a heading stays a heading after moving down a line); a bare text
  // node has no tag to preserve, so it moves as plain text inside a <div>.
  const moved = document.createElement(block.nodeType === 1 ? (block as Element).tagName : 'div');
  if (block.nodeType === 1) {
    while (block.firstChild) moved.appendChild(block.firstChild);
  } else {
    moved.appendChild(document.createTextNode(block.textContent || ''));
  }

  const blank = document.createElement(block.nodeType === 1 ? (block as Element).tagName : 'div');
  blank.appendChild(document.createElement('br'));
  editorEl.replaceChild(blank, block);
  editorEl.insertBefore(moved, blank.nextSibling);

  const caret = document.createRange();
  caret.selectNodeContents(moved);
  caret.collapse(true);
  sel.removeAllRanges();
  sel.addRange(caret);
  return true;
}
