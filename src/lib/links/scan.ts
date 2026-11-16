// The editor's half of Jodd's link detection: a TypeScript port of the
// backend's `link_text` + `scan_links` (src-tauri/src/db.rs), run over the
// editor's DOM instead of an HTML string. Both are held to one fixture,
// tests/fixtures/link-detection.json — change a rule in one and the other
// fails until it follows.
//
// Offsets here are UTF-16 code-unit indices into `LinkText.text`, because that
// is what DOM Ranges count. Rust's are char indices; the fixture compares texts
// only, so the two are never compared with each other.

/** Elements that contribute nothing of their own to the link text. */
export const INLINE_TAGS: ReadonlySet<string> = new Set([
  'a', 'b', 'i', 'u', 's', 'strike', 'em', 'strong', 'code', 'span', 'font', 'sub', 'sup', 'mark',
]);

/** Elements dropped with their whole subtree — no text, and no `'\n'` either. */
export const DROPPED_TAGS: ReadonlySet<string> = new Set(['head', 'style', 'script', 'title']);

/** Void elements: exactly one `'\n'`, no children to bracket. */
const VOID_TAGS: ReadonlySet<string> = new Set(['br', 'img', 'input', 'hr', 'wbr']);

export interface LinkText {
  /** The node that was scanned; an `<a>` at or above it is not "inside an anchor". */
  root: Node;
  text: string;
  /** Text nodes that contributed to `text`, sorted by `start` (the offset in `text` where each begins). */
  nodes: Array<{ node: Text; start: number }>;
}

/**
 * The DOM's link text: mirrors `link_text` in db.rs. Text nodes contribute
 * their data as-is (the DOM already decoded entities), inline elements nothing,
 * and every other element one `'\n'` before its first child and one after its
 * last, so a block boundary stops a URL or wikilink the way it does in Rust.
 */
export function buildLinkText(root: Node): LinkText {
  const parts: string[] = [];
  const nodes: LinkText['nodes'] = [];
  let length = 0;

  const emit = (s: string) => {
    parts.push(s);
    length += s.length;
  };

  const visit = (n: Node) => {
    if (n.nodeType === Node.TEXT_NODE) {
      const t = n as Text;
      if (t.data.length > 0) {
        nodes.push({ node: t, start: length });
        emit(t.data);
      }
      return;
    }
    if (n.nodeType !== Node.ELEMENT_NODE) {
      // Comments, processing instructions: nothing. Documents and fragments: recurse.
      if (n.nodeType === Node.DOCUMENT_NODE || n.nodeType === Node.DOCUMENT_FRAGMENT_NODE) {
        for (let c = n.firstChild; c; c = c.nextSibling) visit(c);
      }
      return;
    }
    const name = (n as Element).localName.toLowerCase();
    if (DROPPED_TAGS.has(name)) return;
    if (VOID_TAGS.has(name)) {
      emit('\n');
      return;
    }
    const inline = INLINE_TAGS.has(name);
    if (!inline) emit('\n');
    for (let c = n.firstChild; c; c = c.nextSibling) visit(c);
    if (!inline) emit('\n');
  };
  visit(root);

  return { root, text: parts.join(''), nodes };
}

export interface LinkSpan {
  kind: 'url' | 'wikilink';
  /** The URL as written, or the wikilink's trimmed inner text. */
  text: string;
  /** Offsets in `LinkText.text`, covering the whole token including `[[` / `]]`. */
  start: number;
  end: number;
  /** The span's first character sits in a text node that has an `<a>` ancestor. */
  insideAnchor: boolean;
}

/**
 * Link whitespace: exactly Unicode White_Space, spelled out (never `\s` or
 * `trim()`, which disagree on U+0085 and U+FEFF). Same list as `is_link_space`.
 */
export function isLinkSpace(ch: string | number): boolean {
  const c = typeof ch === 'number' ? ch : ch.charCodeAt(0);
  return (
    (c >= 0x0009 && c <= 0x000d) ||
    c === 0x0020 ||
    c === 0x0085 ||
    c === 0x00a0 ||
    c === 0x1680 ||
    (c >= 0x2000 && c <= 0x200a) ||
    c === 0x2028 ||
    c === 0x2029 ||
    c === 0x202f ||
    c === 0x205f ||
    c === 0x3000
  );
}

const isAsciiAlnum = (c: number) =>
  (c >= 0x30 && c <= 0x39) || (c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a);

/** `text[at..]` starts with the ASCII literal `lit` (lowercase), ignoring ASCII case only. */
function startsWithCi(text: string, at: number, lit: string): boolean {
  if (at + lit.length > text.length) return false;
  for (let k = 0; k < lit.length; k++) {
    let c = text.charCodeAt(at + k);
    if (c >= 0x41 && c <= 0x5a) c += 0x20;
    if (c !== lit.charCodeAt(k)) return false;
  }
  return true;
}

const LBRACKET = 0x5b;
const RBRACKET = 0x5d;
const NEWLINE = 0x0a;

