// Enter on an empty last line inside a blockquote / heading exits the block
// to a plain paragraph (Notion/Bear-style: one Enter adds a new line within
// the special block; a second Enter, now on an empty line, exits it).
// Extracted from NoteEditor.svelte so the decision can be unit tested without
// mounting the whole editor.
//
// The exit changes the DOM only through execCommand. WebKit's undo history is
// a list of edit commands, not DOM snapshots (see markdownTriggers.ts); the
// insertBefore + remove this used to do was never in that history. Measured
// 2026-09-14 in WKWebView with real key events: after exiting a quote,
// Cmd+Z then Cmd+Shift+Z re-inserted the quote BELOW the paragraph that had
// replaced it and put the next typed character in the wrong block.
//
// Which command depends on where the empty line sits — each one measured:
//
//   <h2><br></h2>, <blockquote><br></blockquote>
//       formatBlock <div>: the whole block is the empty line, convert it.
//   <blockquote>text<br><br></blockquote>
//       delete + insertParagraph + formatBlock <div>: drop the empty line,
//       open a block after this one, make it plain.
//   <blockquote><div>text</div><div><br></div></blockquote>  (Apple / paste)
//       outdent. formatBlock only re-tags the inner <div> and leaves the quote
//       around it — Enter would do nothing. (outdent on the flat shapes above
//       leaves a bare <br> at the root, so it is not used for them.)
//
// Only the LAST line exits. An empty line in the middle of a quote is left to
// native Enter; the old code jumped the caret past the rest of the quote.

const INLINE_TAGS = new Set(['SPAN', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'A', 'CODE', 'FONT']);
const EMBEDDED = 'img, input, object, embed, video, audio, iframe, hr, table';

function isBlank(s: string): boolean {
  return s.replace(/[ \s]+/g, '') === '';
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

export type ExitCommand = 'formatBlock' | 'splitThenFormat' | 'outdent';

// What tryExitSpecialBlock would run for the current caret, or null when
// Enter should be left to the browser. Reads the DOM, never changes it.
export function planExitSpecialBlock(editorEl: HTMLElement | null): ExitCommand | null {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const range = sel.getRangeAt(0);
  if (!range.collapsed) return null;
  const node: Node = range.startContainer;
  const offset = range.startOffset;
  const el = (node.nodeType === 3 ? node.parentElement : (node as Element)) as Element | null;
  const exitable = el?.closest('blockquote, h1, h2, h3, h4, h5, h6') as HTMLElement | null;
  if (!exitable || !editorEl?.contains(exitable)) return null;

  // Treat anything non-whitespace (incl. nbsp) as content on either side.
  if (!isBlank(textBeforeCaret(node, offset)) || !isBlank(textAfterCaret(node, offset))) return null;

  // The last line: after the caret there is nothing but the empty line's own
  // placeholder <br>.
  const rest = document.createRange();
  rest.setStart(node, offset);
  rest.setEnd(exitable, exitable.childNodes.length);
  const tail = rest.cloneContents();
  if (!isBlank(tail.textContent || '') || tail.querySelectorAll('br').length > 1 || tail.querySelector(EMBEDDED)) {
    return null;
  }

  const line = el!.closest('div, p, li, blockquote, h1, h2, h3, h4, h5, h6');
  if (line === exitable) {
    const wholeBlockEmpty =
      isBlank(exitable.textContent || '') &&
      exitable.querySelectorAll('br').length <= 1 &&
      !exitable.querySelector(EMBEDDED);
    return wholeBlockEmpty ? 'formatBlock' : 'splitThenFormat';
  }
  if (exitable.tagName === 'BLOCKQUOTE' && line?.parentElement === exitable && (line.tagName === 'DIV' || line.tagName === 'P')) {
    return 'outdent';
  }
  return null;
}

// Returns true if the caret was on the empty last line of a blockquote or
// heading and that line has been turned into a plain paragraph outside it.
export function tryExitSpecialBlock(editorEl: HTMLElement | null): boolean {
  const plan = planExitSpecialBlock(editorEl);
  if (!plan) return false;
  if (plan === 'formatBlock') {
    document.execCommand('formatBlock', false, '<div>');
  } else if (plan === 'outdent') {
    document.execCommand('outdent');
  } else {
    document.execCommand('delete');
    document.execCommand('insertParagraph');
    document.execCommand('formatBlock', false, '<div>');
  }
  return true;
}
