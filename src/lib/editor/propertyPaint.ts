// What the editor shows of what Jodd recognized in a note — the property
// block at the top (`Key: value` lines) and the due date on each task row —
// painted with the CSS Custom Highlight API (spec 2026-11-19-properties-
// tasks-views §7). Decoration only: nothing here writes to the body, whose
// HTML round-trips to Apple Notes. A highlight is not markup; the hover hint
// goes on the editor element (NoteEditor `onEditorMouseMove`), as links' does.
//
// Recognized = what the deriver recognizes. The grammar is
// props/grammar.ts (pinned to the Rust parser by tests/fixtures/*.json), and
// the LINES it runs over are split here exactly as the deriver splits a
// cached body (`agent_memory::text::html_to_lines`, pinned by
// tests/fixtures/body-lines.json) — NOT as the editor's own noteLines does:
// noteLines makes `<div>a<br>b</div>` one line and drops the `- ` / `# ` /
// `[ ] ` prefixes that keep a list item, heading or task row out of the
// block. noteLines still gives each painted item its `line`: the line-number
// gutter's index, which is where the gutter draws its marker.
//
// Painting follows links/paint.ts and Find (gotcha #34): one highlight per
// kind plus the transparent `jodd-prop-gone`, through which every range that
// stops being painted passes — WebKit never repaints a range that leaves
// every highlight. Never "simplify" that to `CSS.highlights.delete`.

import { parseDue, parsePropertyBlock, parsePropertyLine } from '../props/grammar';
import { suggestFix } from '../props/fixIt';
import { noteLines } from './noteLines';
import { afterNextPaint } from './visibleRanges';

export type PaintKind = 'prop-key' | 'prop-value' | 'due-overdue' | 'due-today' | 'due-future' | 'prop-warn';

export interface PropertyPaintItem {
  kind: PaintKind;
  range: Range;
  /** The line-number gutter's index (noteLines, 0-based) of the line it sits on. */
  line: number;
  /** Due ranges only: the day relative to today (`พรุ่งนี้`, `เลย 2 วัน`). */
  hint?: string;
}

const KINDS: readonly PaintKind[] = ['prop-key', 'prop-value', 'due-overdue', 'due-today', 'due-future', 'prop-warn'];

/** `prop-key` → `jodd-prop-key`. */
export const highlightName = (k: PaintKind): string => `jodd-${k}`;
export const PROP_HL_GONE = 'jodd-prop-gone';
/**
 * Under links (links/paint.ts, 0) and Find (1, 2): a link inside a value, or
 * a find match inside a key, shows as the link or the match.
 */
export const PROP_HL_PRIORITY = -1;

// ─── Lines, split as the deriver splits them ────────────────────────────────

/** Rust's `char::is_whitespace` (Unicode White_Space): `&nbsp;` included, U+FEFF not. */
const WS = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/;
const isWs = (c: string | undefined): boolean => c !== undefined && WS.test(c);
/** The deriver's stand-in for a `<pre>`'s newline while it walks. */
const PRE_NEWLINE = '\uFDD0';
const BLOCKS = new Set(['div', 'p', 'li', 'ul', 'ol', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'pre', 'blockquote', 'tr', 'hr', 'details', 'summary']);
const PREFIX: Record<string, string> = { li: '- ', h1: '# ', h2: '## ', h3: '### ' };

export interface At {
  node: Text;
  offset: number;
}

export interface BodyLine {
  /** The line's text, as `html_to_lines` gives it. */
  text: string;
  /** Where each UTF-16 unit of `text` came from; null for what the walk added (`- `, `[ ] `). */
  at: (At | null)[];
}

/**
 * The body's lines exactly as `agent_memory::text::html_to_lines` gives them
 * for the same HTML (a mirror of its `walk`, include_source on), each char
 * mapped back to the text node it came from. `stopAfter` ends the walk after
 * the first complete line it accepts: the block needs only its own lines and
 * the one that ends it, never the whole note.
 */
