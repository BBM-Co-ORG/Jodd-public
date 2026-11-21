// #-autocomplete in the body (spec 2026-10-20 §6.4): when the `#` being typed
// asks for the account's tags, which ones it shows, and the list keys it
// shares with the `[[` picker. Pure: NoteEditor reads the caret and the
// vocabulary, and does the insert.
//
// Never on an `inline_objects` account (iCloud, gotcha #38): a `#word` there
// is text, not a tag, so suggesting one would mislead. That gate is
// NoteEditor's — these helpers know nothing about accounts.

import { normalizeTag } from './bodyTags';

export const HASHTAG_PICKER_LIMIT = 8;

// A tag's own characters — extractHashtags' set (bodyTags.ts, mirroring
// db::is_tag_word_char): letters, digits, `_`, `-` and combining marks, so
// Thai vowel and tone marks are part of the word, never a break in it.
const TAG_CHAR = '[\\p{L}\\p{N}\\p{M}_\\-]';
// The `#` must start a word: at the start of the text, or after anything that
// is not a tag character — whitespace or punctuation. `_` and `-` are
// punctuation to Unicode but tag characters here; after them extractHashtags
// would not read the result as a tag, so they do not trigger.
const TRIGGER = new RegExp(`(?:^|[^\\p{L}\\p{N}\\p{M}_\\-])#(${TAG_CHAR}*)$`, 'u');
// `https://x.com/#anchor`, `x.com/#`, `mailto:#x`: a `#` inside a token with a
// `:`, `/` or `.` before it is a URL fragment, not a tag.
const URL_LIKE = /\S*[:/.]\S*#/;

/**
 * The prefix typed after the `#` that ends `textBeforeCaret`, or null when
 * that text is not a hashtag being typed. `textBeforeCaret` is the caret's
 * text node up to the caret (a `#` at the start of a text node triggers).
 * The prefix holds tag characters only, so whitespace or punctuation after it
 * — `#stu ` — closes the list.
 */
export function hashtagQueryAtCaret(textBeforeCaret: string, inCodeOrLink: boolean): string | null {
  if (inCodeOrLink) return null;
  const m = textBeforeCaret.match(TRIGGER);
  if (!m) return null;
  const token = textBeforeCaret.slice(textBeforeCaret.search(/\S*$/));
  if (URL_LIKE.test(token)) return null;
  return m[1];
}

/**
 * Up to `limit` tags from `vocabulary` for `prefix`: those starting with it
 * (after normalizeTag) first, then those containing it. Each group keeps the
 * vocabulary's order — tagsByAccount's is alphabetical. An empty prefix
 * matches every tag.
 */
export function rankTags(prefix: string, vocabulary: string[], limit = HASHTAG_PICKER_LIMIT): string[] {
  const q = normalizeTag(prefix);
  const starts: string[] = [];
  const contains: string[] = [];
  const seen = new Set<string>();
  for (const tag of vocabulary) {
    const t = normalizeTag(tag);
    if (!t || seen.has(t)) continue;
    seen.add(t);
    if (t.startsWith(q)) starts.push(t);
    else if (t.includes(q)) contains.push(t);
  }
  return [...starts, ...contains].slice(0, limit);
}

/**
 * The tags the picker may offer: the account's tag cloud (tagsByAccount),
 * minus the `typing` tag when the open note is its only holder. The open
 * note's tags reach the store live as they are typed (refreshBodyTags), so a
 * half-typed `#stu` is already in the cloud — listing it would suggest what
 * was just typed, and "no match" could never be empty. `ownTags` is the open
 * note's entry in the store (empty for a note not yet saved).
 */
export function hashtagVocabulary(
  cloud: { tag: string; count: number }[],
  ownTags: string[],
  typing: string,
): string[] {
  const t = normalizeTag(typing);
  const ownTyping = ownTags.includes(t);
  return cloud.filter((c) => !(c.tag === t && ownTyping && c.count <= 1)).map((c) => c.tag);
}

