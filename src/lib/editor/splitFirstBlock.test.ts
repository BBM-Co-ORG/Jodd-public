// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { tryFixFirstBlockEnter } from './splitFirstBlock';

function setCollapsedCaret(node: Node, offset: number) {
  const range = document.createRange();
  range.setStart(node, offset);
  range.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

describe('tryFixFirstBlockEnter', () => {
  it('splits the very first block when the caret is at its absolute start', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Heading text here');
    h2.appendChild(text);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryFixFirstBlockEnter(editorEl);

    expect(result).toBe(true);
    expect(editorEl.children.length).toBe(2);
    // The now-empty first line keeps the original tag (matches how a normal
    // in-place split behaves elsewhere in the editor).
    expect(editorEl.children[0].tagName).toBe('H2');
    expect(editorEl.children[0].textContent).toBe('');
    // The moved content keeps the heading formatting too — the user's text
    // shouldn't silently lose its heading just because it moved down a line.
    expect(editorEl.children[1].tagName).toBe('H2');
    expect(editorEl.children[1].textContent).toBe('Heading text here');
    // Caret follows the moved text into the new block.
    const sel = window.getSelection()!;
    expect(sel.getRangeAt(0).startContainer).toBe(editorEl.children[1]);
  });

  it('splits a bare, unwrapped text node at the editor root (WebKit default for the first typed line)', () => {
    // WebKit inserts the very first line typed into an empty contenteditable
    // as a raw text node directly under the root — no <div>/<p> wrapper,
    // unlike Chromium. Confirmed by reproducing this exact structure in a
    // live browser during debugging.
    const editorEl = document.createElement('div');
    const text = document.createTextNode('Plain text no wrapper');
    editorEl.appendChild(text);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryFixFirstBlockEnter(editorEl);

    expect(result).toBe(true);
    expect(editorEl.children.length).toBe(2);
    // No original tag to preserve — the emptied placeholder is a plain div.
    expect(editorEl.children[0].tagName).toBe('DIV');
    expect(editorEl.children[0].textContent).toBe('');
    expect(editorEl.children[1].tagName).toBe('DIV');
    expect(editorEl.children[1].textContent).toBe('Plain text no wrapper');
    const sel = window.getSelection()!;
    expect(sel.getRangeAt(0).startContainer).toBe(editorEl.children[1]);
  });

  it('does nothing when the caret is NOT at the start of the first block', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Heading text here');
    h2.appendChild(text);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 7); // mid-word

    const result = tryFixFirstBlockEnter(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(1);
    expect(h2.textContent).toBe('Heading text here');
  });

  it('does nothing for a block that is NOT the first (native split already correct)', () => {
    const editorEl = document.createElement('div');
    const first = document.createElement('div');
    first.textContent = 'First paragraph';
    const second = document.createElement('div');
    const text = document.createTextNode('Second paragraph');
    second.appendChild(text);
    editorEl.appendChild(first);
    editorEl.appendChild(second);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryFixFirstBlockEnter(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(2);
    expect(second.textContent).toBe('Second paragraph');
  });

  it('does nothing when the first block is already empty', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    h2.appendChild(document.createElement('br'));
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(h2, 0);

    const result = tryFixFirstBlockEnter(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(1);
  });
});
