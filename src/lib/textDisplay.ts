// Making an invisible character visible — the Thai orphaned-combining-mark bug.
//
// Measured live 2026-09-09 (`icloud:kaiwan@me.com`): a note whose title was a
// single Thai sara uee `ื` (a combining vowel with no consonant under it)
// rendered as a BLANK title in Jodd on Windows, while iPhone/iCloud showed it
// as `◌ื`. The character is really there — Jodd's own log printed the
// title buffer as exactly `[0E37]`, and it round-tripped to CloudKit correctly
// — so this is not data loss and not a sync bug. It is a rendering difference:
// WebView2 on Windows (with IBM Plex Sans Thai) draws a lone combining mark
// with nothing under it, where iOS follows the Unicode recommendation and
// inserts a DOTTED CIRCLE (U+25CC) base for a "defective combining character
// sequence".
//
// So a title that is only — or starts with — an orphaned combining mark looks
// empty in Jodd, and the user cannot see the character to delete it. These
// helpers make it visible the way iOS does, for DISPLAY ONLY: the stored title
// is never changed (that would be silently editing the user's data), the
// dotted circle is inserted only in what the eye sees.

// One `\p{M}` matcher (Unicode Mark: Mn/Mc/Me). No `g` flag, so `.test` carries
// no `lastIndex` state and is safe to reuse.
const COMBINING_MARK = /\p{M}/u;

/** The leading run of combining marks that have no base — the orphaned prefix
 *  that renders invisibly on Windows. `''` when the title starts with an
 *  ordinary character (the overwhelmingly common case). */
export function leadingOrphanMarks(title: string): string {
  const m = title.match(/^\p{M}+/u);
  return m ? m[0] : '';
}

/** Whether the title begins with an orphaned combining mark — i.e. it has an
 *  invisible-on-Windows leading character the user probably cannot see. */
export function startsWithOrphanCombiningMark(title: string): boolean {
  return leadingOrphanMarks(title) !== '';
}

/** A title as it should be DISPLAYED, never stored. A dotted circle (U+25CC) is
 *  inserted before any combining mark that lacks a base — at the start of the
 *  string, or after a run of other orphaned marks — matching Apple/iOS and the
 *  Unicode recommendation for defective combining sequences. Ordinary Thai
 *  (`สวัสดี`: each vowel/tone mark follows its consonant) is returned
 *  unchanged, because those marks all have a base to sit on. */
export function displayTitle(title: string): string {
  // Fast path: no marks at all, or a normal leading character — nothing to do.
  if (!COMBINING_MARK.test(title) || !startsWithOrphanCombiningMark(title)) {
    return title;
  }
  let out = '';
  let hasBase = false;
  for (const ch of title) {
    // ch is a whole code point (the iterator handles surrogate pairs).
    const isMark = COMBINING_MARK.test(ch);
    if (isMark && !hasBase) {
      out += '◌'; // the dotted circle becomes the base for any marks that stack after it
      hasBase = true;
    }
    out += ch;
    if (!isMark) {
      hasBase = true;
    }
  }
  return out;
}
