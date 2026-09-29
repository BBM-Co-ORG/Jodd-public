// Edits NoteEditor makes to the note body on the user's behalf rather than
// from a keystroke in it: a hashtag added from the tag field, a tag removed
// from its chip, find/replace, a picked [[link]], a checklist row.
//
// Every one goes through execCommand, never direct DOM mutation — WebKit's
// undo history is a list of edit commands, not DOM snapshots (see
// markdownTriggers.ts). Measured 2026-09-14 in WKWebView with real key events,
// against the direct-mutation code this replaced:
//   - Replace / Replace All rewrote text nodes in place. Afterwards Cmd+Z did
//     nothing at all — not even for text typed BEFORE the replace — and redo
//     lost the caret.
//   - A tag appended with appendChild could not be undone, and the typing on
//     either side of it merged into one undo step as if it were not there.

import { ensureCheckboxesNotEditable } from './markdownTriggers';
import { tryExitSpecialBlock } from './exitSpecialBlock';

export type TextMatcher = (text: string) => Array<[start: number, end: number]>;

// Lower-cases one code point at a time and remembers which original
// character each lower-case unit came from. Lower-casing can change length —
// 'İ' becomes 'i̇', two units — so an offset found in text.toLowerCase() does
// not index `text`. Measured: Replace All after "İzmir" threw IndexSizeError
// when the match ended its text node, and mid-node replaced "ar " for "bar".
function foldCase(text: string): { folded: string; from: number[]; to: number[] } {
  let folded = '';
  const from: number[] = [];
  const to: number[] = [];
  for (let i = 0; i < text.length; ) {
    const ch = String.fromCodePoint(text.codePointAt(i)!);
    let lower = ch.toLowerCase();
    // Final sigma: a whole-string toLowerCase() turns a word-final Σ into ς,
    // one code point at a time gives σ — "οδος" stopped finding "ΟΔΟΣ"
    // (measured). Search treats them as one letter, as Unicode case folding
    // does: fold both to σ, on both sides.
    if (lower === 'ς') lower = 'σ';
    for (let k = 0; k < lower.length; k++) {
      from.push(i);
      to.push(i + ch.length);
    }
    folded += lower;
    i += ch.length;
  }
  return { folded, from, to };
}

