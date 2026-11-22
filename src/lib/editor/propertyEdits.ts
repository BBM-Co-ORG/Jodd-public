// The body edits behind the property authoring help (spec 2026-11-19 §7):
// Add property, a fix-it, toolbar Due date. Each runs only from an explicit
// click or keypress in NoteEditor — never on open, paint or save.
//
// Every change is an execCommand on a selection, like the # and [[ picks
// (bodyEdits.ts `replaceRanges`), so Cmd+Z takes it back and the input event
// reaches the dirty/save path the same way. Plain text into the existing
// line structure: no formatBlock of a whole region (it merged lines,
// measured 2026-11-17 — blockStyle.ts), no insertHTML.
//
// Positions are found again from the body each time (propertyPaint
// `bodyLines` / `rowText`), never kept as Ranges: a Gmail save re-renders the
// editor and kills them (gotcha #34).

import { bodyLines, rowText, type At, type BodyLine } from './propertyPaint';
import { dueMarkerEdit, fixItEditAt, insertAddPropertyLine } from './propertyTrigger';
import { leaveEnclosingBlock, replaceRanges } from './bodyEdits';
import { applyBlockStyle } from './blockStyle';
import { parsePropertyLine } from '../props/grammar';

/** What Add property types: a key to overwrite (it is left selected) and its `: `. */
export const ADD_PROPERTY_TEXT = 'Key: ';
const PLACEHOLDER_KEY = 'Key';

const exec = (command: string, value?: string) => {
  document.execCommand(command, false, value);
};

function caretAt(node: Node, offset: number): void {
  const sel = window.getSelection();
  if (!sel) return;
  const r = document.createRange();
  r.setStart(node, offset);
  r.collapse(true);
  sel.removeAllRanges();
  sel.addRange(r);
}

const firstAt = (line: BodyLine): At | null => line.at.find((a): a is At => a !== null) ?? null;

/**
 * The index of the line (in `lines`, from `bodyLines`) holding the caret at
 * `(node, offset)`, or -1. A text node can hold several lines (`<pre>`):
 * the line holding the character just before the caret wins.
 */
export function caretBodyLine(lines: BodyLine[], node: Node, offset: number): number {
  let first = -1;
  let before = -1;
  lines.forEach((l, i) => {
    for (const a of l.at) {
      if (!a || a.node !== node) continue;
      if (first < 0) first = i;
      if (a.offset < offset) before = i;
    }
  });
  return before >= 0 ? before : first;
}

/** A Range over `line.text[from, to)`; null when it covers what the walk added (`- `, `[ ] `). */
function textRange(line: BodyLine, from: number, to: number): Range | null {
  const a = line.at[from];
  const b = line.at[to - 1];
  if (!a || !b || to <= from) return null;
  const r = document.createRange();
  r.setStart(a.node, a.offset);
  r.setEnd(b.node, b.offset + 1);
  return r;
}

/** Just past the line's last character, and past whitespace the line split trimmed. */
function lineEnd(line: BodyLine): { node: Text; offset: number } | null {
  for (let i = line.at.length - 1; i >= 0; i--) {
    const a = line.at[i];
    if (!a) continue;
    let offset = a.offset + 1;
    while (offset < a.node.data.length && /[ \t ]/.test(a.node.data[offset])) offset++;
    return { node: a.node, offset };
  }
  return null;
}

/**
 * Where a line typed before body line 0 starts: before a task row's
 * checkbox (so the row moves down whole), else at its first character.
 */
function lineStart(line: BodyLine): { node: Node; offset: number } | null {
  const a = firstAt(line);
  if (!a) return null;
  if (line.at[0] === null && /^\[[ x]\] /.test(line.text)) {
    for (let el = a.node.parentElement; el; el = el.parentElement) {
      const box = Array.from(el.children).find((c) => c.localName === 'input' && c.getAttribute('type')?.toLowerCase() === 'checkbox');
      if (box) return { node: el, offset: Array.prototype.indexOf.call(el.childNodes, box) as number };
    }
  }
  return { node: a.node, offset: a.offset };
}