export function bodyLines(root: Element, stopAfter?: (text: string) => boolean): BodyLine[] {
  let out = '';
  const at: (At | null)[] = [];
  let lineStart = 0;
  let stopped = false;
  const ends = (...cs: string[]) => out.length > 0 && cs.includes(out[out.length - 1]);
  const push = (s: string, from: At | null) => {
    for (let i = 0; i < s.length; i++) {
      out += s[i];
      at.push(from && { node: from.node, offset: from.offset + i });
      if (s[i] === '\n' || s[i] === PRE_NEWLINE) {
        if (stopAfter?.(trimEnd(out.slice(lineStart, out.length - 1)))) stopped = true;
        lineStart = out.length;
      }
    }
  };

  const text = (node: Text, inPre: boolean) => {
    const t = node.data;
    if (inPre) {
      for (let i = 0; i < t.length && !stopped; i++) {
        if (t[i] === PRE_NEWLINE) continue;
        push(t[i] === '\n' ? PRE_NEWLINE : t[i], { node, offset: i });
      }
      return;
    }
    // `split_whitespace().join(" ")`, then every PRE_NEWLINE removed.
    const collapsed: { c: string; i: number }[] = [];
    let word = -1;
    let words = 0;
    for (let i = 0; i <= t.length; i++) {
      const inWord = i < t.length && !isWs(t[i]);
      if (inWord && word < 0) {
        if (words++ > 0) collapsed.push({ c: ' ', i: i - 1 });
        word = i;
      } else if (!inWord && word >= 0) {
        for (let j = word; j < i; j++) if (t[j] !== PRE_NEWLINE) collapsed.push({ c: t[j], i: j });
        word = -1;
      }
    }
    if (collapsed.length === 0) {
      if (t.length > 0 && out.length > 0 && !ends(' ', '\n')) push(' ', { node, offset: 0 });
      return;
    }
    if (isWs(t[0]) && out.length > 0 && !ends(' ', '\n')) push(' ', { node, offset: 0 });
    for (const { c, i } of collapsed) {
      if (stopped) return;
      push(c, { node, offset: i });
    }
    if (isWs(t[t.length - 1])) push(' ', { node, offset: t.length - 1 });
  };

  const walk = (node: Node, inPre: boolean) => {
    if (stopped) return;
    if (node.nodeType === Node.TEXT_NODE) return text(node as Text, inPre);
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const el = node as Element;
    const tag = el.localName;
    if (tag === 'script' || tag === 'style') return;
    if (tag === 'br') return push('\n', null);
    const block = BLOCKS.has(tag);
    if (block && out.length > 0 && !ends('\n')) push('\n', null);
    if (PREFIX[tag]) push(PREFIX[tag], null);
    if (tag === 'input') push(el.hasAttribute('checked') ? '[x] ' : '[ ] ', null);
    for (const c of Array.from(el.childNodes)) {
      if (stopped) return;
      walk(c, inPre || tag === 'pre');
    }
    if (block && !ends('\n')) push('\n', null);
  };

  for (const c of Array.from(root.childNodes)) {
    if (stopped) break;
    walk(c, false);
  }

  // `html_to_lines`: one trailing newline off, nothing → no lines, split, trim_end.
  let end = out.length;
  if (!stopped && out.endsWith('\n')) end--;
  if (end === 0) return [];
  const lines: BodyLine[] = [];
  let start = 0;
  for (let i = 0; i <= end; i++) {
    if (i === end || out[i] === '\n' || out[i] === PRE_NEWLINE) {
      const t = trimEnd(out.slice(start, i));
      lines.push({ text: t, at: at.slice(start, start + t.length) });
      start = i + 1;
      if (i === end || (stopped && start >= lineStart)) break;
    }
  }
  return lines;
}

function trimEnd(s: string): string {
  let e = s.length;
  while (e > 0 && isWs(s[e - 1])) e--;
  return s.slice(0, e);
}

/** A Range over `line.text[from, to)`, or null when it covers something the walk added. */
function lineRange(line: BodyLine, from: number, to: number): Range | null {
  const a = line.at[from];
  const b = line.at[to - 1];
  if (!a || !b || to <= from) return null;
  const r = document.createRange();
  r.setStart(a.node, a.offset);
  r.setEnd(b.node, b.offset + 1);
  return r;
}

// ─── prop-warn, conservatively ──────────────────────────────────────────────
//
// suggestFix (props/fixIt.ts) offers fixes users did not ask for: it reads
// `Ratio: 1/2` as 1 February and `Meeting at 10:30` as a key missing its
// space. The wavy underline is louder than a menu entry, so it fires only
// for the near misses the spec names: a missing space after the colon (not
// between digits — that is a time), and a slashed date — a full
// `DD/MM/YYYY` under any key, a short `DD/MM` or `DD/MM/YY` only under a key
// that names a date.

const FULL_SLASHED_DATE = /^\d{1,2}\/\d{1,2}\/\d{4}$/;
const DATE_KEY = /(?:^|[\s_-])(?:due|dates?|deadline|start|end|until|when)(?:$|[\s_-])|วัน|กำหนด|เริ่ม|สิ้นสุด/i;

function warnsAsDate(key: string, value: string): boolean {
  return FULL_SLASHED_DATE.test(value) || DATE_KEY.test(key);
}

