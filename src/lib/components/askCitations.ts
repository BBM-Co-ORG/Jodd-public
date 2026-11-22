export type CitedNote = {
  uuid: string;
  account_id: string;
  title: string;
  slug: string;
};

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/**
 * Render an Ask Jodd answer as HTML: a small Markdown subset, with [[slug]]
 * citations replaced by clickable chips.
 *
 * The safety guarantee is that the whole answer is escaped FIRST. The answer
 * is derived from note bodies — user content, and third-party page text once
 * it was ingested — so after that step the only `<` in the output is one this
 * function wrote, from a fixed tag set: p, strong, em, code, pre, ul, ol, li,
 * br, hr, and the chip's button. No tag carries an attribute except the chip
 * (escaped data-*) and `<ol start>`, whose value is an integer this function
 * parsed. Links, images, tables and raw HTML are not rendered; they stay
 * visible as the literal text the model wrote. Extending the subset means
 * adding to that tag list, never passing anything of the model's through.
 *
 * The backend already stripped citations it could not resolve, so anything
 * left that looks like a citation but isn't in `cited` is shown as its bare
 * slug text rather than becoming a chip that goes nowhere.
 *
 * No regex here may use lookbehind: it is a SyntaxError on WebKit before
 * Safari 16.4 (older macOS), and this module ships in the main bundle.
 */
export function renderAnswer(markdown: string, cited: CitedNote[]): string {
  const bySlug = new Map(cited.map((c) => [c.slug, c]));
  // NUL delimits the placeholders below; strip it so the answer cannot forge one.
  const escaped = escapeHtml(markdown.replace(/\0/g, ''));
  const held: string[] = [];
  const hold = (html: string) => `\0${held.push(html) - 1}\0`;

  const body = renderBlocks(escaped, (text) => renderInline(text, bySlug, hold));
  return body.replace(/\0(\d+)\0/g, (_, i: string) => held[Number(i)]);
}

/**
 * Inline spans of one line of already-escaped text. Code spans and citations
 * are swapped for placeholders before emphasis runs, so a `*` or `_` inside
 * code, a slug, or a chip's title or account id is never read as emphasis —
 * and emphasis can never split a chip's markup.
 */
function renderInline(
  text: string,
  bySlug: Map<string, CitedNote>,
  hold: (html: string) => string,
): string {
  return (
    text
      // Backslash escapes and code spans in ONE left-to-right pass: a code
      // span keeps its backslashes (`\\server`), and an escaped backtick never
      // opens one. Escapes become numeric entities, which no rule below matches.
      .replace(/\\([\\`*_[\]#+\-.!])|(`+)(.+?)\2(?!`)/gs, (_, esc?: string, _ticks?: string, code?: string) =>
        esc !== undefined ? `&#${esc.charCodeAt(0)};` : hold(`<code>${code!.trim()}</code>`),
      )
      // `\0` excluded: a placeholder inside [[...]] is a code span, and holding
      // it as slug text would leak the raw placeholder (restore is one pass).
      .replace(/\[\[([^[\]\n\0]+)\]\]/g, (_, slug: string) => {
        const note = bySlug.get(slug.trim());
        if (!note) return hold(slug);
        return hold(
          `<button class="cite-chip" data-uuid="${escapeHtml(note.uuid)}" ` +
            `data-account="${escapeHtml(note.account_id)}">${escapeHtml(note.title)}</button>`,
        );
      })
      // Strong-em, then strong, then em. Em content excludes `<`: at this point
      // the only `<` in the string is a tag a strong rule wrote, so this keeps
      // the two from interleaving into misnested markup.
      // Content must start and end with a non-space; spelled as `\S(...\S)?`
      // rather than lookbehind (see above).
      .replace(/\*\*\*([^\s*](?:[^*]*?[^\s*])?)\*\*\*/g, '<strong><em>$1</em></strong>')
      .replace(/\*\*(\S(?:(?:(?!\*\*).)*?\S)?)\*\*/gs, '<strong>$1</strong>')
      .replace(/(^|[^\p{L}\p{M}\p{N}_])__(\S(?:(?:(?!__).)*?\S)?)__(?![\p{L}\p{M}\p{N}_])/gsu, '$1<strong>$2</strong>')
      .replace(/\*([^\s*<>](?:[^*<>]*?[^\s*<>])?)\*/g, '<em>$1</em>')
      // `_` is emphasis only at word boundaries, so push_one_dirty stays text.
      // The boundary before it is captured and put back ($1), not looked behind.
      .replace(/(^|[^\p{L}\p{M}\p{N}_])_([^\s_<>](?:[^_<>]*?[^\s_<>])?)_(?![\p{L}\p{M}\p{N}_])/gu, '$1<em>$2</em>')
  );
}

type List = { tag: 'ul' | 'ol'; indent: number };

// `s` (dotAll) on purpose: lines are already split on \n, and without it a
// U+2028 in the line makes `.*$` fail — after backtracking that took seconds.
const LIST_ITEM = /^( *)([-*+]|(\d{1,9})[.)])[ \t]+(.*)$/s;
const HEADING = /^ {0,3}#{1,6}[ \t]+(.*)$/s;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
// A backtick fence's info string cannot contain a backtick, so ```npm ci```
// on one line is inline code, not a fence that swallows the rest.
const FENCE = /^( *)(`{3,}(?=[^`]*$)|~{3,})/;

