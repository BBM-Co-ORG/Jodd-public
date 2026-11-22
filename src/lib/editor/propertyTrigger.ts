// Authoring help for properties and due dates (spec 2026-11-19-properties-
// tasks-views §7): when the text before the caret asks for the key picker,
// the value picker or the date picker; what each lists; where Add property
// and Due date put their text; which fix a wavy underline offers. Pure:
// NoteEditor reads the caret, the lines and the vocabulary, and does every
// edit — and only on an explicit pick, click or toolbar press. Nothing here,
// or anything that calls it on open, paint or save, rewrites a body (the
// `migrate_tags_to_body` incident).
//
// The grammar is props/grammar.ts (pinned to the Rust deriver); the line
// split is propertyPaint.ts `bodyLines` (pinned to `html_to_lines`); the
// wavy-underline gate is propertyPaint.ts `warnFix`. Each is used, never
// re-implemented, so the trigger, the paint and the derived data agree.

import { enterPicks } from './hashtagTrigger';
import { parseDue, parsePropertyBlock, parsePropertyLine } from '../props/grammar';
import { dueOffset, warnFix } from './propertyPaint';

export type PropertyTrigger =
  | { kind: 'key'; prefix: string }
  | { kind: 'value'; key: string; prefix: string }
  | { kind: 'date' }
  | null;

/** `property_vocabulary` (Task 8): `[key display, notes using it, top values]`, most used first. */
export type PropertyVocabulary = [key: string, count: number, values: string[]][];

export const PROPERTY_PICKER_LIMIT = 8;

/** A row of the property picker (components/PropertyPicker.svelte). */
export interface PickerItem {
  /** What the row shows. */
  label: string;
  /** What a pick writes. */
  value: string;
  /** Shown muted after the label (a quick date's ISO form). */
  detail?: string;
}

const MAX_KEY_CHARS = 32;
// A key being typed: what parsePropertyLine accepts before its colon — a
// letter first, then letters, digits, marks (Thai vowels and tones), spaces,
// `-` and `_`. A trailing space is allowed while typing (`Due d…`).
const KEY_SO_FAR = /^\p{L}[\p{L}\p{N}\p{M} _-]*$/u;
// `📅 ` (with or without U+FE0F) or `due: ` — the markers parseDue reads, its
// `due:` only where a word does not run into it (`overdue:` is not one) —
// then at least one space, then at most a partial date.
const DATE_MARK = /(?:\u{1F4C5}️?|(?<![\p{Alphabetic}\p{N}])due:)[ \t]+([0-9-]{0,10})$/iu;
const FULL_DATE = /^\d{4}-\d{2}-\d{2}$/;

const norm = (s: string) => s.replace(/ /g, ' ');

/**
 * Where the date goes when the text before the caret asks for the date
 * picker: the index just after the marker and its space(s), so a pick
 * replaces a partly typed date. Null when it does not ask.
 */
export function dateTriggerStart(textBeforeCaret: string): number | null {
  const text = norm(textBeforeCaret);
  const m = DATE_MARK.exec(text);
  if (!m || FULL_DATE.test(m[1])) return null;
  return text.length - m[1].length;
}

/**
 * Which picker the caret asks for. `lineIndexInBlock`: the caret's line
 * within the property block — 0 for the first body line, up to and including
 * the block's length (the line that would extend it) — or null for any other
 * line. `textBeforeCaret`: that line, from its start to the caret.
 *
 * - `date` after `📅 `, `due: ` or a `Due: ` property, on any line, until a
 *   whole date is there.
 * - `value` after `Key: ` on a block line.
 * - `key` while a non-empty key is typed on a block line, before its colon.
 */
export function propertyTriggerAt(lineIndexInBlock: number | null, textBeforeCaret: string): PropertyTrigger {
  if (dateTriggerStart(textBeforeCaret) !== null) return { kind: 'date' };
  if (lineIndexInBlock === null) return null;
  const text = norm(textBeforeCaret);
  const colon = text.indexOf(':');
  if (colon < 0) {
    if (!KEY_SO_FAR.test(text) || Array.from(text).length > MAX_KEY_CHARS) return null;
    return { kind: 'key', prefix: text };
  }
  // A key, `:`, at least one space: what remains is the value so far.
  const key = text.slice(0, colon);
  if (!/^ +/.test(text.slice(colon + 1)) || !parsePropertyLine(`${key}: x`)) return null;
  return { kind: 'value', key, prefix: text.slice(colon + 1).trimStart() };
}

/**
 * Where Add property puts its `Key: ` line: the end of the property block —
 * the block stays contiguous — or body line 0 when there is none (directly
 * under the title: the title is the editor's own field, not a body line).
 */
export function insertAddPropertyLine(bodyLines: string[]): { lineIndex: number } {
  return { lineIndex: parsePropertyBlock(bodyLines).length };
}

function rank(prefix: string, candidates: string[], limit: number): string[] {
  const q = prefix.toLowerCase();
  const starts: string[] = [];
  const contains: string[] = [];
  for (const c of candidates) {
    const l = c.toLowerCase();
    if (l.startsWith(q)) starts.push(c);
    else if (l.includes(q)) contains.push(c);
  }
  return [...starts, ...contains].slice(0, limit);
}

/** The account's keys for `prefix`: starting with it first, then containing it; most used first in each. */
export function rankKeys(prefix: string, vocabulary: PropertyVocabulary, limit = PROPERTY_PICKER_LIMIT): string[] {
  return rank(prefix, vocabulary.map(([k]) => k), limit);
}