/** Select the `Key` of the `Key: ` line at `index`; turns a list item or heading there into a plain line first. */
function selectPlaceholder(editorEl: HTMLElement, index: number): boolean {
  const find = () => {
    const line = bodyLines(editorEl)[index];
    if (!line) return null;
    const k = line.text.indexOf(`${PLACEHOLDER_KEY}:`);
    return k < 0 ? null : { line, k };
  };
  let hit = find();
  if (!hit) return false;
  // `- Key:` / `# Key:` is no property line: the walk's prefix means a list
  // item or a heading (h1–h3). Only line 0 can land in one — Add property
  // typed into the front of the first line.
  if (hit.line.at[0] === null && !/^\[[ x]\] /.test(hit.line.text)) {
    const r = textRange(hit.line, hit.k, hit.k + PLACEHOLDER_KEY.length);
    if (!r) return false;
    const el = r.startContainer.parentElement;
    if (el?.closest('li')) {
      caretAt(r.startContainer, r.startOffset);
      leaveEnclosingBlock(editorEl);
    } else {
      applyBlockStyle(editorEl, r, 'div');
    }
    hit = find();
    if (!hit) return false;
  }
  const r = textRange(hit.line, hit.k, hit.k + PLACEHOLDER_KEY.length);
  if (!r) return false;
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(r);
  return true;
}

/**
 * Toolbar Add property: a `Key: ` line at the end of the property block, or
 * as body line 0 when there is none, with `Key` selected so typing replaces
 * it. Returns the new line's index, or -1 when it could not be placed.
 */
export function insertPropertyLine(editorEl: HTMLElement): number {
  const lines = bodyLines(editorEl);
  const { lineIndex } = insertAddPropertyLine(lines.map((l) => l.text));
  editorEl.focus();
  if (lineIndex > 0) {
    const end = lineEnd(lines[lineIndex - 1]);
    if (!end) return -1;
    caretAt(end.node, end.offset);
    exec('insertParagraph');
    exec('insertText', ADD_PROPERTY_TEXT);
  } else if (lines.length > 0 && lines[0].text !== '') {
    // Typed at the front of the first line, then split off it: the line
    // keeps its own block, and the key is not typed into an empty block
    // WebKit may have made a copy of a list item or a heading.
    const start = lineStart(lines[0]);
    if (!start) return -1;
    caretAt(start.node, start.offset);
    exec('insertText', ADD_PROPERTY_TEXT);
    exec('insertParagraph');
  } else {
    // An empty body, or a blank first line: type into it.
    const first = editorEl.firstChild;
    caretAt(first && first.nodeType === 1 ? first : editorEl, 0);
    exec('insertText', ADD_PROPERTY_TEXT);
  }
  return selectPlaceholder(editorEl, lineIndex) ? lineIndex : -1;
}

/** A Range over `line.text[from, to)`; collapsed just after `from - 1` when they are equal. */
function editRange(line: BodyLine, from: number, to: number): Range | null {
  if (to > from) return textRange(line, from, to);
  const a = line.at[from - 1];
  if (!a) return null;
  const r = document.createRange();
  r.setStart(a.node, a.offset + 1);
  r.collapse(true);
  return r;
}

/**
 * Accept the fix-it on body line `index`, if the line still reads
 * `expected` (it may have changed since the offer). Only the characters that
 * change are touched (`fixItEditAt`, ruling R12): a space goes in after the
 * colon, a date's characters are replaced — a link or a bold key on the
 * line stays as it is. True when the body changed.
 */
export function applyFixIt(editorEl: HTMLElement, index: number, expected: string, today: Date): boolean {
  const lines = bodyLines(editorEl, (t) => parsePropertyLine(t) === null);
  const line = lines[index];
  if (!line || line.text !== expected) return false;
  const fix = fixItEditAt(lines.map((l) => l.text), index, today);
  if (!fix) return false;
  const ranges: Range[] = [];
  for (const e of fix.edits) {
    const r = editRange(line, e.start, e.end);
    if (!r) return false;
    ranges.push(r);
  }
  return replaceRanges(editorEl, ranges, fix.edits.map((e) => e.text), { giveBackFocus: false }) > 0;
}

/**
 * Toolbar Due date on `row` (a task row: the parent of its checkbox): set
 * its due date to `date` — the existing marker's date replaced, else
 * ` 📅 <date>` appended (propertyTrigger `dueMarkerEdit`).
 */
export function applyDueDate(editorEl: HTMLElement, row: Element, date: string): boolean {
  const { text, at } = rowText(row);
  const e = dueMarkerEdit(text, date);
  const r = document.createRange();
  if (e.start < e.end) {
    r.setStart(at[e.start].node, at[e.start].offset);
    r.setEnd(at[e.end - 1].node, at[e.end - 1].offset + 1);
  } else if (e.start < at.length) {
    r.setStart(at[e.start].node, at[e.start].offset);
  } else if (at.length > 0) {
    const last = at[at.length - 1];
    r.setStart(last.node, last.offset + 1);
  } else {
    const box = Array.from(row.children).find((c) => c.localName === 'input');
    if (!box) return false;
    r.setStart(row, (Array.prototype.indexOf.call(row.childNodes, box) as number) + 1);
  }
  if (e.start >= e.end) r.collapse(true);
  return replaceRanges(editorEl, [r], e.text, { giveBackFocus: false }) > 0;
}