/** Heading text without its optional closing `#` run. */
function headingText(text: string): string {
  return text.trimEnd().replace(/(^|[ \t])#+$/, '').trimEnd();
}

/**
 * Block structure of already-escaped text: paragraphs (single newlines become
 * <br>), headings (rendered as a bold paragraph — a chat answer does not need
 * a type scale), rules, fenced code, and ordered/unordered lists, nested by
 * indentation and kept as one list across blank lines between items.
 */
function renderBlocks(src: string, inline: (text: string) => string): string {
  const out: string[] = [];
  const lists: List[] = [];
  let para: string[] = [];
  let fence: { marker: string; indent: number; lines: string[] } | null = null;
  let blankBefore = false;

  const flushPara = () => {
    if (para.length) out.push(`<p>${para.map(inline).join('<br>')}</p>`);
    para = [];
  };
  const popList = () => out.push(`</li></${lists.pop()!.tag}>`);
  const closeLists = () => {
    while (lists.length) popList();
  };
  const openList = (tag: 'ul' | 'ol', indent: number, start: number) => {
    out.push(tag === 'ol' && start !== 1 ? `<ol start="${start}"><li>` : `<${tag}><li>`);
    lists.push({ tag, indent });
  };
  // A line indented at least two columns past the innermost item's marker
  // belongs inside that item; models use 2, 3 or 4.
  const insideItem = (indent: number) => lists.length > 0 && indent >= lists.at(-1)!.indent + 2;

  for (const raw of src.replace(/\r\n?/g, '\n').split('\n')) {
    const line = raw.replace(/\t/g, '    ');

    if (fence) {
      if (line.trim().startsWith(fence.marker) && /^[`~]+$/.test(line.trim())) {
        out.push(`<pre><code>${fence.lines.join('\n')}</code></pre>`);
        fence = null;
      } else {
        // Raw, not tab-expanded, minus the fence's own indent (a fence inside
        // a list item is indented by the item, not by its code).
        fence.lines.push(raw.replace(/^ +/, (sp) => sp.slice(Math.min(sp.length, fence!.indent))));
      }
      continue;
    }

    if (!line.trim()) {
      flushPara();
      blankBefore = true;
      continue;
    }

    const indent = line.length - line.trimStart().length;
    const fenceOpen = FENCE.exec(line);
    const item = RULE.test(line) ? null : LIST_ITEM.exec(line);

    if (item) {
      flushPara();
      const tag = item[3] === undefined ? 'ul' : 'ol';
      const start = item[3] === undefined ? 1 : parseInt(item[3], 10);
      while (lists.length && indent + 1 < lists.at(-1)!.indent) popList();
      if (insideItem(indent)) {
        openList(tag, indent, start);
      } else if (lists.length && lists.at(-1)!.tag === tag) {
        out.push('</li><li>');
      } else {
        if (lists.length) popList();
        openList(tag, indent, start);
      }
      out.push(inline(item[4]));
    } else if (lists.length && (!blankBefore || insideItem(indent)) && !fenceOpen && !RULE.test(line) && !HEADING.test(line)) {
      // Continuation of the open item: a lazy line, or an indented one after a blank.
      out.push(`<br>${inline(line.trim())}`);
    } else {
      if (!(fenceOpen && insideItem(indent))) closeLists();
      const heading = HEADING.exec(line);
      if (!fenceOpen && !heading && !RULE.test(line)) {
        para.push(line.trim());
      } else {
        flushPara();
        if (fenceOpen) fence = { marker: fenceOpen[2], indent: fenceOpen[1].length, lines: [] };
        else if (heading) out.push(`<p><strong>${inline(headingText(heading[1]))}</strong></p>`);
        else out.push('<hr>');
      }
    }
    blankBefore = false;
  }

  if (fence) out.push(`<pre><code>${fence.lines.join('\n')}</code></pre>`);
  flushPara();
  closeLists();
  return out.join('');
}
