// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { tryBackspaceMergeEmptyPrevious, planBackspaceMergeEmpty } from './backspaceMergeEmpty';

function setCollapsedCaret(node: Node, offset: number) {
  const range = document.createRange();
  range.setStart(node, offset);
  range.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

// jsdom has no execCommand, and what the commands do to the DOM is WebKit's
// behaviour, measured there (see backspaceMergeEmpty.ts). These tests pin the
// decision, the commands issued and the selection each one acts on.
type Call = { args: unknown[]; selection: string };
let calls: Call[];
function describeSelection(): string {
  const r = window.getSelection()!.getRangeAt(0);
  const at = (n: Node, o: number) => `${n.nodeType === 3 ? JSON.stringify((n as Text).data) : n.nodeName}@${o}`;
  return r.collapsed ? at(r.startContainer, r.startOffset) : `${at(r.startContainer, r.startOffset)}..${at(r.endContainer, r.endOffset)}`;
}
beforeEach(() => {
  document.body.innerHTML = '';
  calls = [];
  const exec = vi.fn((...args: unknown[]) => {
    calls.push({ args, selection: describeSelection() });
    return true;
  });
  Object.defineProperty(document, 'execCommand', { value: exec, configurable: true, writable: true });
});

function editor(html: string): HTMLElement {
  const editorEl = document.createElement('div');
  editorEl.innerHTML = html;
  document.body.appendChild(editorEl);
  return editorEl;
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
    // The empty line is the first block: it takes the heading's tag, so the
    // merge WebKit performs on delete keeps an <h2>.
    expect(calls).toEqual([
      { args: ['formatBlock', false, '<h2>'], selection: 'H2@0' },
      { args: ['delete'], selection: 'H2@0' },
    ]);
    expect(window.getSelection()!.getRangeAt(0).startContainer).toBe(h2);
  });

  it('with a block above the empty line, deletes from the end of that block through the empty line', () => {
    const editorEl = editor('<div>above</div><div><br></div><h2>Heading</h2>');
    const [above, empty] = Array.from(editorEl.children);
    const h2 = editorEl.querySelector('h2')!;
    setCollapsedCaret(h2.firstChild!, 0);

    let deleted: Range | null = null;
    Object.defineProperty(document, 'execCommand', {
      value: vi.fn(() => { deleted = window.getSelection()!.getRangeAt(0).cloneRange(); return true; }),
      configurable: true,
      writable: true,
    });

    expect(tryBackspaceMergeEmptyPrevious(editorEl)).toBe(true);
    // End of the block above → end of the empty line; the <h2> is outside it.
    expect([deleted!.startContainer, deleted!.startOffset]).toEqual([above, above.childNodes.length]);
    expect([deleted!.endContainer, deleted!.endOffset]).toEqual([empty, empty.childNodes.length]);
    // Caret goes back to where Backspace was pressed.
    const sel = window.getSelection()!.getRangeAt(0);
    expect(sel.startContainer).toBe(h2);
    expect(sel.startOffset).toBe(0);
  });

  it('skips whitespace text between blocks when looking for the previous line', () => {
    const editorEl = editor('<div>above</div>\n<div>&nbsp;</div>\n<h2>Heading</h2>');
    setCollapsedCaret(editorEl.querySelector('h2')!.firstChild!, 0);

    const plan = planBackspaceMergeEmpty(editorEl);
    expect(plan?.kind).toBe('deleteFromAbove');
    expect(plan && 'above' in plan && (plan.above as Element).textContent).toBe('above');
  });

  it('does not treat a line holding only an image or a checkbox as empty', () => {
    for (const inner of ['<img src="x.png">', '<input type="checkbox" contenteditable="false">&nbsp;']) {
      const editorEl = editor(`<div>above</div><div>${inner}</div><h2>Heading</h2>`);
      setCollapsedCaret(editorEl.querySelector('h2')!.firstChild!, 0);
      expect(tryBackspaceMergeEmptyPrevious(editorEl)).toBe(false);
    }
    expect(calls).toEqual([]);
  });

  it('never deletes from an image, rule or table above — a heading takes the re-tag path instead', () => {
    for (const above of ['<img src="x.png">', '<hr>', '<table><tbody><tr><td>t</td></tr></tbody></table>']) {
      const editorEl = editor(`${above}<div><br></div><h2>Heading</h2>`);
      setCollapsedCaret(editorEl.querySelector('h2')!.firstChild!, 0);
      expect(planBackspaceMergeEmpty(editorEl)?.kind).toBe('retagEmpty');
    }
  });

  it('leaves a plain block under an empty line below a rule to native Backspace', () => {
    const editorEl = editor('<hr><div><br></div><div>plain</div>');
    setCollapsedCaret(editorEl.lastElementChild!.firstChild!, 0);
    expect(planBackspaceMergeEmpty(editorEl)).toBeNull();
  });

  it('still deletes from a block that merely ENDS in an image', () => {
    const editorEl = editor('<div>caption<img src="x.png"></div><div><br></div><h2>Heading</h2>');
    setCollapsedCaret(editorEl.querySelector('h2')!.firstChild!, 0);
    expect(planBackspaceMergeEmpty(editorEl)?.kind).toBe('deleteFromAbove');
  });

  it('leaves a non-heading block under an empty FIRST line to native Backspace, which keeps it', () => {
    const editorEl = editor('<div><br></div><ul><li>item</li></ul>');
    setCollapsedCaret(editorEl.querySelector('li')!.firstChild!, 0);

    expect(tryBackspaceMergeEmptyPrevious(editorEl)).toBe(false);
    expect(calls).toEqual([]);
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
