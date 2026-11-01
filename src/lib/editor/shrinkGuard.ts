// The editor's last line against pushing a note it failed to render: a save
// that would leave under a quarter of the note's text is refused. Both save
// paths use this — the debounced autosave and the save that runs when you
// switch notes or the editor goes — so neither can slip past the other.
//
// Both bodies are counted the same way, from their HTML. The guard once read
// the edited body through the editor's `innerText`, which leaves out a closed
// <details>; every AI-made note keeps its verbatim source in one, so editing
// a note whose source dwarfed its summary was always refused (2026-10-29).

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

/** The message to show when saving `newHtml` over `baselineHtml` (the body
 *  the backend last confirmed) would drop most of the note, or null when the
 *  save may go ahead. No baseline — a draft never confirmed — means there is
 *  nothing to measure against. */
export function shrinkRefusal(baselineHtml: string | undefined, newHtml: string): string | null {
  if (baselineHtml === undefined) return null;
  const before = bodyTextLength(baselineHtml);
  if (before < MIN_TEXT_TO_JUDGE) return null;
  const after = bodyTextLength(newHtml);
  if (after / before >= MIN_KEPT_RATIO) return null;
  return `Autosave blocked: would shrink note from ${before} to ${after} chars. If you intended to clear the note, delete it instead.`;
}