/**
 * Ruling R31: does Enter (or Tab) take the highlighted tag? Only for a
 * non-empty prefix, and only once the user moved the highlight or the top
 * row is a prefix match — `#` + Enter is a new line, and `#ud` + Enter must
 * not turn into `#study` just because "study" contains "ud". Shift+Enter and
 * Shift+Tab never pick (NoteEditor).
 */
export function enterPicks(query: string, items: string[], moved: boolean): boolean {
  const q = normalizeTag(query);
  if (!q || items.length === 0) return false;
  return moved || normalizeTag(items[0]).startsWith(q);
}

// Elements that end a line of text: a caret's context never reads across one.
const BLOCK = 'div,p,li,ul,ol,h1,h2,h3,h4,h5,h6,blockquote,pre,table,tr,td,th';
const isTagChar = (ch: string) => new RegExp(`^${TAG_CHAR}`, 'u').test(ch);

export interface CaretTextContext {
  /** The caret's line up to the caret, across inline elements (`<span>a</span>#b` reads `a#b`); a `<br>` or a nested block reads as a line break. */
  before: string;
  /** Does the text right after the caret, on the same line, continue a tag word (`#wo|rk`)? */
  continuesWord: boolean;
  /** The text node and offset holding index `i` of `before`. */
  locate(i: number): { node: Text; offset: number } | null;
}

/**
 * The text around a caret in a text node, on its own line: from the start of
 * the caret's block (the nearest BLOCK ancestor inside `root`, else `root`)
 * to the caret, and the first character after it before the line ends.
 */
export function caretTextContext(root: Element, caret: Text, offset: number): CaretTextContext {
  let block: Element = root;
  for (let el = caret.parentElement; el && el !== root; el = el.parentElement) {
    if (el.matches(BLOCK)) { block = el; break; }
  }
  const segs: { node: Text; at: number; len: number }[] = [];
  let before = '';
  let afterChar = '';
  let phase: 'before' | 'after' = 'before';
  const w = document.createTreeWalker(block, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT);
  for (let n = w.nextNode(); n; n = w.nextNode()) {
    const lineBreak = n.nodeType === 1 && ((n as Element).tagName === 'BR' || (n as Element).matches(BLOCK));
    if (phase === 'before') {
      if (lineBreak) { before += '\n'; continue; }
      if (n.nodeType !== 3) continue;
      const t = n as Text;
      if (t === caret) {
        segs.push({ node: t, at: before.length, len: offset });
        before += t.data.slice(0, offset);
        afterChar = t.data.slice(offset, offset + 2);
        if (afterChar) break;
        phase = 'after';
      } else {
        segs.push({ node: t, at: before.length, len: t.data.length });
        before += t.data;
      }
    } else {
      if (lineBreak) break;
      if (n.nodeType === 3 && (n as Text).data) { afterChar = (n as Text).data.slice(0, 2); break; }
    }
  }
  return {
    before,
    continuesWord: afterChar !== '' && isTagChar(afterChar),
    locate(i) {
      for (const s of segs) if (i >= s.at && i < s.at + s.len) return { node: s.node, offset: i - s.at };
      return null;
    },
  };
}

export type PickerKeyAction = { move: number } | 'pick' | 'close' | null;

/**
 * What a key does to an open caret list (the `[[` picker, the # picker):
 * ↑/↓ move (wrapping), Enter/Tab pick, Esc closes. Null — the key is the
 * editor's — for any other key, an empty list, or a key pressed while an IME
 * is composing (Thai, Android keyboards): Enter there commits the composition,
 * and taking it would pick a tag from a half-typed word.
 */
export function pickerKeyAction(
  key: string,
  index: number,
  count: number,
  ev: { isComposing?: boolean; keyCode?: number } = {},
): PickerKeyAction {
  if (ev.isComposing || ev.keyCode === 229) return null;
  if (count <= 0) return null;
  if (key === 'ArrowDown') return { move: (index + 1) % count };
  if (key === 'ArrowUp') return { move: (index - 1 + count) % count };
  if (key === 'Enter' || key === 'Tab') return 'pick';
  if (key === 'Escape') return 'close';
  return null;
}
