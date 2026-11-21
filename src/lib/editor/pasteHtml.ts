// What a paste keeps. The clipboard's HTML is reduced to a whitelist of
// structural tags; everything else is unwrapped (children kept, tag dropped).
import { rendersNothing } from './noteLines';

// <font> and <span> are dropped because they almost always carry
// presentational styling we don't want.
const PASTE_TAG_WHITELIST = new Set([
  'A', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'CODE', 'PRE',
  'BR', 'DIV', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
  'UL', 'OL', 'LI', 'BLOCKQUOTE',
]);

// A pasted <p> becomes a <div>: one line, as every line of an Apple note is.
// A copy out of Notes.app (or any Cocoa app) is RTF on the pasteboard, and
// WebKit hands the page each RTF paragraph as a <p style="margin: 0…">
// (measured 2026-11-17). Stripping the style left a bare <p>, which every
// reader shows with a one-line gap above and below — and pressing Enter in
// it makes more of them. A live note's links came out spaced apart in Jodd,
// Gmail and Notes.app from one paste of a tag line.
function asLine(p: Element): Element {
  const div = p.ownerDocument.createElement('div');
  while (p.firstChild) div.appendChild(p.firstChild);
  p.replaceWith(div);
  return div;
}

export function sanitizePastedHtml(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  // Process body's children — never the body itself, or it would be unwrapped
  // out of the document and doc.body would be null on read-back.
  const walk = (node: Node) => {
    const kids = Array.from(node.childNodes);
    for (const child of kids) walk(child);
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    let el = node as Element;
    if (el.tagName === 'P') el = asLine(el);
    if (el.tagName === 'SCRIPT' || el.tagName === 'STYLE' || el.tagName === 'META') {
      el.remove();
      return;
    }
    if (!PASTE_TAG_WHITELIST.has(el.tagName)) {
      const parent = el.parentNode;
      if (!parent) return;
      while (el.firstChild) parent.insertBefore(el.firstChild, el);
      parent.removeChild(el);
      return;
    }
    // Unwrapping an <img> or an empty <span> leaves its <div> with nothing in
    // it: a zero-height box the editor cannot show and the note keeps (the
    // empty <div></div> that jammed line 140 of a live note, 2026-11-12). A
    // blank LINE, <div><br></div>, is not this — it has a <br>.
    if (rendersNothing(el)) {
      el.remove();
      return;
    }
    for (const attr of Array.from(el.attributes)) {
      if (el.tagName === 'A' && attr.name === 'href' && isSafePastedHref(attr.value)) continue;
      el.removeAttribute(attr.name);
    }
  };
  for (const child of Array.from(doc.body.childNodes)) walk(child);
  return doc.body.innerHTML;
}

/** A pasted link loses its href when the scheme can run or embed content
 *  (`javascript:`, `data:`, `vbscript:`, `blob:`, `file:`); the link text
 *  stays. The note syncs to other devices and other apps, which do not all
 *  share this editor's click guard. A block list, not an allow list: notes
 *  legitimately carry app schemes (`applenotes:`, `jodd:`, `tel:` …). */
export function isSafePastedHref(href: string): boolean {
  // Browsers ignore whitespace and control characters inside a scheme.
  const compact = href.replace(/[\u0000-\u0020]/g, '');
  const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(compact);
  if (!scheme) return true;
  return !['javascript', 'data', 'vbscript', 'blob', 'file'].includes(scheme[1].toLowerCase());
}
