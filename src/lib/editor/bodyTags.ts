// What counts as a tag in the open note — the editor's half of the rule
// db::tags_for_account applies in Rust. The two must agree, or a chip comes
// back while the user types that the database says is not there.
//
// The rule depends on the backend (`TagModel`, backend/mod.rs; gotcha #38):
//   - body_text: a `#word` in the text is a tag. Gmail, Outlook, LocalFs, SSH
//     — Apple never makes a real tag on those accounts, so the text is the
//     only place a tag can live.
//   - inline_objects: only a hashtag span is a tag. iCloud, where Apple's tag
//     is an inline object Jodd renders as `data-jodd-inline="hashtag"`
//     (icloud/format_html.rs). A `#word` in the text is what Apple's Delete
//     Tag and Convert to Text leave behind, and Jodd cannot yet create the
//     object, so a `#tag` typed here would be a tag nowhere but in Jodd.

import type { ModalWorkflow } from '../types';

export type TagModel = 'body_text' | 'inline_objects';

// Mirror of db::normalize_tag: trim, lowercase, drop whitespace, control
// chars and every '#'. Unicode-friendly (Thai, emoji survive) — \p{White_Space}
// and \p{Cc} match Rust's is_whitespace()/is_control().
export function normalizeTag(raw: string): string {
  return raw.trim().toLowerCase().replace(/[#\p{White_Space}\p{Cc}]/gu, '');
}

// Mirror of db::extract_hashtags: # at a word boundary, then
// letters/digits/_/HYPHEN/combining-marks (\p{M} → Thai etc.), requiring a
// letter (so #2024 isn't a tag). Lowercased, de-duplicated, first-seen order.
//
// Hyphen support added 2026-06-13 to match the Rust side (db.rs is_tag_word_char).
// Compound kebab-case tags like #database-migrations and #local-first must parse
// as one tag, not truncate at the dash. Word-boundary on the LEFT also excludes
// hyphen so `foo-#bar` still recognizes #bar (hyphen before # is a boundary).
export function extractHashtags(text: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const re = /(?:^|[^\p{L}\p{N}_-])#([\p{L}\p{N}_\-\p{M}]+)/gu;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const raw = m[1];
    if (!/\p{L}/u.test(raw)) continue;
    const norm = raw.toLowerCase();
    if (!seen.has(norm)) { seen.add(norm); out.push(norm); }
  }
  return out;
}

// The span contract is pinned in icloud/format_html.rs (INLINE_KIND_ATTR,
// INLINE_KIND_HASHTAG); grep for it there before changing this selector.
const HASHTAG_SPAN = 'span[data-jodd-inline="hashtag"]';

// The open note's tags, under its account's model (resolve it with
// `tagModelOf`, which is never undefined). `undefined` reads as body_text.
export function tagsInEditor(root: HTMLElement, model: TagModel | undefined): string[] {
  if (model !== 'inline_objects') {
    // innerText, not textContent: it puts line breaks between blocks, so a
    // `#tag` opening a line is still at a word boundary. jsdom lacks it.
    return extractHashtags(root.innerText ?? root.textContent ?? '');
  }
  // A span is one tag by construction — `#` + Apple's DisplayText — so it is
  // normalized, never re-tokenized (which would cut `#lab😀` to `lab`).
  const out: string[] = [];
  for (const el of root.querySelectorAll(HASHTAG_SPAN)) {
    const tag = normalizeTag(el.textContent ?? '');
    if (tag && !out.includes(tag)) out.push(tag);
  }
  return out;
}

// The tag model for an account: its capability once loaded, and until then
// the backend its id names — the same prefix rule as db::tag_model_for_account
// (`{backend}:{email}`, gotcha #18's account identity), so the two can never
// disagree. Without the fallback an iCloud note's first render used the text
// rule: the chip of a tag Apple had deleted, and an add-tag field writing
// `#tag` text into Apple's note. A legacy bare id predates iCloud.
export function tagModelOf(
  caps: { tag_model?: TagModel } | undefined,
  accountId: string | null | undefined,
): TagModel {
  if (caps?.tag_model) return caps.tag_model;
  return accountId?.startsWith('icloud:') ? 'inline_objects' : 'body_text';
}

// Can Jodd add or remove a tag here? Not where a tag is an object Jodd cannot
// create: adding would only write `#tag` text, which Apple never treats as a
// tag. Pass `tagModelOf(..)`, so an iCloud note is display-only from its
// first render rather than once capabilities arrive.
export function tagsEditable(model: TagModel | undefined): boolean {
  return model !== 'inline_objects';
}

// Where Extract, the LLM workflows and URL ingest save the LLM's suggested
// tags as a plain "Keywords:" line instead of tags — the frontend half of
// llm::markdown::tags_line. Separate from tagsEditable on purpose: once Jodd
// can create Apple's hashtag object, tags may become creatable here before
// the chip row becomes editable. `undefined` (capabilities loading) → false:
// say nothing rather than guess.
export function suggestedTagsBecomeKeywords(model: TagModel | undefined): boolean {
  return model === 'inline_objects';
}

export const KEYWORDS_HINT = "On iCloud, suggested tags are saved as a Keywords line. Jodd can't create Apple tags yet.";

// The Extract modal's hint. Action items never carry tags
// (llm::meeting::envelope builds `tags: vec![]`), and a raw transcript
// ingest (`clean` false) makes no LLM call at all.
export function keywordsHintShown(model: TagModel | undefined, workflow: ModalWorkflow, clean: boolean): boolean {
  if (!suggestedTagsBecomeKeywords(model)) return false;
  if (workflow === 'action_items') return false;
  if (workflow === 'transcript' && !clean) return false;
  return true;
}
