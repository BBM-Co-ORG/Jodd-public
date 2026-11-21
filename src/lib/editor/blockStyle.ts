// Text style (Title / Heading / Subheading / Body) applied one line at a time.
//
// WebKit's formatBlock over a selection of several lines wraps them ALL in
// one new block and turns the boundaries between them into <br>: three body
// lines made Heading came out as <h2>A<br>B<br>C</h2>, and three <p> lines
// made Body as <div>A<br>B<br>C</div> — one paragraph to Notes.app, which
// styles a paragraph, not a line. Measured in WKWebView 2026-11-17.
//
// And on a line inside such a block (one paragraph whose lines are split by
// <br>), formatBlock at a caret nests the new block in the old one:
// <div>A<br><h2>B</h2>C</div>. So first every <br> that separates a line the
// style reaches from the next is replaced by a paragraph break
// (insertParagraph on the selected <br>: <div>A</div><div>B</div>…, same tag,
// same look), then each line gets formatBlock at a caret on its own.
//
// Every step is an execCommand, never a DOM edit behind WebKit's back, so the
// undo history stays in step (markdownTriggers.ts); the price is that undo
// takes it back one step at a time.

export interface Point {
  node: Node;
  offset: number;
}

/** One line as WebKit counts paragraphs: a block's content, or the part of
 *  it between two <br>s. */
export interface EditorLine {
  start: Point;
  end: Point;
  /** The <br> that ends this line and starts another in the same block —
   *  not the placeholder <br> a block keeps after its last line. */
  endBr: HTMLBRElement | null;
}

const BLOCK = new Set([
  'ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'DD', 'DETAILS', 'DIV', 'DL', 'DT',
  'FIELDSET', 'FIGCAPTION', 'FIGURE', 'FOOTER', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
  'HEADER', 'LI', 'MAIN', 'NAV', 'OL', 'P', 'PRE', 'SECTION', 'SUMMARY', 'TABLE',
  'TBODY', 'TFOOT', 'THEAD', 'TR', 'UL',
]);
const SKIP = new Set(['STYLE', 'SCRIPT', 'TEMPLATE', 'HEAD', 'TITLE', 'META', 'LINK']);
const REPLACED = new Set(['IMG', 'VIDEO', 'AUDIO', 'CANVAS', 'SVG', 'IFRAME', 'OBJECT', 'EMBED', 'INPUT', 'HR']);
// Collapsible whitespace only: a no-break space is content (`<div>&nbsp;</div>`).
const SHOWS = /[^ \t\n\r\f]/;

function indexOf(n: Node): number {
  return Array.prototype.indexOf.call(n.parentNode!.childNodes, n);
}

/** The editor's lines, in document order. */
export function editorLines(root: Element): EditorLine[] {
  const out: EditorLine[] = [];
  let cur: EditorLine | null = null;
  // A <br> that ended a line: a separator if another line starts after it in
  // the same block, the block's placeholder if the block ends first.
  let pendingBr: { br: HTMLBRElement; line: EditorLine } | null = null;
  const begin = (start: Point, end: Point): void => {
    if (cur) {
      cur.end = end;
      return;
    }
    if (pendingBr) pendingBr.line.endBr = pendingBr.br;
    pendingBr = null;
    cur = { start, end, endBr: null };
  };
  const close = (): EditorLine | null => {
    const done = cur;
    if (done) out.push(done);
    cur = null;
    return done;
  };
  const visit = (n: Node): void => {
    if (n.nodeType === Node.TEXT_NODE) {
      const t = n as Text;
      if (!t.data.length || (!cur && !SHOWS.test(t.data))) return;
      begin({ node: t, offset: 0 }, { node: t, offset: t.data.length });
      return;
    }
    if (n.nodeType !== Node.ELEMENT_NODE) return;
    const e = n as Element;
    if (SKIP.has(e.tagName)) return;
    if (e.tagName === 'BR') {
      const at = { node: e.parentNode!, offset: indexOf(e) };
      // A <br> at a line's start is a blank line of its own.
      if (!cur) begin(at, at);
      const line = close()!;
      pendingBr = { br: e as HTMLBRElement, line };
      return;
    }
    if (BLOCK.has(e.tagName)) {
      close();
      pendingBr = null;
      e.childNodes.forEach(visit);
      close();
      pendingBr = null;
      return;
    }
    if (REPLACED.has(e.tagName)) {
      const i = indexOf(e);
      begin({ node: e.parentNode!, offset: i }, { node: e.parentNode!, offset: i + 1 });
      return;
    }
    e.childNodes.forEach(visit);
  };
  root.childNodes.forEach(visit);
  close();
  return out;
}