/** Index of the node whose `[start, start + length)` holds `pos`, or -1. */
function nodeIndexAt(lt: LinkText, pos: number): number {
  return findNode(lt, (n) => (pos < n.start ? -1 : pos >= n.start + n.node.data.length ? 1 : 0));
}

/** Index of the node whose `(start, start + length]` holds `pos` (an end position), or -1. */
function nodeIndexEndingAt(lt: LinkText, pos: number): number {
  return findNode(lt, (n) => (pos <= n.start ? -1 : pos > n.start + n.node.data.length ? 1 : 0));
}

function findNode(lt: LinkText, cmp: (n: LinkText['nodes'][number]) => number): number {
  let lo = 0;
  let hi = lt.nodes.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const c = cmp(lt.nodes[mid]);
    if (c === 0) return mid;
    if (c < 0) hi = mid - 1;
    else lo = mid + 1;
  }
  return -1;
}

const XHTML_NS = 'http://www.w3.org/1999/xhtml';

/**
 * The text node at `pos` has an HTML `<a>` ancestor strictly inside the scanned
 * root. Walked by hand, not `closest('a')`: that climbs past the root (an
 * editor that itself sits inside an `<a>` would mark every span) and also
 * matches an SVG `<a>`.
 */
function insideAnchorAt(lt: LinkText, pos: number): boolean {
  const i = nodeIndexAt(lt, pos);
  if (i < 0) return false;
  for (let e = lt.nodes[i].node.parentElement; e && e !== lt.root; e = e.parentElement) {
    if (e.localName === 'a' && e.namespaceURI === XHTML_NS) return true;
  }
  return false;
}

/**
 * The ONE link scanner, ported from `scan_links` in db.rs — read its doc
 * comment for the rules. Every occurrence, in order, never overlapping.
 */
export function scanLinks(lt: LinkText): LinkSpan[] {
  const text = lt.text;
  const n = text.length;
  const out: LinkSpan[] = [];
  let i = 0;
  while (i < n) {
    const c0 = text.charCodeAt(i);
    if (c0 === LBRACKET && i + 1 < n && text.charCodeAt(i + 1) === LBRACKET) {
      const start = i + 2;
      let j = start;
      let close = -1;
      while (j < n) {
        const c = text.charCodeAt(j);
        if (c === NEWLINE || (c === LBRACKET && j + 1 < n && text.charCodeAt(j + 1) === LBRACKET)) break;
        if (c === RBRACKET && j + 1 < n && text.charCodeAt(j + 1) === RBRACKET) {
          close = j;
          break;
        }
        j++;
      }
      if (close > start) {
        let a = start;
        let b = close;
        while (a < b && isLinkSpace(text.charCodeAt(a))) a++;
        while (b > a && isLinkSpace(text.charCodeAt(b - 1))) b--;
        if (a < b) {
          out.push({
            kind: 'wikilink',
            text: text.slice(a, b),
            start: i,
            end: close + 2,
            insideAnchor: insideAnchorAt(lt, i),
          });
        }
        i = close + 2;
      } else {
        i = start; // not a wikilink: resume right after the `[[`
      }
      continue;
    }

    const schemeLen = startsWithCi(text, i, 'https://') ? 8 : startsWithCi(text, i, 'http://') ? 7 : 0;
    if (schemeLen > 0) {
      if (i > 0 && isAsciiAlnum(text.charCodeAt(i - 1))) {
        i++;
        continue;
      }
      let end = i + schemeLen;
      let depth = 0;
      while (end < n) {
        const c = text.charCodeAt(end);
        if (isLinkSpace(c) || c === 0x3c /* < */ || c === 0x3e /* > */ || c === 0x22 /* " */ || c === LBRACKET || c === RBRACKET) break;
        if (c === 0x28 /* ( */) depth++;
        else if (c === 0x29 /* ) */) {
          if (depth === 0) break;
          depth--;
        }
        end++;
      }
      while (end > i && '.,;:!?\''.includes(text[end - 1])) end--;
      if (end > i + schemeLen) {
        out.push({
          kind: 'url',
          text: text.slice(i, end),
          start: i,
          end,
          insideAnchor: insideAnchorAt(lt, i),
        });
      }
      i = end;
      continue;
    }
    i++;
  }
  return out;
}

/**
 * The DOM Range covering `[start, end)` of the link text, or null when the
 * interval is empty or does not land inside text nodes. The start prefers the
 * later node at a boundary and the end the earlier one, so the Range hugs the
 * text rather than the tag boundaries around it.
 */
export function spanToRange(lt: LinkText, start: number, end: number): Range | null {
  if (!(start < end)) return null;
  const a = nodeIndexAt(lt, start);
  const b = nodeIndexEndingAt(lt, end);
  if (a < 0 || b < 0 || b < a) return null;
  const first = lt.nodes[a];
  const last = lt.nodes[b];
  const range = first.node.ownerDocument.createRange();
  range.setStart(first.node, start - first.start);
  range.setEnd(last.node, end - last.start);
  return range;
}
