// The editor's own CSS, as WebKit writes it into the note.
//
// When Backspace or Delete joins two lines, WebKit wraps the text it moves in
// a <span style> that reproduces the style it had where it came from, so it
// looks the same where it lands. It leaves the span out when that style
// matches the line it lands in — but it compares the DECLARED values, and the
// editor root declares its font as CSS variables (`font-family:
// var(--font-note); font-size: var(--size-md)`), which it cannot resolve
// to compare. So every merge wrapped the moved text in a copy of them.
// Measured 2026-11-17 in WKWebView: with plain values on the root no span
// appears; with variables it appears even when the root only inherits them.
//
// The variables are Jodd's and mean nothing anywhere else: in Gmail or
// Notes.app they resolve to nothing and the text inherits, exactly as it
// would with no declaration. So dropping them changes how no reader shows
// the note — it only keeps Jodd's stylesheet out of the user's notes.
//
// Applied to what the editor SAVES, never to the live DOM: changing the DOM
// behind WebKit's back desynchronises its undo history (markdownTriggers.ts).

const LEAKED = /var\(\s*--/i;

export function stripLeakedEditorStyles(html: string): string {
  // Typing calls this on every keystroke; almost every body has no leak.
  if (!LEAKED.test(html)) return html;
  const tpl = document.createElement('template');
  tpl.innerHTML = html;
  const styled = Array.from(tpl.content.querySelectorAll<HTMLElement>('[style]')).filter((el) =>
    LEAKED.test(el.getAttribute('style') ?? ''),
  );
  if (!styled.length) return html;
  for (const el of styled) {
    const kept = (el.getAttribute('style') ?? '')
      .split(';')
      .map((d) => d.trim())
      .filter((d) => d && !LEAKED.test(d));
    if (kept.length) el.setAttribute('style', `${kept.join('; ')};`);
    else el.removeAttribute('style');
  }
  // A span left with no attributes is only the wrapper WebKit added.
  for (const el of styled) {
    if (el.tagName !== 'SPAN' || el.attributes.length) continue;
    el.replaceWith(...Array.from(el.childNodes));
  }
  return tpl.innerHTML;
}