function warnsAsMissingSpace(text: string): boolean {
  const colon = text.indexOf(':');
  return !(/[0-9]/.test(text.charAt(colon - 1)) && /[0-9]/.test(text.charAt(colon + 1)));
}

/**
 * The line as fix-it would rewrite it, when this line warns; null when it
 * does not. `inBlock`: a line of the block (it parses — only its slashed
 * date can warn); otherwise the one line that ends the block (only a missing
 * space can warn there, and its date is rewritten too only where a block
 * line's date would warn: `Ratio:1/2` → `Ratio: 1/2`, never `Ratio:
 * 2026-02-01`). The paint and the fix-it (editor/propertyTrigger.ts
 * `fixItAt`) both ask this, so a fix is offered exactly where the wavy
 * underline is drawn, and only the fix that underline stands for.
 */
export function warnFix(text: string, inBlock: boolean, today: Date): string | null {
  const fix = suggestFix(text, today);
  if (fix === null) return null;
  if (inBlock) {
    const p = parsePropertyLine(text);
    return p && warnsAsDate(p.key, p.value) ? fix : null;
  }
  if (text.trim() === '' || !warnsAsMissingSpace(text)) return null;
  // suggestFix's own reading: the space goes after the first colon.
  const spaced = parsePropertyLine(text.replace(/\u00a0/g, ' ').replace(':', ': '));
  if (!spaced) return null;
  return warnsAsDate(spaced.key, spaced.value) ? fix : `${spaced.key}: ${spaced.value}`;
}

// ─── Due dates ──────────────────────────────────────────────────────────────

const dayNumber = (y: number, m: number, d: number) => Math.round(Date.UTC(y, m - 1, d) / 86_400_000);
function daysFromToday(iso: string, today: Date): number {
  const [y, m, d] = iso.split('-').map(Number);
  return dayNumber(y, m, d) - dayNumber(today.getFullYear(), today.getMonth() + 1, today.getDate());
}

/** `วันนี้`, `พรุ่งนี้`, `อีก N วัน`, `เลย N วัน` — `today` is the device's local date. */
export function relativeDay(iso: string, today: Date): string {
  const n = daysFromToday(iso, today);
  if (n === 0) return 'วันนี้';
  if (n === 1) return 'พรุ่งนี้';
  return n > 1 ? `อีก ${n} วัน` : `เลย ${-n} วัน`;
}

export interface RowText {
  text: string;
  at: At[];
}

/** The row's text as `tasks.rs` `row_text` reads it: every text descendant, in order. */
export function rowText(row: Element): RowText {
  let text = '';
  const at: At[] = [];
  const walker = row.ownerDocument.createTreeWalker(row, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode() as Text | null; n; n = walker.nextNode() as Text | null) {
    for (let i = 0; i < n.data.length; i++) at.push({ node: n, offset: i });
    text += n.data;
  }
  return { text, at };
}

/**
 * Where in `text` the due date `parseDue` read sits. parseDue says which
 * date, not where, and the same date can appear again as plain text; so each
 * occurrence is masked with another date in turn, and the one parseDue then
 * reports is the one it read. The grammar stays in one place.
 */
export function dueOffset(text: string, due: string): number {
  const other = due === '1999-01-01' ? '1998-01-01' : '1999-01-01';
  for (let i = text.lastIndexOf(due); i >= 0; i = i === 0 ? -1 : text.lastIndexOf(due, i - 1)) {
    if (parseDue(text.slice(0, i) + other + text.slice(i + due.length)) === other) return i;
  }
  return -1;
}

// ─── The scan ───────────────────────────────────────────────────────────────

/**
 * Everything to paint in `root` (the editor body), in document order.
 * Read-only on the DOM: it builds Ranges and nothing else.
 */
