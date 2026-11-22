// The editor's last line against pushing a note it failed to render: a save
// that would leave under a quarter of the note's text is refused. Both save
// paths use this — the debounced autosave and the save that runs when you
// switch notes or the editor goes — so neither can slip past the other.
//
// Both bodies are counted the same way, from their HTML. The guard once read
// the edited body through the editor's `innerText`, which leaves out a closed
// <details>; every AI-made note keeps its verbatim source in one, so editing
// a note whose source dwarfed its summary was always refused (2026-10-29).
//
// The message names the note and says which save refused it. A refusal on
// 2026-11-22 (151 characters down to 19) could not be traced: nothing said
// which note it was, and the refused body is kept nowhere.
import { plural } from '../errorText';
import { displayTitle } from '../textDisplay';

const MIN_TEXT_TO_JUDGE = 20;
const MIN_KEPT_RATIO = 0.25;

/** The text length of a note body's HTML: what is inside <body> (the whole
 *  string when there is none), without tags, <style> or <script>. */
export function bodyTextLength(html: string): number {
  const inner = html.match(/<body[^>]*>([\s\S]*)<\/body>/i)?.[1] ?? html;
  return inner
    .replace(/<(style|script)\b[^>]*>[\s\S]*?<\/\1>/gi, ' ')
    .replace(/<[^>]*>/g, ' ')
    .replace(/&nbsp;/gi, ' ')
    .replace(/\s+/g, ' ')
    .trim().length;
}

/** Which save the guard stopped: the autosave while you type (the edit stays
 *  in the editor), or the save when you leave the note (the note goes back
 *  to what the backend last confirmed). */
export type ShrinkPath = 'autosave' | 'leaving';

export interface Shrink {
  before: number;
  after: number;
}

/** The before/after text lengths when saving `newHtml` over `baselineHtml`
 *  (the body the backend last confirmed) would drop most of the note, or
 *  null when the save may go ahead. No baseline — a draft never confirmed —
 *  means there is nothing to measure against. */
export function measureShrink(baselineHtml: string | undefined, newHtml: string): Shrink | null {
  if (baselineHtml === undefined) return null;
  const before = bodyTextLength(baselineHtml);
  if (before < MIN_TEXT_TO_JUDGE) return null;
  const after = bodyTextLength(newHtml);
  if (after / before >= MIN_KEPT_RATIO) return null;
  return { before, after };
}

/** The message for a refused save of the note titled `title`. */
export function shrinkRefusalText({ before, after }: Shrink, title: string, path: ShrinkPath): string {
  const name = displayTitle(title.trim()) || 'Untitled';
  const outcome = path === 'leaving' ? ' The note is back as it was last saved.' : '';
  return `Jodd didn't save your edit to “${name}”: it would remove most of the note (${plural(before, 'character')} down to ${after.toLocaleString('en-US')}).${outcome} To clear a note, delete it instead.`;
}

/** measureShrink and shrinkRefusalText in one: the message, or null. */
export function shrinkRefusal(
  baselineHtml: string | undefined,
  newHtml: string,
  title = '',
  path: ShrinkPath = 'autosave',
): string | null {
  const shrink = measureShrink(baselineHtml, newHtml);
  return shrink ? shrinkRefusalText(shrink, title, path) : null;
}
