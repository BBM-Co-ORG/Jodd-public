// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { tryBackspaceMergeEmptyPrevious } from './backspaceMergeEmpty';

function setCollapsedCaret(node: Node, offset: number) {
  const range = document.createRange();
  range.setStart(node, offset);
  range.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

describe('tryBackspaceMergeEmptyPrevious', () => {
  it('removes an empty previous sibling and keeps the current block untouched (tag + content)', () => {
    const editorEl = document.createElement('div');
    const blank = document.createElement('h2');
    blank.appendChild(document.createElement('br'));
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Heading text here');
    h2.appendChild(text);
    editorEl.appendChild(blank);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryBackspaceMergeEmptyPrevious(editorEl);

    expect(result).toBe(true);
    expect(editorEl.children.length).toBe(1);
    expect(editorEl.children[0]).toBe(h2);
    expect(editorEl.children[0].tagName).toBe('H2');
    expect(editorEl.children[0].textContent).toBe('Heading text here');
    // Caret stays at the start of the (now-only) block.
    const sel = window.getSelection()!;
    expect(sel.getRangeAt(0).startContainer).toBe(h2);
    expect(sel.getRangeAt(0).startOffset).toBe(0);
  });

  it('does nothing when the caret is NOT at the block start', () => {
    const editorEl = document.createElement('div');
    const blank = document.createElement('div');
    blank.appendChild(document.createElement('br'));
    const div = document.createElement('div');
    const text = document.createTextNode('some text');
    div.appendChild(text);
    editorEl.appendChild(blank);
    editorEl.appendChild(div);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 3); // mid-word

    const result = tryBackspaceMergeEmptyPrevious(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(2);
  });

  it('does nothing when the previous sibling has real content', () => {
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

    const result = tryBackspaceMergeEmptyPrevious(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(2);
    expect(first.textContent).toBe('First paragraph');
  });

  it('does nothing when there is no previous sibling (first block in editor)', () => {
    const editorEl = document.createElement('div');
    const h2 = document.createElement('h2');
    const text = document.createTextNode('Only block');
    h2.appendChild(text);
    editorEl.appendChild(h2);
    document.body.appendChild(editorEl);

    setCollapsedCaret(text, 0);

    const result = tryBackspaceMergeEmptyPrevious(editorEl);

    expect(result).toBe(false);
    expect(editorEl.children.length).toBe(1);
  });
});
