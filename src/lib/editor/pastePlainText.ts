// Pasting text that came with no HTML (a URL copied from a browser's address
// bar is the common case).
//
// It used to go through execCommand('insertText'). WebKit folds that into the
// typing command that is still open, so a second paste — and any typing right
// before it — became part of the same undo step: two pastes undid together.
// Measured 2026-09-30 in scripts/webkit-editor-harness (keys/paste-*.txt), real
// Cmd+V: two plain pastes → one Cmd+Z removed both; "xy" typed then pasted →
// one Cmd+Z removed both; the same two pastes as HTML → one Cmd+Z each.
//
// insertHTML is a replace-selection command of its own, so each paste is one
// step. The markup mirrors what insertText builds for a multi-line string —
// first line inline, every later line its own <div>, an empty line <div><br></div>
// — measured identical in the harness (keys/paste-multiline-*.txt); a <br>-joined
// version left bare text nodes outside any block.
function escapeText(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// insertText writes a trailing space as &nbsp; itself; as markup it would be an
// ordinary space, which rendering collapses. Measured: ' a  b\t c   ' via
// insertText ends ' &nbsp;&nbsp;' (a run keeps its first space, the rest are
// &nbsp;; a lone one is &nbsp;), via plain insertHTML it ends '   '. The leading
// spaces of a line that starts its own block are collapsed the same way and are
// all made &nbsp; here — the same on screen; that case was not compared byte for
// byte. (Interior runs and a first line's leading space came out identical.)
function keepEdgeSpaces(line: string, startsBlock: boolean): string {
  const nbsps = (n: number) => '&nbsp;'.repeat(n);
  const head = startsBlock ? line.replace(/^ +/, (m) => nbsps(m.length)) : line;
  return head.replace(/ +$/, (m) => (m.length === 1 ? nbsps(1) : ' ' + nbsps(m.length - 1)));
}

export function plainTextToPasteHtml(text: string): string {
  const lines = text.split(/\r\n|\r|\n/).map((line, i) => keepEdgeSpaces(escapeText(line), i > 0));
  const [first, ...rest] = lines;
  return first + rest.map((line) => `<div>${line === '' ? '<br>' : line}</div>`).join('');
}

// False when nothing was pasted. An empty insertHTML is not a harmless no-op —
// away from the start of a block it splits the block (harness README) — so an
// empty string never reaches it.
export function pastePlainText(text: string): boolean {
  if (text === '') return false;
  document.execCommand('insertHTML', false, plainTextToPasteHtml(text));
  return true;
}