/** <0 when a is before b, 0 when equal, >0 when after. */
function cmp(a: Point, b: Point): number {
  const r = a.node.ownerDocument!.createRange();
  r.setStart(a.node, a.offset);
  r.collapse(true);
  return -r.comparePoint(b.node, b.offset);
}

/** The first and last line a range touches. A selection that ends right at
 *  a line's start (a triple-click) does not reach that line — WebKit's own
 *  rule for formatBlock. */
export function linesInRange(lines: EditorLine[], range: Range): [number, number] | null {
  const start = { node: range.startContainer, offset: range.startOffset };
  const end = { node: range.endContainer, offset: range.endOffset };
  const first = lines.findIndex((l) => cmp(l.end, start) >= 0);
  if (first < 0) return null;
  let last = first;
  if (!range.collapsed) {
    for (let i = first + 1; i < lines.length && cmp(lines[i].start, end) < 0; i++) last = i;
  }
  return [first, last];
}

/** How many characters into its line a point is, clamped to the line. */
function textOffset(line: EditorLine, p: Point): number {
  if (cmp(p, line.start) <= 0) return 0;
  const to = cmp(p, line.end) >= 0 ? line.end : p;
  const r = p.node.ownerDocument!.createRange();
  r.setStart(line.start.node, line.start.offset);
  r.setEnd(to.node, to.offset);
  return r.toString().length;
}

/** The point that many characters into a line. */
function pointAt(root: Element, line: EditorLine, offset: number): Point {
  const doc = root.ownerDocument;
  const r = doc.createRange();
  r.setStart(line.start.node, line.start.offset);
  r.setEnd(line.end.node, line.end.offset);
  const walk = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let left = offset;
  for (let t = walk.nextNode() as Text | null; t; t = walk.nextNode() as Text | null) {
    if (!r.intersectsNode(t)) continue;
    const from = t === line.start.node ? line.start.offset : 0;
    const to = t === line.end.node ? line.end.offset : t.data.length;
    if (left <= to - from) return { node: t, offset: from + left };
    left -= to - from;
  }
  return line.end;
}

export type Exec = (command: string, value?: string) => void;
const execCommand: Exec = (command, value) => {
  document.execCommand(command, false, value);
};

/** Give every line the range touches the block `tag` (`h1`…`div`), and leave
 *  the selection where it was in the text. */
export function applyBlockStyle(root: HTMLElement, range: Range, tag: string, exec: Exec = execCommand): void {
  const sel = root.ownerDocument.getSelection();
  if (!sel) return;
  const select = (a: Point, b: Point = a): void => {
    const r = root.ownerDocument.createRange();
    r.setStart(a.node, a.offset);
    r.setEnd(b.node, b.offset);
    sel.removeAllRanges();
    sel.addRange(r);
  };
  let lines = editorLines(root);
  const reach = linesInRange(lines, range);
  if (!reach) {
    sel.removeAllRanges();
    sel.addRange(range);
    exec('formatBlock', `<${tag}>`);
    return;
  }
  const [first, last] = reach;
  const tail = lines.length - last - 1;
  const startAt = textOffset(lines[first], { node: range.startContainer, offset: range.startOffset });
  const endAt = textOffset(lines[last], { node: range.endContainer, offset: range.endOffset });

  // 1. Every <br> on the boundary of a line the style reaches becomes a
  //    paragraph break — last first, so the earlier ones stay where they are.
  const breaks = [lines[first - 1]?.endBr, ...lines.slice(first, last + 1).map((l) => l.endBr)];
  for (const br of breaks.filter((b): b is HTMLBRElement => !!b).reverse()) {
    if (!br.isConnected) continue;
    const at = { node: br.parentNode!, offset: indexOf(br) };
    select(at, { node: at.node, offset: at.offset + 1 });
    exec('insertParagraph');
  }

  // 2. One formatBlock per line, at a caret — last first, for the same reason.
  for (let i = last; i >= first; i--) {
    lines = editorLines(root);
    if (!lines[i]) continue;
    select(lines[i].start);
    exec('formatBlock', `<${tag}>`);
  }

  // 3. The selection back over the same text.
  lines = editorLines(root);
  const lastNow = lines.length - tail - 1;
  if (!lines[first] || !lines[lastNow] || lastNow < first) return;
  select(pointAt(root, lines[first], startAt), pointAt(root, lines[lastNow], endAt));
}
