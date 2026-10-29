// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { tryExitSpecialBlock, planExitSpecialBlock } from './exitSpecialBlock';

function setCollapsedCaret(node: Node, offset: number) {
  const range = document.createRange();
  range.setStart(node, offset);
  range.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

// jsdom has no execCommand, and what the commands do to the DOM is WebKit's
// behaviour, measured there (see exitSpecialBlock.ts). These tests pin the
// decision and the commands issued.
let exec: ReturnType<typeof vi.fn>;
beforeEach(() => {
  document.body.innerHTML = '';
  exec = vi.fn(() => true);
  Object.defineProperty(document, 'execCommand', { value: exec, configurable: true, writable: true });
});

function editor(html: string): HTMLElement {
  const editorEl = document.createElement('div');
  editorEl.innerHTML = html;
  document.body.appendChild(editorEl);
  return editorEl;
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
    expect(exec).not.toHaveBeenCalled();
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
    expect(exec).not.toHaveBeenCalled();
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
    // The empty heading line itself becomes a plain block.
    expect(exec.mock.calls).toEqual([['formatBlock', false, '<div>']]);
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
    expect(exec.mock.calls).toEqual([['formatBlock', false, '<div>']]);
  });

  it('drops the empty last line of a multi-line quote, then opens a plain block after it', () => {
    const editorEl = editor('<blockquote>quoted<br><br></blockquote>');
    const bq = editorEl.firstElementChild!;
    setCollapsedCaret(bq, 2); // between the two <br>s: the empty second line

    expect(tryExitSpecialBlock(editorEl)).toBe(true);
    expect(exec.mock.calls).toEqual([
      ['delete'],
      ['insertParagraph'],
      ['formatBlock', false, '<div>'],
    ]);
  });

  it('outdents an empty last <div> line of a quote made of <div> lines — formatBlock would only re-tag the inner div', () => {
    const editorEl = editor('<blockquote><div>a</div><div><br></div></blockquote>');
    const line = editorEl.querySelector('blockquote > div:last-child')!;
    setCollapsedCaret(line, 0);

    expect(planExitSpecialBlock(editorEl)).toBe('outdent');
    expect(tryExitSpecialBlock(editorEl)).toBe(true);
    expect(exec.mock.calls).toEqual([['outdent']]);
  });

  it('leaves an empty line in the MIDDLE of a quote to native Enter', () => {
    const editorEl = editor('<blockquote>a<br><br>b</blockquote>');
    setCollapsedCaret(editorEl.firstElementChild!, 2);

    expect(tryExitSpecialBlock(editorEl)).toBe(false);
    expect(exec).not.toHaveBeenCalled();
  });

  it('does not treat an image as an empty line', () => {
    const editorEl = editor('<blockquote>a<br><br><img src="x.png"></blockquote>');
    setCollapsedCaret(editorEl.firstElementChild!, 2);

    expect(tryExitSpecialBlock(editorEl)).toBe(false);
  });
});
