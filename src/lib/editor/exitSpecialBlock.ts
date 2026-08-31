// Enter on a genuinely empty line inside a blockquote / heading exits the
// block to a plain paragraph below (Notion/Bear-style: one Enter adds a new
// line within the special block; a second Enter, now on an empty line, exits
// it). Extracted from NoteEditor.svelte so the DOM-boundary logic can be unit
// tested without mounting the whole editor.

const INLINE_TAGS = new Set(['SPAN', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'A', 'CODE', 'FONT']);

function isBlank(s: string): boolean {
  return s.replace(/[ \s]+/g, '') === '';
}

// Text from line-start (previous <br> or block start) up to the cursor.
function textBeforeCaret(node: Node, offset: number): string {
  let acc = '';
  let walker: Node | null;
  if (node.nodeType === 3) {
    acc = (node.textContent || '').slice(0, offset);
    walker = node.previousSibling;
  } else {
    walker = offset > 0 ? (node as Element).childNodes[offset - 1] : null;
  }
  while (walker) {
    if (walker.nodeType === 3) {
      acc = (walker.textContent || '') + acc;
    } else if (walker.nodeType === 1) {
      const tag = (walker as Element).tagName;
      if (tag === 'BR') break;
      if (!INLINE_TAGS.has(tag)) break;
      acc = (walker.textContent || '') + acc;
    } else break;
    walker = walker.previousSibling;
  }
  return acc;
}

// Text from the cursor up to line-end (next <br> or block end) — the mirror
// of textBeforeCaret. A line is only empty when BOTH sides are blank; the
// original implementation only checked the before side, so it misfired for
// Enter pressed at offset 0 of a non-empty heading (cursor has no text
// before it regardless of how much text follows).
function textAfterCaret(node: Node, offset: number): string {
  let acc = '';
  let walker: Node | null;
  if (node.nodeType === 3) {
    acc = (node.textContent || '').slice(offset);
    walker = node.nextSibling;
  } else {
    walker = (node as Element).childNodes[offset] ?? null;
  }
  while (walker) {
    if (walker.nodeType === 3) {
      acc += walker.textContent || '';
    } else if (walker.nodeType === 1) {
      const tag = (walker as Element).tagName;
      if (tag === 'BR') break;
      if (!INLINE_TAGS.has(tag)) break;
      acc += walker.textContent || '';
    } else break;
    walker = walker.nextSibling;
  }
  return acc;
}

// Returns true if the cursor is on an empty line inside a blockquote or
// heading AND we successfully moved the cursor to a fresh plain <div>
// outside the block. Used by Enter to "exit" special-formatted blocks.
export function tryExitSpecialBlock(editorEl: HTMLElement | null): boolean {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return false;
  const range = sel.getRangeAt(0);
  if (!range.collapsed) return false;
  const node: Node = range.startContainer;
  const offset = range.startOffset;
  const el = (node.nodeType === 3 ? node.parentElement : (node as Element)) as Element | null;
  const exitable = el?.closest('blockquote, h1, h2, h3, h4, h5, h6') as HTMLElement | null;
  if (!exitable || !editorEl?.contains(exitable)) return false;

  const before = textBeforeCaret(node, offset);
  const after = textAfterCaret(node, offset);
  // Treat anything non-whitespace (incl. nbsp) as content on either side.
  if (!isBlank(before) || !isBlank(after)) return false;

  const div = document.createElement('div');
  div.appendChild(document.createElement('br'));
  exitable.parentNode!.insertBefore(div, exitable.nextSibling);
  // Clean up the trailing empty <br> left in the exitable block — otherwise
  // there's a visible blank line at the bottom of the heading / quote.
  const last = exitable.lastChild;
  if (last && last.nodeType === 1 && (last as Element).tagName === 'BR') last.remove();
  caretToEnd(div);
  return true;
}

export function caretToEnd(el: HTMLElement) {
  const sel = window.getSelection();
  if (!sel) return;
  const range = document.createRange();
  range.selectNodeContents(el);
  range.collapse(false);
  sel.removeAllRanges();
  sel.addRange(range);
}
