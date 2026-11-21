// @vitest-environment jsdom
//
// What a "line" is in a note: one paragraph, list item or heading, however
// many screen lines it wraps onto. The tooltip's count and the editor's
// line-number gutter both come from noteLines, so they cannot disagree.
import { describe, it, expect } from 'vitest';
import { noteLines, noteStats, statsLine } from './noteLines';

function root(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  return el;
}
const texts = (html: string) => noteLines(root(html)).map((l) => l.text);

describe('noteLines', () => {
  it('one line per Apple Notes <div>, a blank <div><br></div> included', () => {
    expect(texts('<div>one</div><div><br></div><div>three</div>')).toEqual(['one', '', 'three']);
  });

  it('an empty block with no <br> takes no height, so it is not a line (gutter showed 139,140,141 jammed)', () => {
    expect(texts('<div>a</div><div></div><div> <span></span> </div><p></p><div>b</div>')).toEqual(['a', 'b']);
    // Still lines: an image, an empty list item (it shows its marker).
    expect(noteLines(root('<div><img src="x.png"></div><ul><li></li></ul>'))).toHaveLength(2);
  });

  it('a paragraph with <br>s inside is still ONE line (a paragraph, not a screen line)', () => {
    // The <br> still separates words: "a" and "b" are two.
    expect(texts('<p>a<br>b</p><p>c</p>')).toEqual(['a b', 'c']);
  });

  it('list items and nested lists: the item text before a sublist is its own line', () => {
    expect(texts('<ul><li>a<ul><li>b</li><li>c</li></ul></li><li>d</li></ul>')).toEqual(['a', 'b', 'c', 'd']);
  });

  it('headings, blockquote lines and a table row each count once', () => {
    expect(texts('<h1>T</h1><blockquote><div>q1</div><div>q2</div></blockquote><table><tr><td>x</td><td>y</td></tr></table>'))
      .toEqual(['T', 'q1', 'q2', 'x y']);
  });

  it('bare text between blocks is a line of its own; formatting whitespace is not', () => {
    expect(texts('typed first\n<div>second</div>\n  <div>third</div>')).toEqual(['typed first', 'second', 'third']);
  });

  it('inline formatting does not split a word or a line', () => {
    expect(texts('<div><b>he</b>llo <i>world</i></div>')).toEqual(['hello world']);
  });

  it('a closed <details> is its summary line only; open, it shows its content', () => {
    const closed = '<details><summary>Source</summary><div>s1</div><div>s2</div></details>';
    expect(texts(closed)).toEqual(['Source']);
    expect(texts(closed.replace('<details>', '<details open>'))).toEqual(['Source', 's1', 's2']);
  });

  it('skips <style>, <script> and <hr>', () => {
    expect(texts('<style>.x{}</style><div>a</div><hr><script>1</script><div>b</div>')).toEqual(['a', 'b']);
  });

  it('a line with only an image is still a line', () => {
    expect(texts('<div><img src="x"></div><div>after</div>')).toEqual(['', 'after']);
  });

  it('each line carries the node to measure for the gutter', () => {
    const r = root('<div>a</div>bare<div>c</div>');
    const lines = noteLines(r);
    expect(lines[0].nodes[0]).toBe(r.children[0]);
    expect(lines[1].nodes[0].nodeType).toBe(Node.TEXT_NODE);
  });
});

describe('noteStats', () => {
  it('counts Thai words with the segmenter, not by spaces', () => {
    const s = noteStats('', '<div>ประชุมวางแผนสัปดาห์หน้า กับ team</div>');
    expect(s.words).toBe(6);
  });

  it('counts characters as graphemes: a Thai cluster or a skin-toned emoji is one', () => {
    expect(noteStats('', '<div>ที่ 👍🏽</div>').characters).toBe(3);
  });

  it('counts what the reader sees: the title, and a closed <details> as its summary alone', () => {
    // A saved source transcript folded under a summary is not the note's
    // words: live, one such note read 24,382 words for a few hundred shown.
    const closed = '<div>one two</div><details><summary>Src</summary><div>three four five</div></details>';
    const s = noteStats('My title', closed);
    expect(s).toEqual({ words: 2 + 2 + 1, characters: 'My title'.length + 'one two'.length + 'Src'.length, lines: 2 });
    // Opened, the same text is shown, so it counts.
    const open = noteStats('My title', closed.replace('<details>', '<details open>'));
    expect(open).toEqual({ words: 2 + 2 + 1 + 3, characters: s.characters + 'three four five'.length, lines: 3 });
  });

  it('decodes entities and collapses whitespace before counting', () => {
    const s = noteStats('', '<div>a&nbsp;&amp;&nbsp;b</div>');
    expect(s.characters).toBe('a & b'.length);
    expect(s.words).toBe(2);
  });

  it('reads a whole saved document, not only a fragment', () => {
    const s = noteStats('', '<html><head></head><body style="x"><div>a</div><div>b</div></body></html>');
    expect(s.lines).toBe(2);
  });

  it('an empty note is all zeros', () => {
    expect(noteStats('', '')).toEqual({ words: 0, characters: 0, lines: 0 });
  });
});

describe('statsLine', () => {
  it('plural forms with grouped numbers', () => {
    expect(statsLine({ words: 1, characters: 1234, lines: 2 })).toBe('1 word · 1,234 characters · 2 lines');
  });
});