// Case-insensitive, non-overlapping occurrences of `query` — Find's semantics.
// Offsets index the original text; a match must cover whole characters.
export function substringMatches(query: string): TextMatcher {
  const q = foldCase(query).folded;
  return (text) => {
    const out: Array<[number, number]> = [];
    if (!q) return out;
    const { folded, from, to } = foldCase(text);
    let j = folded.indexOf(q);
    while (j !== -1) {
      const last = j + q.length - 1;
      const wholeStart = j === 0 || from[j - 1] !== from[j];
      const wholeEnd = last + 1 === folded.length || from[last + 1] !== from[last];
      if (wholeStart && wholeEnd) {
        out.push([from[j], to[last]]);
        j = folded.indexOf(q, j + q.length);
      } else {
        j = folded.indexOf(q, j + 1);
      }
    }
    return out;
  };
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// `#tag` as a whole hashtag: not preceded by a word character, and not a
// prefix of a longer tag (#work must not match #working). Only the `#tag`
// itself is in the range — the character before it stays.
export function hashtagMatches(tag: string): TextMatcher {
  return (text) => {
    const re = new RegExp(`(^|[^\\p{L}\\p{N}_])#${escapeRegex(tag)}(?=[^\\p{L}\\p{N}_\\p{M}]|$)`, 'giu');
    const out: Array<[number, number]> = [];
    for (const m of text.matchAll(re)) {
      out.push([m.index! + m[1].length, m.index! + m[0].length]);
    }
    return out;
  };
}

// One Range per match, in document order. A match must lie within a single
// text node — the same limit the find bar has always had.
export function findTextRanges(root: Node, matcher: TextMatcher): Range[] {
  const out: Range[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode() as Text | null; n; n = walker.nextNode() as Text | null) {
    if (!n.data) continue;
    for (const [start, end] of matcher(n.data)) {
      const r = document.createRange();
      r.setStart(n, start);
      r.setEnd(n, end);
      out.push(r);
    }
  }
  return out;
}

function select(range: Range) {
  const sel = window.getSelection();
  sel?.removeAllRanges();
  sel?.addRange(range);
}

// execCommand edits the selection, so the editor has to hold it. Returns a
// function that gives focus back to whatever had it (the tag field, the find
// bar) when that was outside the editor.
function takeFocus(editorEl: HTMLElement, giveBack: boolean): () => void {
  const prev = document.activeElement as HTMLElement | null;
  if (prev !== editorEl) editorEl.focus();
  return () => {
    if (giveBack && prev && prev !== editorEl && !editorEl.contains(prev) && typeof prev.focus === 'function') {
      prev.focus();
    }
  };
}

// Replaces each range's contents with `text` (deletes them when `text` is
// empty) as edit commands. Ranges must be in document order; they are applied
// last-first so an edit never shifts a range still waiting its turn. Returns
// how many were replaced.
export function replaceRanges(
  editorEl: HTMLElement,
  ranges: Range[],
  text: string,
  { giveBackFocus = true }: { giveBackFocus?: boolean } = {},
): number {
  const live = ranges.filter((r) => editorEl.contains(r.startContainer) && editorEl.contains(r.endContainer));
  if (!live.length) return 0;
  const giveBack = takeFocus(editorEl, giveBackFocus);

  // Measured: these join the undo step of whatever the user typed just before,
  // so one Cmd+Z takes back that typing and the replace together — coherent,
  // only coarse. An empty insertHTML first does close the typing step, but
  // anywhere except a block start it also splits the block there (Replace All
  // left a stray <div>, a picked [[link]] duplicated the query). Do not add
  // it back; unlink and removeFormat do not close the step at all.
  for (let i = live.length - 1; i >= 0; i--) {
    select(live[i]);
    if (text) document.execCommand('insertText', false, text);
    else document.execCommand('delete');
  }
  giveBack();
  return live.length;
}

// What adding `tag` to a body whose last line reads `lastLine` inserts: onto
// that line when it is empty or already a line of hashtags, else on a new line.
// A checklist row always gets a new line: its checkbox adds no text, so an
// empty row reads as blank and a row of tags as a tag line, and the tag was
// typed onto the row — an unchecked task named "#work" (measured).
export interface TagInsertion {
  newLine: boolean;
  text: string;
}
const TAG_LINE = /^\s*(#[^\s#]+\s*)+$/u;

export function planTagInsertion(lastLine: string, tag: string, lastLineIsTask = false): TagInsertion {
  if (lastLineIsTask) return { newLine: true, text: `#${tag}` };
  if (!lastLine.trim()) return { newLine: false, text: `#${tag}` };
  if (TAG_LINE.test(lastLine)) return { newLine: false, text: `${/\s$/.test(lastLine) ? '' : ' '}#${tag}` };
  return { newLine: true, text: `#${tag}` };
}

// Adds `#tag` at the end of the body, on the trailing line of tags.
export function appendTagToBody(editorEl: HTMLElement, tag: string) {
  const giveBack = takeFocus(editorEl, true);
  const sel = window.getSelection();
  if (!sel) return giveBack();
  const end = document.createRange();
  end.selectNodeContents(editorEl);
  end.collapse(false);
  select(end);
  // The last position a caret can actually sit — inside the last block, not
  // after it — then read that paragraph without changing anything.
  sel.modify('move', 'forward', 'documentboundary');
  sel.modify('extend', 'backward', 'paragraphboundary');
  const lastLine = sel.toString();
  const lineStart = sel.getRangeAt(0).cloneRange();
  lineStart.collapse(true);
  const lastLineIsTask = checkboxBesideCaret(lineStart);
  sel.collapseToEnd();

  const plan = planTagInsertion(lastLine, tag, lastLineIsTask);
  if (plan.newLine) document.execCommand('insertParagraph');
  if (plan.newLine || !lastLine.trim()) leaveEnclosingBlock(editorEl);
  document.execCommand('insertText', false, plan.text);
  giveBack();
}

function caretWithin(editorEl: HTMLElement, selector: string): Element | null {
  const node = window.getSelection()?.anchorNode ?? null;
  const el = node && (node.nodeType === 3 ? node.parentElement : (node as Element));
  const hit = el?.closest(selector) ?? null;
  return hit && editorEl.contains(hit) ? hit : null;
}

// insertParagraph continues the block it is pressed in — a new <li>, another
// quoted line, more <pre> — so a tag line opened after a list became a bullet
// (measured: <ul><li>buy milk</li><li>#work</li></ul>). Step out to a plain
// line first. outdent lifts a list item one level, so nested lists repeat it.
function leaveEnclosingBlock(editorEl: HTMLElement) {
  for (let li = caretWithin(editorEl, 'li'), guard = 0; li && guard < 10; guard++) {
    document.execCommand('outdent');
    const next = caretWithin(editorEl, 'li');
    if (next === li) break;
    li = next;
  }
  if (caretWithin(editorEl, 'pre')) document.execCommand('formatBlock', false, '<div>');
  else tryExitSpecialBlock(editorEl);
}

// Turns the caret's paragraph into a checklist row: a checkbox at its start.
//
// The markup carries no contenteditable. Measured: insertHTML of a checkbox
// that DOES carry contenteditable="false" corrupts undo — Cmd+Z restores the
// attribute but leaves the checkbox in the body, stranded between blocks.
// Inserted bare and marked afterwards (an attribute change, which does not
// desynchronise the history), it undoes and redoes cleanly. The old
// `<div><input…></div>` markup also came out nested, <div><div><input>, on an
// empty line.
//
// A line that is already a task is left alone: a second checkbox made one row
// read as a task whose text starts with a checkbox (measured).
export function insertChecklist(editorEl: HTMLElement) {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return;
  const before = sel.getRangeAt(0).cloneRange();
  sel.modify('move', 'backward', 'paragraphboundary');
  if (checkboxBesideCaret(sel.getRangeAt(0))) {
    sel.removeAllRanges();
    sel.addRange(before);
    return;
  }
  document.execCommand('insertHTML', false, '<input type="checkbox">&nbsp;');
  ensureCheckboxesNotEditable(editorEl);
}

// True when a collapsed caret sits directly before or after a checkbox —
// WebKit puts the start of a task row on either side of it.
export function checkboxBesideCaret(caret: Range): boolean {
  const isCheckbox = (n: Node | null | undefined) =>
    !!n && n.nodeType === 1 && (n as Element).matches('input[type=checkbox]');
  const c = caret.startContainer;
  const o = caret.startOffset;
  if (c.nodeType === 3) {
    return (o === 0 && isCheckbox(c.previousSibling)) || (o === (c as Text).length && isCheckbox(c.nextSibling));
  }
  return isCheckbox(c.childNodes[o - 1]) || isCheckbox(c.childNodes[o]);
}
