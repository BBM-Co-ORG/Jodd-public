// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { tryExitSpecialBlock } from './exitSpecialBlock';

function setCollapsedCaret(node: Node, offset: number) {
  const range = document.createRange();
  range.setStart(node, offset);
  range.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

describe('tryExitSpecialBlock', () => {
  it('does NOT exit when the cursor is at the start of a non-empty heading', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Heading text here');
    h2.appendChild(text);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryExitSpecialBlock(editorEl);

    expect(result).toBe(false);
    // The heading must be untouched — no sibling inserted, text intact.
    expect(editorEl.children.length).toBe(1);
    expect(h2.textContent).toBe('Heading text here');
  });

  it('does NOT exit when the cursor is in the middle of a non-empty heading', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Heading text here');
    h2.appendChild(text);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 7); // between "Heading" and " text here"

    const result = tryExitSpecialBlock(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(1);
  });

  it('exits when the cursor is on a genuinely empty line inside a heading', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    h2.appendChild(document.createElement('br'));
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(h2, 0);

    const result = tryExitSpecialBlock(editorEl);

    expect(result).toBe(true);
    // A new plain block should now exist after the (now-empty) heading.
    expect(editorEl.children.length).toBe(2);
    expect(editorEl.children[1].tagName).toBe('DIV');
  });

  it('exits when the cursor is at the end of an empty line inside a blockquote', () => {
    const editorEl = document.createElement('div');
    const bq = document.createElement('blockquote');
    bq.appendChild(document.createElement('br'));
    editorEl.appendChild(bq);
    document.body.appendChild(editorEl);

    setCollapsedCaret(bq, 1);

    const result = tryExitSpecialBlock(editorEl);

    expect(result).toBe(true);
    expect(editorEl.children.length).toBe(2);
  });
});
