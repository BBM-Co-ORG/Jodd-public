// @vitest-environment jsdom
//
// jsdom has no execCommand. The stub edits the selection the way WebKit does
// for the two commands these edits use — insertText replaces what is
// selected and leaves the caret after it; insertParagraph splits the caret's
// line block there — and records each command with the text after the caret,
// so the tests pin which command runs where. What WebKit itself does with
// them is measured in bodyEdits.ts / blockStyle.ts; live look owed (report).
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { insertPropertyLine, applyFixIt, applyDueDate, caretBodyLine, ADD_PROPERTY_TEXT } from './propertyEdits';
import { bodyLines, propertyRanges } from './propertyPaint';
import { fixItAt } from './propertyTrigger';

const TODAY = new Date(2026, 10, 19);

type Call = { cmd: string; arg?: string; at: string };
let calls: Call[];
let el: HTMLElement;

function place(node: Node, offset: number) {
  const r = document.createRange();
  r.setStart(node, offset);
  r.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(r);
}

function lineBlock(node: Node): Element {
  let n: Node = node;
  while (n.parentNode && n.parentNode !== el) n = n.parentNode;
  return n as Element;
}

beforeEach(() => {
  document.body.innerHTML = '';
  el = document.createElement('div');
  el.contentEditable = 'true';
  document.body.appendChild(el);
  calls = [];
  const execCommand = vi.fn((cmd: string, _ui?: boolean, arg?: string) => {
    const sel = window.getSelection()!;
    const r = sel.getRangeAt(0);
    const c = r.startContainer;
    const after = c.nodeType === 3 ? (c as Text).data.slice(r.startOffset) : (c.childNodes[r.startOffset]?.textContent ?? '');
    calls.push({ cmd, arg, at: `|${after}` });
    if (cmd === 'insertText') {
      if (!r.collapsed) r.deleteContents();
      const node = r.startContainer;
      if (node.nodeType === 3) {
        (node as Text).insertData(r.startOffset, arg!);
        place(node, r.startOffset + arg!.length);
      } else {
        const t = document.createTextNode(arg!);
        r.insertNode(t);
        place(t, t.data.length);
      }
    } else if (cmd === 'insertParagraph') {
      const block = lineBlock(r.startContainer);
      const tail = document.createRange();
      tail.setStart(r.startContainer, r.startOffset);
      tail.setEnd(block, block.childNodes.length);
      const moved = tail.extractContents();
      const next = document.createElement(block.localName === 'li' ? 'li' : 'div');
      next.appendChild(moved);
      block.after(next);
      place(next, 0);
    }
    return true;
  });
  Object.defineProperty(document, 'execCommand', { value: execCommand, configurable: true, writable: true });
});

const selected = () => window.getSelection()!.toString();

describe('insertPropertyLine (toolbar Add property)', () => {
  it('with no block, puts `Key: ` on body line 0, the first line moved down whole', () => {
    el.innerHTML = '<div>Hello</div><div>world</div>';
    expect(insertPropertyLine(el)).toBe(0);
    expect(calls.map((c) => [c.cmd, c.at])).toEqual([
      ['insertText', '|Hello'],
      ['insertParagraph', '|Hello'],
    ]);
    expect(el.innerHTML).toBe('<div>Key: </div><div>Hello</div><div>world</div>');
    expect(selected()).toBe('Key');
  });

  it('with a block, puts it at the end of the block, keeping it contiguous', () => {
    el.innerHTML = '<div>Status: active</div><div>Owner: Kai</div><div><br></div><div>body</div>';
    expect(insertPropertyLine(el)).toBe(2);
    expect(calls.map((c) => [c.cmd, c.arg])).toEqual([
      ['insertParagraph', undefined],
      ['insertText', ADD_PROPERTY_TEXT],
    ]);
    expect(bodyLines(el).map((l) => l.text).slice(0, 3)).toEqual(['Status: active', 'Owner: Kai', 'Key:']);
    expect(selected()).toBe('Key');
  });

  it('splits after trailing whitespace, so the new key has no leading space', () => {
    el.innerHTML = '<div>Status: active </div><div>body</div>';
    insertPropertyLine(el);
    expect(bodyLines(el).map((l) => l.text).slice(0, 2)).toEqual(['Status: active', 'Key:']);
  });

  it('types into an empty body', () => {
    el.innerHTML = '';
    expect(insertPropertyLine(el)).toBe(0);
    expect(el.textContent).toBe('Key: ');
    expect(selected()).toBe('Key');
  });

  it('a task row on line 0 moves down with its checkbox', () => {
    el.innerHTML = '<div><input type="checkbox"> ship it</div>';
    expect(insertPropertyLine(el)).toBe(0);
    expect(el.innerHTML).toBe('<div>Key: </div><div><input type="checkbox"> ship it</div>'.replace(' ', '&nbsp;'));
    expect(selected()).toBe('Key');
  });

});

