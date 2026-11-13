// What a paste keeps. The clipboard's HTML is reduced to a whitelist of
// structural tags; everything else is unwrapped (children kept, tag dropped).
import { rendersNothing } from './noteLines';

// <font> and <span> are dropped because they almost always carry
// presentational styling we don't want.
const PASTE_TAG_WHITELIST = new Set([
  'A', 'B', 'STRONG', 'I', 'EM', 'U', 'S', 'STRIKE', 'CODE', 'PRE',
  'BR', 'P', 'DIV', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6',
  'UL', 'OL', 'LI', 'BLOCKQUOTE',
]);

export function sanitizePastedHtml(html: string): string {
  const doc = new DOMParser().parseFromString(html, 'text/html');
  // Process body's children — never the body itself, or it would be unwrapped
  // out of the document and doc.body would be null on read-back.
  const walk = (node: Node) => {
    const kids = Array.from(node.childNodes);
    for (const child of kids) walk(child);
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const el = node as Element;
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
      if (el.tagName === 'A' && attr.name === 'href') continue;
      el.removeAttribute(attr.name);
    }
  };
  for (const child of Array.from(doc.body.childNodes)) walk(child);
  return doc.body.innerHTML;
}
