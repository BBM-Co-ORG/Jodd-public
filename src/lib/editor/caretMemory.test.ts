// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { markCaret, restoreCaret } from './caretMemory';

function body(html: string): HTMLElement {
  const el = document.createElement('div');
  el.contentEditable = 'true';
  el.tabIndex = 0;
  el.innerHTML = html;
  document.body.appendChild(el);
  return el;
}
function caretAt(node: Node, offset: number) {
  const r = document.createRange();
  r.setStart(node, offset);
  r.collapse(true);
  const s = window.getSelection()!;
  s.removeAllRanges();
  s.addRange(r);
}

describe('caretMemory', () => {
  it('marks the caret as a text offset across lines, and restores it there', () => {
    const el = body('<div>one</div><div>two</div><div>three</div>');
    caretAt(el.children[2].firstChild!, 2); // "th|ree"
    const mark = markCaret(el)!;
    expect(mark.start).toEqual({ line: 2, col: 2 });

    caretAt(el.children[0].firstChild!, 0); // the selection is taken elsewhere
    restoreCaret(el, mark);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.startContainer).toBe(el.children[2].firstChild);
    expect(r.startOffset).toBe(2);
    expect(document.activeElement).toBe(el);
    el.remove();
  });

  it('survives a re-render: offsets, not nodes', () => {
    const el = body('<div>alpha</div><div>beta</div>');
    caretAt(el.children[1].firstChild!, 3);
    const mark = markCaret(el)!;
    el.innerHTML = '<div>alpha</div><div>beta</div>'; // fresh nodes
    restoreCaret(el, mark);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.startContainer).toBe(el.children[1].firstChild);
    expect(r.startOffset).toBe(3);
    el.remove();
  });

  it('clamps to the end when the text is now shorter (the other version is shorter)', () => {
    const el = body('<div>a long line of mine</div>');
    caretAt(el.firstElementChild!.firstChild!, 15);
    const mark = markCaret(el)!;
    el.innerHTML = '<div>short</div>';
    restoreCaret(el, mark);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.startOffset).toBe(5);
    el.remove();
  });

  // Re-critique after #296: after Discard the body holds the other device's
  // text; a character count from the start landed on an unrelated line, off
  // screen. The same line keeps the user near where they were.
  it('maps to the same line in a different text, not the same character count', () => {
    const el = body('<div>a very long first line here</div><div>b</div><div>third</div>');
    caretAt(el.children[2].firstChild!, 3); // "thi|rd" on line 3
    const mark = markCaret(el)!;
    el.innerHTML = '<div>x</div><div>y</div><div>other third line</div><div>four</div>';
    restoreCaret(el, mark);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.startContainer).toBe(el.children[2].firstChild);
    expect(r.startOffset).toBe(3);
    el.remove();
  });

  it('clamps to the last line when the text now has fewer lines', () => {
    const el = body('<div>1</div><div>2</div><div>three</div>');
    caretAt(el.children[2].firstChild!, 2);
    const mark = markCaret(el)!;
    el.innerHTML = '<div>only</div>';
    restoreCaret(el, mark);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.startContainer).toBe(el.firstElementChild!.firstChild);
    expect(r.startOffset).toBe(2);
    el.remove();
  });

  it('a selection outside the body marks nothing', () => {
    const el = body('<div>x</div>');
    const other = body('<div>y</div>');
    caretAt(other.firstElementChild!.firstChild!, 0);
    expect(markCaret(el)).toBeNull();
    el.remove();
    other.remove();
  });
});
