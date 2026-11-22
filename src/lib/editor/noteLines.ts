// What a "line" is in a note, said once: one paragraph, list item, heading or
// table row, however many screen lines it wraps onto. The date tooltip's
// count (noteStats, from the stored body) and the editor's line-number gutter
// (from the live editor) both walk the DOM with noteLines, so the number the
// tooltip gives is the last number the gutter shows.
//
// A DOM walk, not a regex over the HTML: nesting decides what a line is
// (`<li>a<ul><li>b</li></ul></li>` is two), and the gutter needs the nodes
// themselves to measure where each line sits.
import { bodyContent } from '../noteHtml';
import { plural } from '../errorText';

const BLOCK = new Set([
  'ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'DD', 'DETAILS', 'DIV', 'DL', 'DT',
  'FIELDSET', 'FIGCAPTION', 'FIGURE', 'FOOTER', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
  'HEADER', 'LI', 'MAIN', 'NAV', 'OL', 'P', 'PRE', 'SECTION', 'SUMMARY', 'TABLE',
  'TBODY', 'TFOOT', 'THEAD', 'TR', 'UL',
]);
// Never a line, and never text: rules, styles and scripts.
const SKIP = new Set(['STYLE', 'SCRIPT', 'TEMPLATE', 'HR', 'HEAD', 'TITLE', 'META', 'LINK']);
const BLOCK_SELECTOR = [...BLOCK].join(',').toLowerCase();

export interface NoteLine {
  /** The nodes the line is made of: one block element, or a run of bare
   *  text and inline nodes between blocks (the gutter measures a Range). */
  nodes: Node[];
  /** Its text, whitespace collapsed; '' for a blank line or an image. */
  text: string;
}

function el(n: Node): Element | null {
  return n.nodeType === Node.ELEMENT_NODE ? (n as Element) : null;
}

function textOf(nodes: Node[]): string {
  let out = '';
  const walk = (n: Node): void => {
    if (n.nodeType === Node.TEXT_NODE) {
      out += (n as Text).data;
      return;
    }
    const e = el(n);
    if (!e || SKIP.has(e.tagName)) return;
    // A break or a cell boundary still separates words.
    if (e.tagName === 'BR') {
      out += ' ';
      return;
    }
    const cell = e.tagName === 'TD' || e.tagName === 'TH';
    if (cell) out += ' ';
    e.childNodes.forEach(walk);
    if (cell) out += ' ';
  };
  nodes.forEach(walk);
  return out.replace(/\s+/g, ' ').trim();
}

/** A run of inline nodes is a line when it shows something: text, or an
 *  element such as an image. Source-formatting whitespace and a lone <br>
 *  between blocks are not. */
function runShows(run: Node[]): boolean {
  return run.some((n) => {
    if (n.nodeType === Node.TEXT_NODE) return /\S/.test((n as Text).data);
    const e = el(n);
    return !!e && e.tagName !== 'BR' && !SKIP.has(e.tagName);
  });
}

// A block with nothing in it takes no height (an empty <div></div> is a
// zero-height box), so numbering it draws a number squeezed onto the border
// between its neighbours — live 2026-11-12, "139, 140, 141" with 140 jammed
// between two link lines. A blank LINE is <div><br></div>; this is not one.
// Not <li> (an empty item still shows its marker) and not table rows.
const EMPTY_WHEN_BARE = new Set(['DIV', 'P', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6']);
const REPLACED = 'img,video,audio,canvas,svg,iframe,object,embed,input,textarea,select,button,br,hr';

export function rendersNothing(e: Element): boolean {
  if (!EMPTY_WHEN_BARE.has(e.tagName)) return false;
  if (/\S/.test(e.textContent ?? '')) return false;
  return !e.querySelector(REPLACED);
}

/** The note's lines, in order. */
export function noteLines(root: Element): NoteLine[] {
  const lines: NoteLine[] = [];
  const push = (nodes: Node[]) => lines.push({ nodes, text: textOf(nodes) });

  const container = (parent: Element): void => {
    let children = [...parent.childNodes];
    // A closed <details> shows its summary only.
    if (parent.tagName === 'DETAILS' && !parent.hasAttribute('open')) {
      children = children.filter((c) => el(c)?.tagName === 'SUMMARY');
    }
    let run: Node[] = [];
    const flush = () => {
      if (runShows(run)) push(run);
      run = [];
    };
    for (const c of children) {
      const e = el(c);
      if (e && SKIP.has(e.tagName)) {
        flush();
        continue;
      }
      const isBlock = !!e && BLOCK.has(e.tagName);
      // A table row is one line, whatever its cells hold.
      const holdsBlocks = !!e && e.tagName !== 'TR' && !!e.querySelector(BLOCK_SELECTOR);
      if (!isBlock && !holdsBlocks) {
        run.push(c);
        continue;
      }
      flush();
      if (holdsBlocks || e!.tagName === 'DETAILS') container(e!);
      else if (!rendersNothing(e!)) push([e!]);
    }
    flush();
  };

  container(root);
  return lines;
}

export interface NoteStats {
  words: number;
  characters: number;
  lines: number;
}

let wordSeg: Intl.Segmenter | null | undefined;
let graphemeSeg: Intl.Segmenter | null | undefined;
function segmenter(kind: 'word' | 'grapheme'): Intl.Segmenter | null {
  const make = () => (typeof Intl.Segmenter === 'function' ? new Intl.Segmenter(undefined, { granularity: kind }) : null);
  if (kind === 'word') return wordSeg === undefined ? (wordSeg = make()) : wordSeg;
  return graphemeSeg === undefined ? (graphemeSeg = make()) : graphemeSeg;
}

// Thai has no spaces between words: splitting on spaces would count a whole
// Thai sentence as one word. The segmenter's dictionary splits it.
function countWords(text: string): number {
  const seg = segmenter('word');
  if (!seg) return text.split(/\s+/).filter(Boolean).length;
  let n = 0;
  for (const s of seg.segment(text)) if (s.isWordLike) n++;
  return n;
}

// Graphemes, not UTF-16 units: "ที่" is one character to a reader, not three.
function countCharacters(text: string): number {
  const seg = segmenter('grapheme');
  if (!seg) return Array.from(text).length;
  let n = 0;
  for (const _ of seg.segment(text)) n++;
  return n;
}

/** What the reader sees: the title and the body as the editor shows it.
 *  A closed <details> counts as its summary alone — a saved source
 *  transcript folded away under one made a few hundred visible words read
 *  as 24,382 (live, 2026-11-12). Lines are the gutter's last number. */
export function noteStats(title: string, bodyHtml: string): NoteStats {
  const body = new DOMParser().parseFromString(bodyContent(bodyHtml ?? ''), 'text/html').body;
  const lines = noteLines(body);
  const texts = [(title ?? '').replace(/\s+/g, ' ').trim(), ...lines.map((l) => l.text)].filter(Boolean);
  return {
    words: texts.reduce((n, t) => n + countWords(t), 0),
    characters: texts.reduce((n, t) => n + countCharacters(t), 0),
    lines: lines.length,
  };
}

export function statsLine(s: NoteStats): string {
  return `${plural(s.words, 'word')} · ${plural(s.characters, 'character')} · ${plural(s.lines, 'line')}`;
}
