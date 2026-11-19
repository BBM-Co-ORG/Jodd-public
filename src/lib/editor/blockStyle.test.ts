// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { applyBlockStyle, editorLines, linesInRange, type Exec } from './blockStyle';

function editor(html: string): HTMLDivElement {
  const ed = document.createElement('div');
  ed.innerHTML = html;
  document.body.replaceChildren(ed);
  return ed;
}
function text(root: Node, s: string): Text {
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let t = w.nextNode(); t; t = w.nextNode()) if ((t as Text).data === s) return t as Text;
  throw new Error(`no text ${s}`);
}
function range(a: Node, ao: number, b: Node = a, bo: number = ao): Range {
  const r = document.createRange();
  r.setStart(a, ao);
  r.setEnd(b, bo);
  return r;
}
const lineTexts = (ed: Element) => editorLines(ed).map((l) => range(l.start.node, l.start.offset, l.end.node, l.end.offset).toString());

describe('editorLines', () => {
  it('counts a block as a line, and each part of a block a <br> splits off', () => {
    const ed = editor('<div>t</div><div>A<br>B<br>C</div><h2>D</h2>');
    expect(lineTexts(ed)).toEqual(['t', 'A', 'B', 'C', 'D']);
  });

  it('marks the <br> between two lines, never the placeholder after a block\'s last line', () => {
    const ed = editor('<div>A<br>B<br></div><div>C</div>');
    const [a, b, c] = editorLines(ed);
    expect(a.endBr).toBe(ed.querySelectorAll('br')[0]);
    expect(b.endBr).toBeNull();
    expect(c.endBr).toBeNull();
  });

  it('counts a blank line, alone in its block or between two <br>s', () => {
    const ed = editor('<div>A</div><div><br></div><div>B<br><br>C</div>');
    const lines = editorLines(ed);
    expect(lineTexts(ed)).toEqual(['A', '', 'B', '', 'C']);
    const [b1, b2] = ed.querySelectorAll('div')[2].querySelectorAll('br');
    expect(lines[2].endBr).toBe(b1);
    expect(lines[3].endBr).toBe(b2);
  });

  it('follows a <br> into an inline wrapper — the shape a line merge leaves', () => {
    const ed = editor('<div><span style="x">m3<br></span><span>SLZ</span></div>');
    const [m3, slz] = editorLines(ed);
    expect(m3.endBr).toBe(ed.querySelector('br'));
    expect(slz.endBr).toBeNull();
  });

  it('skips the source whitespace between blocks, keeps a no-break space', () => {
    const ed = editor('<div>A</div>\n  <div>&nbsp;</div>\n');
    expect(lineTexts(ed)).toEqual(['A', ' ']);
  });
});

describe('linesInRange', () => {
  const ed = () => editor('<div>A</div><div>B</div><div>C</div>');

  it('a caret touches its own line', () => {
    const e = ed();
    expect(linesInRange(editorLines(e), range(text(e, 'B'), 0))).toEqual([1, 1]);
  });

  it('a selection touches every line it crosses', () => {
    const e = ed();
    expect(linesInRange(editorLines(e), range(text(e, 'A'), 0, text(e, 'C'), 1))).toEqual([0, 2]);
  });

  it('a triple-click, which ends at the next line\'s start, does not reach that line', () => {
    const e = ed();
    expect(linesInRange(editorLines(e), range(text(e, 'A'), 0, text(e, 'B'), 0))).toEqual([0, 0]);
  });
});

describe('applyBlockStyle', () => {
  // jsdom has no execCommand: record what would run, and where.
  function recorder() {
    const calls: string[] = [];
    const exec: Exec = (cmd, value) => {
      const r = document.getSelection()!.getRangeAt(0);
      const at = r.collapsed
        ? `caret@${(r.startContainer as Text).data ?? r.startContainer.nodeName}`
        : `select:${r.cloneContents().firstChild?.nodeName}`;
      calls.push(`${cmd}${value ? ` ${value}` : ''} ${at}`);
    };
    return { calls, exec };
  }

  it('formats each selected line on its own, last first', () => {
    const ed = editor('<div>t</div><div>A</div><div>B</div><div>C</div><div>D</div>');
    const { calls, exec } = recorder();
    applyBlockStyle(ed, range(text(ed, 'A'), 0, text(ed, 'C'), 1), 'h2', exec);
    expect(calls).toEqual(['formatBlock <h2> caret@C', 'formatBlock <h2> caret@B', 'formatBlock <h2> caret@A']);
  });

  it('first turns each <br> around the selected lines into a paragraph break', () => {
    const ed = editor('<div>A<br>B<br>C<br>D</div>');
    const { calls, exec } = recorder();
    applyBlockStyle(ed, range(text(ed, 'B'), 0, text(ed, 'C'), 1), 'div', exec);
    expect(calls).toEqual([
      'insertParagraph select:BR',
      'insertParagraph select:BR',
      'insertParagraph select:BR',
      'formatBlock <div> caret@C',
      'formatBlock <div> caret@B',
    ]);
  });

  it('a caret on one line of a merged block splits only that line out', () => {
    const ed = editor('<div>A<br>B<br>C</div>');
    const { calls, exec } = recorder();
    applyBlockStyle(ed, range(text(ed, 'B'), 0), 'h2', exec);
    expect(calls).toEqual(['insertParagraph select:BR', 'insertParagraph select:BR', 'formatBlock <h2> caret@B']);
  });

  it('a line in a block of its own needs no split', () => {
    const ed = editor('<div>A</div><div>B</div>');
    const { calls, exec } = recorder();
    applyBlockStyle(ed, range(text(ed, 'B'), 0), 'h1', exec);
    expect(calls).toEqual(['formatBlock <h1> caret@B']);
  });

  it('leaves the selection over the same text', () => {
    const ed = editor('<p>Aa</p><p>Bb</p><p>Cc</p>');
    applyBlockStyle(ed, range(text(ed, 'Aa'), 1, text(ed, 'Cc'), 1), 'div', () => {});
    expect(document.getSelection()!.toString()).toBe('aBbC');
  });
});