/**
 * The values used with `key` (matched case-insensitively, as keys compare)
 * for `prefix`. A value already typed in full is left out: listed, it would
 * own Enter, and Enter after a finished value is a new line.
 */
export function rankValues(key: string, prefix: string, vocabulary: PropertyVocabulary, limit = PROPERTY_PICKER_LIMIT): string[] {
  const k = key.trim().toLowerCase();
  const values = vocabulary.find(([v]) => v.toLowerCase() === k)?.[2] ?? [];
  const p = prefix.toLowerCase();
  return rank(prefix, values.filter((v) => v.toLowerCase() !== p), limit);
}

/**
 * Does Enter (or Tab) take the highlighted row? The # list's rule
 * (`enterPicks`, ruling R31) for a typed prefix; and, since a property list
 * also opens with nothing typed (after `Key: `, from Add property), a row the
 * user moved the highlight to — but never the top row of an untouched list.
 *
 * `onlyIfMoved` (ruling R13): a key list in a note with no property block
 * yet. There, body line 0 of every note is the block position, and a first
 * word like `Note` or `Project` is prose — Enter must stay a new line unless
 * the user moved the highlight onto a key. A click on a row still picks.
 */
export function propertyEnterPicks(prefix: string, items: string[], moved: boolean, onlyIfMoved = false): boolean {
  if (items.length === 0) return false;
  if (onlyIfMoved) return moved;
  return moved || enterPicks(prefix, items, moved);
}

/**
 * The fix-it for line `index` of the body (`bodyLines` texts): the whole
 * line as it would be rewritten, or null. Offered only where propertyPaint
 * draws `prop-warn` — a block line, or the one line that ends the block —
 * and only the fix that warning stands for (`warnFix`).
 */
export function fixItAt(lines: string[], index: number, today: Date): string | null {
  if (index < 0 || index >= lines.length) return null;
  const n = parsePropertyBlock(lines).length;
  if (index > n) return null;
  return warnFix(lines[index], index < n, today);
}

/** An edit in a line's text offsets (`bodyLines` text): replace `[start, end)` with `text`. */
export interface LineEdit {
  start: number;
  end: number;
  text: string;
}

/**
 * The fix-it for line `index` as the MINIMAL edit (ruling R12) — a space
 * inserted after the colon, and/or the date's characters replaced with the
 * ISO date — so every other node on the line (a link, a bold key) is left
 * alone; plus a label that says what changes. Null where `fixItAt` offers
 * nothing. The edits, applied to the line, give exactly `fixItAt`'s line.
 */
export function fixItEditAt(lines: string[], index: number, today: Date): { edits: LineEdit[]; label: string } | null {
  const fixed = fixItAt(lines, index, today);
  if (fixed === null) return null;
  const text = norm(lines[index]);
  const colon = text.indexOf(':');
  if (colon < 0) return null;
  const key = text.slice(0, colon);
  const rest = text.slice(colon + 1);
  const value = rest.trim();
  const newValue = fixed.slice(fixed.indexOf(':') + 2);
  const space = !rest.startsWith(' ');
  const vStart = colon + 1 + (rest.length - rest.trimStart().length);
  const vEnd = vStart + value.length;
  const parts: string[] = [];
  let edits: LineEdit[];
  if (space) parts.push(`add a space after “${key}:”`);
  if (newValue !== value) parts.push(`${value} → ${newValue}`);
  if (space && newValue !== value) edits = [{ start: vStart, end: vEnd, text: ` ${newValue}` }];
  else if (space) edits = [{ start: colon + 1, end: colon + 1, text: ' ' }];
  else edits = [{ start: vStart, end: vEnd, text: newValue }];
  // The edit must give exactly the line fixItAt offers, or nothing is offered.
  let out = text;
  for (const e of [...edits].reverse()) out = out.slice(0, e.start) + e.text + out.slice(e.end);
  if (out !== fixed || parts.length === 0) return null;
  return { edits, label: parts.join(', ') };
}

/**
 * Toolbar Due date on a task row whose text (propertyPaint `rowText`) is
 * `rowText`: the edit, in that text's offsets. The date the parser reads
 * (the last valid marker's) is replaced — a row never gets a second marker;
 * a marker typed without a date gets one; otherwise ` 📅 <date>` is
 * appended, the canonical form (§6.2).
 */
export function dueMarkerEdit(rowText: string, date: string): { start: number; end: number; text: string } {
  const due = parseDue(rowText);
  if (due !== null) {
    const i = dueOffset(rowText, due);
    if (i >= 0) return { start: i, end: i + due.length, text: date };
  }
  const bare = /(?:\u{1F4C5}️?|(?<![\p{Alphabetic}\p{N}])due:)([ \t ]*)$/iu.exec(rowText);
  if (bare) return { start: rowText.length, end: rowText.length, text: bare[1] ? date : ` ${date}` };
  const spaced = rowText === '' || /[\s ]$/.test(rowText);
  return { start: rowText.length, end: rowText.length, text: `${spaced ? '' : ' '}\u{1F4C5} ${date}` };
}

const pad = (n: number) => String(n).padStart(2, '0');

/** The device's local calendar day as `YYYY-MM-DD`. */
export function isoDay(d: Date): string {
  return `${String(d.getFullYear()).padStart(4, '0')}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** The date picker's quick rows. */
export function dateChoices(today: Date): { label: string; value: string }[] {
  const plus = (n: number) => isoDay(new Date(today.getFullYear(), today.getMonth(), today.getDate() + n));
  return [
    { label: 'Today', value: plus(0) },
    { label: 'Tomorrow', value: plus(1) },
    { label: 'In a week', value: plus(7) },
  ];
}
