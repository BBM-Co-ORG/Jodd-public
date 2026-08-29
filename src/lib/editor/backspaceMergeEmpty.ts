// WebKit's native backspace-merge-with-previous-block loses the current
// block's tag (a heading merging backward into an empty line comes out as a
// plain, demoted block) — the mirror-image of the Enter-split quirk in
// splitFirstBlock.ts. Confirmed live: pressing Backspace at the start of a
// heading, to remove a blank line directly above it, silently turns the
// heading into plain-sized text.
//
// Scope: only the "previous sibling is empty" case is handled manually here.
// Merging two blocks that both have real content works correctly via the
// browser's native behavior and is left alone.

function isBlank(s: string): boolean {
  return s.replace(/[ \s]+/g, '') === '';
}

export function tryBackspaceMergeEmptyPrevious(editorEl: HTMLElement | null): boolean {
  if (!editorEl) return false;
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return false;
  const range = sel.getRangeAt(0);
  if (!range.collapsed) return false;

  const node: Node = range.startContainer;
  const offset = range.startOffset;

  // Find the direct child of editorEl that contains the caret.
  let block: Node | null = node;
  while (block && block.parentNode !== editorEl) block = block.parentNode;
  if (!block) return false;

  // Only handle a caret at the block's absolute start.
  const probe = document.createRange();
  probe.setStart(block, 0);
  probe.setEnd(node, offset);
  if (!isBlank(probe.toString())) return false;

  const prev = block.previousSibling;
  if (!prev) return false;
  if (!isBlank(prev.textContent || '')) return false;

  editorEl.removeChild(prev);

  const caret = document.createRange();
  caret.setStart(block, 0);
  caret.collapse(true);
  sel.removeAllRanges();
  sel.addRange(caret);
  return true;
}