export function propertyRanges(root: Element, today: Date): PropertyPaintItem[] {
  const items: Omit<PropertyPaintItem, 'line'>[] = [];
  const add = (kind: PaintKind, range: Range | null, hint?: string) => {
    if (range) items.push(hint === undefined ? { kind, range } : { kind, range, hint });
  };

  // The block: its lines, and the one line after it (the block position).
  const lines = bodyLines(root, (t) => parsePropertyLine(t) === null);
  const props = parsePropertyBlock(lines.map((l) => l.text));
  for (const p of props) {
    const line = lines[p.pos];
    const colon = line.text.indexOf(':');
    const rest = line.text.slice(colon + 1);
    const vStart = colon + 1 + (rest.length - rest.trimStart().length);
    const vEnd = vStart + p.value.length;
    add('prop-key', lineRange(line, 0, colon));
    add(warnFix(line.text, true, today) !== null ? 'prop-warn' : 'prop-value', lineRange(line, vStart, vEnd));
  }
  const next = lines[props.length];
  if (next && warnFix(next.text, false, today) !== null) {
    add('prop-warn', lineRange(next, 0, next.text.length));
  }

  // Task rows: the parent of each checkbox, as tasks.rs `collect_tasks` reads it.
  const rows = new Set<Element>();
  for (const input of Array.from(root.querySelectorAll('input'))) {
    if (input.getAttribute('type')?.toLowerCase() !== 'checkbox' || !input.parentElement) continue;
    rows.add(input.parentElement);
  }
  // A row nested in a row reads its child's text too: paint each date once.
  const seen = new Map<Text, Set<number>>();
  for (const row of rows) {
    const { text, at } = rowText(row);
    const due = parseDue(text);
    if (due === null) continue;
    const i = dueOffset(text, due);
    if (i < 0) continue;
    const a = at[i];
    const b = at[i + due.length - 1];
    const offsets = seen.get(a.node) ?? new Set<number>();
    if (offsets.has(a.offset)) continue;
    seen.set(a.node, offsets.add(a.offset));
    const range = document.createRange();
    range.setStart(a.node, a.offset);
    range.setEnd(b.node, b.offset + 1);
    const box = Array.from(row.children).find((c) => c.localName === 'input' && c.getAttribute('type')?.toLowerCase() === 'checkbox');
    const done = !!box && (box.hasAttribute('checked') || (box as HTMLInputElement).checked);
    const n = daysFromToday(due, today);
    // Nothing is overdue once done.
    const kind: PaintKind = done || n > 0 ? 'due-future' : n === 0 ? 'due-today' : 'due-overdue';
    add(kind, range, relativeDay(due, today));
  }

  if (items.length === 0) return [];
  items.sort((x, y) => x.range.compareBoundaryPoints(Range.START_TO_START, y.range));
  const lineOf = gutterLineOf(root);
  return items.map((it) => ({ ...it, line: lineOf(it.range.startContainer) }));
}

/** The noteLines index (the gutter's) holding `node`, or -1. */
function gutterLineOf(root: Element): (node: Node) => number {
  const index = new Map<Node, number>();
  noteLines(root).forEach((l, i) => l.nodes.forEach((n) => index.set(n, i)));
  return (node) => {
    for (let n: Node | null = node; n && n !== root; n = n.parentNode) {
      const i = index.get(n);
      if (i !== undefined) return i;
    }
    return -1;
  };
}

// ─── Painting ───────────────────────────────────────────────────────────────

const canHighlight = () => typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';

export interface PropertyPainter {
  /** Paint these items; whatever was painted before moves into `PROP_HL_GONE`. */
  paint(items: readonly PropertyPaintItem[]): void;
  /** Un-paint everything — through `PROP_HL_GONE`, never `delete`. */
  clear(): void;
}

/**
 * `afterPaint` (editor/visibleRanges.ts `afterNextPaint`, injected for tests)
 * empties gone once the browser has painted it, as links/paint.ts does:
 * WebKit re-validates every range in every highlight on every frame.
 */
export function createPropertyPainter(afterPaint: (cb: () => void) => void = afterNextPaint): PropertyPainter {
  let painted: Range[] = [];
  let generation = 0;
  const highlight = (ranges: Range[]) => {
    const h = new Highlight(...ranges);
    h.priority = PROP_HL_PRIORITY;
    return h;
  };
  const paint = (items: readonly PropertyPaintItem[]) => {
    if (!canHighlight()) return;
    // Gone first: on the first paint that fixes its place in the registry
    // ahead of the kinds, so a range in both paints as its kind.
    CSS.highlights.set(PROP_HL_GONE, highlight(painted));
    for (const k of KINDS) {
      CSS.highlights.set(highlightName(k), highlight(items.filter((i) => i.kind === k).map((i) => i.range)));
    }
    painted = items.map((i) => i.range);
    const mine = ++generation;
    afterPaint(() => {
      if (mine === generation && canHighlight()) CSS.highlights.set(PROP_HL_GONE, highlight([]));
    });
  };
  return { paint, clear: () => paint([]) };
}

/** The hint of the item under `(x, y)` (client coordinates), or ''. */
export function propertyHintAt(x: number, y: number, items: readonly PropertyPaintItem[]): string {
  for (const it of items) {
    if (!it.hint) continue;
    for (const r of Array.from(it.range.getClientRects?.() ?? [])) {
      if (x >= r.left && x < r.right && y >= r.top && y < r.bottom) return it.hint;
    }
  }
  return '';
}
