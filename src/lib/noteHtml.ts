// Where note content lives inside a stored body — the frontend twin of
// `mime822::append_html` / `mime822::body_inner` (gotcha #37).
//
// A body the editor saved is a whole document (`wrapBody` in NoteEditor):
// `<html><head></head><body …>…</body></html>`. Appending by string
// concatenation put new content after `</html>`, which the editor's old
// `<body>…</body>` regex never read: the editor showed nothing, and its next
// save dropped the content. Found in the note-provenance live pass, 2026-10-08.

const BODY = /<body[^>]*>([\s\S]*)<\/body>([\s\S]*)$/i;

// Whatever follows the closing </html> (or </body> when there is none).
function stranded(afterBody: string): string {
  const t = afterBody.trimStart();
  return t.toLowerCase().startsWith('</html>') ? t.slice('</html>'.length) : t;
}

/** The content of a note body: inside `<body>` plus anything stranded after
 *  the document by an older build; a fragment as-is. */
export function bodyContent(html: string): string {
  const m = html.match(BODY);
  return m ? m[1] + stranded(m[2]) : html;
}

/** Append `fragment` inside the document, never after `</html>`. Content an
 *  older build stranded after the document moves back inside, ahead of it. */
export function appendHtml(existing: string, fragment: string): string {
  const close = existing.toLowerCase().lastIndexOf('</body>');
  if (close < 0 || !/<body[^>]*>/i.test(existing.slice(0, close))) return existing + fragment;
  const lost = stranded(existing.slice(close + '</body>'.length));
  return lost
    ? `${existing.slice(0, close)}${lost}${fragment}</body></html>`
    : `${existing.slice(0, close)}${fragment}${existing.slice(close)}`;
}
