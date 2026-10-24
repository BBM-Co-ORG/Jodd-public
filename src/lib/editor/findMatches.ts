// Find in note: what counts as a match, line by line.
//
// A match never leaves its line — the line the user sees, not a text node.
// Finding per text node (findTextRanges) cannot anchor ^ and $ to lines, and
// missed any word whose formatting changes mid-word ("li<b>ne</b>"). Here the
// editor is flattened into lines first — blocks and <br> end a line, inline
// formatting does not — and each line remembers which text node every
// character came from, so a match maps back to a DOM Range.

import { substringMatches } from './bodyEdits';

export interface FindOptions {
  matchCase: boolean;
  regex: boolean;
}

// groups[0] is the whole match, groups[n] capture group n (undefined when the
// group did not take part) — the shape of a RegExp exec result.
export interface LineMatch {
  start: number;
  end: number;
  groups: Array<string | undefined>;
}

export type LineMatcher = (line: string) => LineMatch[];

export type CompiledFind = { ok: true; match: LineMatcher } | { ok: false; error: string };

interface Segment {
  node: Text;
  nodeStart: number; // offset inside node.data
  lineStart: number; // offset inside the line's text
  length: number;
}

export interface EditorLine {
  text: string;
  segments: Segment[];
}

const BLOCK = new Set([
  'ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'DD', 'DIV', 'DL', 'DT', 'FIGCAPTION', 'FIGURE',
  'FOOTER', 'FORM', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HEADER', 'HR', 'LI', 'MAIN', 'NAV', 'OL',
  'P', 'PRE', 'SECTION', 'TABLE', 'TBODY', 'TD', 'TFOOT', 'TH', 'THEAD', 'TR', 'UL',
]);
// Elements whose text the reader never sees as note text.
const OPAQUE = new Set(['INPUT', 'IMG', 'OBJECT', 'SCRIPT', 'STYLE', 'TEMPLATE']);

export function editorLines(root: Node): EditorLine[] {
  const lines: EditorLine[] = [];
  let cur: EditorLine = { text: '', segments: [] };
  const breakLine = (force: boolean) => {
    if (force || cur.text || cur.segments.length) lines.push(cur);
    cur = { text: '', segments: [] };
  };
  const add = (node: Text, nodeStart: number, piece: string) => {
    if (!piece) return;
    cur.segments.push({ node, nodeStart, lineStart: cur.text.length, length: piece.length });
    cur.text += piece;
  };

  const walk = (parent: Node, inPre: boolean) => {
    for (let n = parent.firstChild; n; n = n.nextSibling) {
      if (n.nodeType === Node.TEXT_NODE) {
        const t = n as Text;
        // The editor collapses whitespace, so a newline outside <pre> renders
        // as a space — except the indentation between serialized blocks,
        // which renders as nothing at all.
        if (!inPre) {
          if (t.data.includes('\n') && !t.data.trim()) continue;
          add(t, 0, t.data);
          continue;
        }
        let at = 0;
        for (const piece of t.data.split('\n')) {
          if (at > 0) breakLine(true);
          add(t, at, piece);
          at += piece.length + 1;
        }
      } else if (n.nodeType === Node.ELEMENT_NODE) {
        const tag = (n as Element).tagName;
        if (OPAQUE.has(tag)) continue;
        if (tag === 'BR') { breakLine(true); continue; }
        const block = BLOCK.has(tag);
        if (block) breakLine(false);
        walk(n, inPre || tag === 'PRE');
        if (block) breakLine(false);
      }
    }
  };
  walk(root, false);
  breakLine(false);
  return lines;
}

// A boundary of a non-empty match: a start sits at the beginning of the
// character it precedes, an end at the end of the character it follows, so a
// match never starts at the tail of the previous text node.
function boundary(line: EditorLine, offset: number, isEnd: boolean): [Text, number] | null {
  for (const s of line.segments) {
    const inside = isEnd
      ? offset > s.lineStart && offset <= s.lineStart + s.length
      : offset >= s.lineStart && offset < s.lineStart + s.length;
    if (inside) return [s.node, s.nodeStart + offset - s.lineStart];
  }
  return null;
}

// absStart/absEnd index the note as its lines joined by one break each —
// stable across a re-render of the same text, which a Range is not.
export interface BodyMatch {
  range: Range;
  match: LineMatch;
  absStart: number;
  absEnd: number;
}

export function findBodyMatches(root: Node, match: LineMatcher): BodyMatch[] {
  const out: BodyMatch[] = [];
  let base = 0;
  for (const line of editorLines(root)) {
    const lineBase = base;
    base += line.text.length + 1;
    if (!line.text) continue;
    for (const m of match(line.text)) {
      const a = boundary(line, m.start, false);
      const b = boundary(line, m.end, true);
      if (!a || !b) continue;
      const range = document.createRange();
      range.setStart(a[0], a[1]);
      range.setEnd(b[0], b[1]);
      out.push({ range, match: m, absStart: lineBase + m.start, absEnd: lineBase + m.end });
    }
  }
  return out;
}

function plainMatcher(query: string, matchCase: boolean): LineMatcher {
  if (!matchCase) {
    // substringMatches already folds case one code point at a time ('İ', 'ς').
    const find = substringMatches(query);
    return (line) => find(line).map(([start, end]) => ({ start, end, groups: [line.slice(start, end)] }));
  }
  return (line) => {
    const out: LineMatch[] = [];
    for (let i = line.indexOf(query); i !== -1; i = line.indexOf(query, i + query.length)) {
      out.push({ start: i, end: i + query.length, groups: [query] });
    }
    return out;
  };
}

function regexMatcher(re: RegExp): LineMatcher {
  return (line) => {
    const out: LineMatch[] = [];
    re.lastIndex = 0;
    for (let m = re.exec(line); m; m = re.exec(line)) {
      if (m[0] === '') {
        // Agreed 2026-10-06: zero-length matches (^, $, x*) are not matches —
        // nothing to paint, and replacing one only inserts.
        const cp = line.codePointAt(re.lastIndex);
        re.lastIndex += re.unicode && cp !== undefined && cp > 0xffff ? 2 : 1;
        if (re.lastIndex > line.length) break;
        continue;
      }
      out.push({ start: m.index, end: m.index + m[0].length, groups: Array.from(m) });
    }
    return out;
  };
}

export function compileFind(query: string, opts: FindOptions): CompiledFind {
  if (!query) return { ok: true, match: () => [] };
  if (!opts.regex) return { ok: true, match: plainMatcher(query, opts.matchCase) };
  const flags = opts.matchCase ? 'g' : 'gi';
  try {
    return { ok: true, match: regexMatcher(new RegExp(query, flags + 'u')) };
  } catch {
    // The unicode flavour rejects escapes people carry over from other tools
    // (`\-`, `\#`); the legacy flavour accepts them.
  }
  try {
    return { ok: true, match: regexMatcher(new RegExp(query, flags)) };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

// $1..$9, $& and $$ — the subset of String.prototype.replace agreed for
// Replace. A group the pattern does not have stays literal, as in JS.
export function expandReplacement(template: string, m: LineMatch, regex: boolean): string {
  if (!regex) return template;
  return template.replace(/\$([$&1-9])/g, (all, t: string) => {
    if (t === '$') return '$';
    if (t === '&') return m.groups[0] ?? '';
    const n = Number(t);
    return n < m.groups.length ? (m.groups[n] ?? '') : all;
  });
}
