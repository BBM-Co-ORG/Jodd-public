// Derives the one-line snippet shown under each note title in NoteList.
//
// Extracted from NoteList.svelte so the entity handling below can be pinned by
// plain unit tests. It was a private helper there that stripped tags and
// collapsed whitespace but never decoded entities, so a body containing
// Apple's routine `&nbsp;` rendered the six literal characters in the preview
// while the title line looked fine (the title comes from the message subject,
// which is already plain text and has nothing to decode). Reported 2026-08-14
// on a Microsoft/Exchange account, but the defect is backend-agnostic — Gmail
// bodies carry the same entities.
//
// Deliberately DOM-free rather than `DOMParser(...).body.textContent`: this
// runs for every note on every list render, and `textContent` would also fuse
// adjacent blocks (`<div>a</div><div>b</div>` → `ab`) instead of keeping the
// word boundary the tag-to-space substitution gives us.

/** Preview budget, in characters of decoded text (not of source HTML). */
export const PREVIEW_MAX_CHARS = 80;

/**
 * The named entities Apple Notes, Gmail and Exchange actually emit in note
 * bodies. Anything outside this set is left as written rather than dropped —
 * a stray `&foo;` in the preview is honest; silently deleting it is not.
 */
const NAMED_ENTITIES: Record<string, string> = {
  nbsp: ' ',
  amp: '&',
  lt: '<',
  gt: '>',
  quot: '"',
  apos: "'",
};

const ENTITY_RE = /&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z][a-zA-Z0-9]*);/g;

/** True for code points that are actually representable as text. */
function isEncodable(cp: number): boolean {
  return (
    Number.isFinite(cp) &&
    cp > 0 &&
    cp <= 0x10ffff &&
    // Lone surrogates are not characters; fromCodePoint accepts them but the
    // result is an unpaired surrogate that corrupts the string.
    !(cp >= 0xd800 && cp <= 0xdfff)
  );
}

/**
 * Decode HTML character references in ONE pass.
 *
 * The single pass is the point: `&amp;nbsp;` must come out as the literal text
 * `&nbsp;`, because that is what the note's author wrote. Re-decoding until
 * stable would wrongly turn it into a hard space.
 */
function decodeEntities(text: string): string {
  return text.replace(ENTITY_RE, (whole, body: string) => {
    if (body[0] === '#') {
      const isHex = body[1] === 'x' || body[1] === 'X';
      const cp = parseInt(isHex ? body.slice(2) : body.slice(1), isHex ? 16 : 10);
      // Out-of-range / malformed refs stay as source text. The note list
      // renders every note, so one bad body must not throw and blank the pane.
      return isEncodable(cp) ? String.fromCodePoint(cp) : whole;
    }
    return NAMED_ENTITIES[body] ?? whole;
  });
}

/**
 * Convert a note's `body_html` into the plain-text preview line.
 *
 * Step order is load-bearing:
 *  1. strip tags to spaces — so decoded angle brackets in step 2 can never be
 *     mistaken for markup and eaten (a body containing the text `&lt;b&gt;`
 *     would otherwise lose it, and the content after it, with no error);
 *  2. decode entities;
 *  3. collapse whitespace — after decoding, because `&nbsp;` yields U+00A0,
 *     which JS `\s` matches, so this is what actually normalises hard spaces;
 *  4. truncate — last, so entities are budgeted by their decoded length.
 */
export function notePreview(html: string): string {
  const stripped = (html ?? '').replace(/<[^>]*>/g, ' ');
  return decodeEntities(stripped).replace(/\s+/g, ' ').trim().slice(0, PREVIEW_MAX_CHARS);
}