describe('caretBodyLine', () => {
  it("finds the caret's line, the one before the caret when a node holds several", () => {
    el.innerHTML = '<div>Status: a</div><pre>Owner: b\nDue: c</pre>';
    const lines = bodyLines(el);
    expect(caretBodyLine(lines, el.firstChild!.firstChild!, 3)).toBe(0);
    const pre = el.lastChild!.firstChild!;
    expect(caretBodyLine(lines, pre, 0)).toBe(1);
    expect(caretBodyLine(lines, pre, 10)).toBe(2);
    expect(caretBodyLine(lines, document.createTextNode('x'), 0)).toBe(-1);
  });
});

describe('applyFixIt — only on an explicit accept, only what the underline stands for', () => {
  it('adds the missing space', () => {
    el.innerHTML = '<div>Status:active</div><div>body</div>';
    expect(applyFixIt(el, 0, 'Status:active', TODAY)).toBe(true);
    expect(el.innerHTML).toBe('<div>Status: active</div><div>body</div>');
  });

  it('rewrites a date under a date key, Buddhist year included', () => {
    el.innerHTML = '<div>Status: active</div><div>Due: 25/11/2569</div>';
    expect(applyFixIt(el, 1, 'Due: 25/11/2569', TODAY)).toBe(true);
    expect(el.textContent).toBe('Status: activeDue: 2026-11-25');
  });

  it('never rewrites a ratio or a time', () => {
    el.innerHTML = '<div>Ratio: 1/2</div><div>Meeting at 10:30</div>';
    expect(applyFixIt(el, 0, 'Ratio: 1/2', TODAY)).toBe(false);
    expect(applyFixIt(el, 1, 'Meeting at 10:30', TODAY)).toBe(false);
    el.innerHTML = '<div>Ratio:1/2</div>';
    expect(applyFixIt(el, 0, 'Ratio:1/2', TODAY)).toBe(true);
    expect(el.textContent).toBe('Ratio: 1/2');
  });

  it('keeps a link on the line: the space goes in, the <a href> stays', () => {
    el.innerHTML = '<div>Source:<a href="https://x.com/a">Article</a></div><div>body</div>';
    expect(applyFixIt(el, 0, 'Source:Article', TODAY)).toBe(true);
    expect(el.innerHTML).toBe('<div>Source: <a href="https://x.com/a">Article</a></div><div>body</div>');
  });

  it('keeps an inline span: only the date characters change', () => {
    el.innerHTML = '<div><b>Status</b>: a</div><div><b>Due</b>: 25/11/2569</div>';
    expect(applyFixIt(el, 1, 'Due: 25/11/2569', TODAY)).toBe(true);
    expect(el.innerHTML).toBe('<div><b>Status</b>: a</div><div><b>Due</b>: 2026-11-25</div>');
  });

  it('does nothing when the line changed since the offer', () => {
    el.innerHTML = '<div>Status:activ</div>';
    expect(applyFixIt(el, 0, 'Status:active', TODAY)).toBe(false);
    expect(calls).toEqual([]);
  });
});

describe('applyDueDate (toolbar Due date)', () => {
  const row = (html: string) => {
    el.innerHTML = html;
    return el.firstElementChild!;
  };

  it('appends ` 📅 <date>` to a row without one', () => {
    const r = row('<div><input type="checkbox"> ship it</div>');
    expect(applyDueDate(el, r, '2026-11-25')).toBe(true);
    expect(r.textContent).toBe(' ship it 📅 2026-11-25');
  });

  it('replaces the existing date: never a second marker', () => {
    const r = row('<div><input type="checkbox"> ship 📅 2026-11-20 <b>soon</b></div>');
    applyDueDate(el, r, '2026-11-25');
    expect(r.textContent).toBe(' ship 📅 2026-11-25 soon');
    expect(r.textContent!.split('📅')).toHaveLength(2);
  });

  it('fills an empty row after its checkbox', () => {
    const r = row('<div><input type="checkbox"></div>');
    applyDueDate(el, r, '2026-11-25');
    expect(r.innerHTML).toBe('<input type="checkbox">📅 2026-11-25');
  });
});

describe('fix-it and the wavy underline agree', () => {
  // One line per <div>, so the paint's gutter line is the body line.
  const bodies = [
    ['Status:active', 'text'],
    ['Due: 25/11', 'Status: a'],
    ['Owner: Kai', 'Due: 25/11/2569'],
    ['Ratio: 1/2'],
    ['Ratio:1/2'],
    ['Meeting at 10:30'],
    ['Status: a', 'Meeting at 10:30'],
    ['Q: x', 'Start: 1/2/2026', 'Note:1/2'],
    ['see https://x.com/a'],
    ['text', 'Status:active'],
    ['สถานะ:รอ'],
  ];
  it.each(bodies.map((b) => [b.join(' | '), b]))('%s', (_name, lines) => {
    el.innerHTML = (lines as string[]).map((l) => `<div>${l}</div>`).join('');
    const warned = new Set(propertyRanges(el, TODAY).filter((i) => i.kind === 'prop-warn').map((i) => i.line));
    const offered = new Set((lines as string[]).map((_, i) => i).filter((i) => fixItAt(lines as string[], i, TODAY) !== null));
    expect([...offered].sort()).toEqual([...warned].sort());
  });
});
