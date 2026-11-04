/** A run of changelog text: bold, code, both, or neither. */
export type InlineSpan = { text: string; bold: boolean; code: boolean };

type Token = { kind: 'text' | 'code' | 'mark'; text: string };

/**
 * Splits one CHANGELOG.md bullet into runs of plain text, `**bold**` and
 * `` `code` ``. Nothing else is markup: the caller renders each run as a text
 * node (never `{@html}`), so whatever the changelog says — tags, entities —
 * shows as written.
 *
 * Code is read first, so `` `**` `` is two asterisks in code, not the start of
 * a bold run (What's New showed the backticks; Galaxy Tab S7, 2026-10-21).
 * A code run inside a bold one is both. An unmatched `**` or a lone backtick
 * stays literal text.
 */
export function changelogInline(src: string): InlineSpan[] {
  // A code span, or a bold marker. At a backtick the code alternative is
  // tried first; with no closing backtick it fails and the backtick is text.
  const tokens: Token[] = [];
  const re = /`([^`]+)`|\*\*/g;
  let last = 0;
  for (let m = re.exec(src); m; m = re.exec(src)) {
    if (m.index > last) tokens.push({ kind: 'text', text: src.slice(last, m.index) });
    tokens.push(m[1] !== undefined ? { kind: 'code', text: m[1] } : { kind: 'mark', text: '**' });
    last = m.index + m[0].length;
  }
  if (last < src.length) tokens.push({ kind: 'text', text: src.slice(last) });

  // Bold markers pair in order (first with second, third with fourth), like
  // the lazy `\*\*(.+?)\*\*` this replaces: a pair needs something between
  // it, and an odd one out is text.
  const marks = tokens.flatMap((tok, i) => (tok.kind === 'mark' ? [i] : []));
  const toggles = new Set<number>();
  for (let k = 0; k + 1 < marks.length; ) {
    if (marks[k + 1] - marks[k] > 1) {
      toggles.add(marks[k]).add(marks[k + 1]);
      k += 2;
    } else {
      k += 1;
    }
  }

  const out: InlineSpan[] = [];
  const push = (text: string, bold: boolean, code: boolean) => {
    const prev = out[out.length - 1];
    if (prev && prev.bold === bold && prev.code === code && !code) prev.text += text;
    else out.push({ text, bold, code });
  };
  let bold = false;
  tokens.forEach((tok, i) => {
    if (toggles.has(i)) bold = !bold;
    else push(tok.text, bold, tok.kind === 'code');
  });
  return out;
}
