// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { substringMatches, hashtagMatches, findTextRanges, replaceRanges, planTagInsertion, checkboxBesideCaret } from './bodyEdits';

// jsdom has no execCommand; what the commands do is WebKit's behaviour and was
// measured there (see bodyEdits.ts). Here: which ranges, in what order, with
// which commands.
type Call = { args: unknown[]; selected: string };
let calls: Call[];
beforeEach(() => {
  document.body.innerHTML = '';
  calls = [];
  const exec = vi.fn((...args: unknown[]) => {
    const r = window.getSelection()!.getRangeAt(0);
    const where = r.collapsed ? `|${(r.startContainer as Text).data?.slice(r.startOffset) ?? ''}` : r.toString();
    calls.push({ args, selected: `${where} in ${r.startContainer.parentElement?.tagName}@${r.startOffset}` });
    return true;
  });
  Object.defineProperty(document, 'execCommand', { value: exec, configurable: true, writable: true });
});

function editor(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  document.body.appendChild(el);
  return el;
}

describe('substringMatches', () => {
  it('finds every case-insensitive, non-overlapping occurrence', () => {
    expect(substringMatches('foo')('Foo one fOO foofoo')).toEqual([[0, 3], [8, 11], [12, 15], [15, 18]]);
    expect(substringMatches('aa')('aaa')).toEqual([[0, 2]]);
    expect(substringMatches('')('anything')).toEqual([]);
  });

  it("indexes the ORIGINAL text when lower-casing changes length ('İ' → 'i̇')", () => {
    const end = 'İzmir notes foo';
    expect(substringMatches('foo')(end)).toEqual([[12, 15]]);
    expect(end.slice(12, 15)).toBe('foo');
    const mid = 'İzmir bar foo';
    expect(substringMatches('bar')(mid).map(([s, e]) => mid.slice(s, e))).toEqual(['bar']);
    expect(substringMatches('İzmir')('see İzmir')).toEqual([[4, 9]]);
  });

  it('treats final and non-final sigma as one letter, in either direction', () => {
    expect(substringMatches('οδος')('ΟΔΟΣ')).toEqual([[0, 4]]);
    expect(substringMatches('ΟΔΟΣ')('οδος')).toEqual([[0, 4]]);
    expect(substringMatches('οδοσ')('στην οδος')).toEqual([[5, 9]]);
  });

  it('only matches whole characters — "i" is not half of "İ"', () => {
    expect(substringMatches('i')('İ')).toEqual([]);
    expect(substringMatches('izmir')('İzmir')).toEqual([]);
  });
});

describe('checkboxBesideCaret', () => {
  function caret(node: Node, offset: number): Range {
    const r = document.createRange();
    r.setStart(node, offset);
    r.collapse(true);
    return r;
  }

  it('sees a checkbox on either side of the start of a task row', () => {
    const row = editor('<div><input type="checkbox" contenteditable="false">&nbsp;ship it</div>').firstElementChild!;
    const text = row.lastChild!;
    expect(checkboxBesideCaret(caret(row, 0))).toBe(true);
    expect(checkboxBesideCaret(caret(row, 1))).toBe(true);
    expect(checkboxBesideCaret(caret(text, 0))).toBe(true);
  });

  it('does not see one on a plain line or mid-row', () => {
    const plain = editor('<div>ship it</div>').firstElementChild!;
    expect(checkboxBesideCaret(caret(plain.firstChild!, 0))).toBe(false);
    const row = editor('<div><input type="checkbox">&nbsp;ship it</div>').firstElementChild!;
    expect(checkboxBesideCaret(caret(row.lastChild!, 3))).toBe(false);
  });
});

describe('hashtagMatches', () => {
  it('matches the whole #tag only, and leaves the character before it out of the range', () => {
    const text = 'a #work b #working c#work #work';
    const hits = hashtagMatches('work')(text).map(([s, e]) => [s, text.slice(s, e)]);
    expect(hits).toEqual([[2, '#work'], [26, '#work']]);
  });

  it('escapes regex metacharacters in the tag and handles Thai', () => {
    expect(hashtagMatches('c++')('learn #c++ now')).toEqual([[6, 10]]);
    expect(hashtagMatches('งาน')('ทำ #งาน เสร็จ')).toEqual([[3, 7]]);
  });
});

describe('findTextRanges', () => {
  it('returns ranges in document order across text nodes, including inside inline formatting', () => {
    const el = editor('<div>foo one foo</div><div>two <b>foo</b></div>');
    const ranges = findTextRanges(el, substringMatches('foo'));
    expect(ranges.map((r) => [r.toString(), r.startContainer.parentElement!.tagName, r.startOffset])).toEqual([
      ['foo', 'DIV', 0],
      ['foo', 'DIV', 8],
      ['foo', 'B', 0],
    ]);
  });
});

describe('replaceRanges', () => {
  it('replaces last-to-first with insertText, and issues nothing else', () => {
    const el = editor('<div>foo one foo</div><div>two <b>foo</b></div>');
    const n = replaceRanges(el, findTextRanges(el, substringMatches('foo')), 'QUX');
    expect(n).toBe(3);
    // Last-first: the <b> match is replaced before the ones earlier in the note.
    expect(calls.map((c) => [c.args[0], c.selected])).toEqual([
      ['insertText', 'foo in B@0'],
      ['insertText', 'foo in DIV@8'],
      ['insertText', 'foo in DIV@0'],
    ]);
    expect(calls[0].args).toEqual(['insertText', false, 'QUX']);
  });

  it('deletes when the replacement is empty, and does nothing without ranges', () => {
    const el = editor('<div>a #work b</div>');
    replaceRanges(el, findTextRanges(el, hashtagMatches('work')), '');
    expect(calls.map((c) => [c.args[0], c.selected])).toEqual([
      ['delete', '#work in DIV@2'],
    ]);
    calls = [];
    expect(replaceRanges(el, [], 'x')).toBe(0);
    expect(calls).toEqual([]);
  });

  it('skips ranges that are no longer inside the editor', () => {
    const el = editor('<div>foo</div>');
    const outside = document.createElement('div');
    outside.textContent = 'foo';
    document.body.appendChild(outside);
    const stale = findTextRanges(outside, substringMatches('foo'));
    expect(replaceRanges(el, stale, 'x')).toBe(0);
    expect(calls).toEqual([]);
  });

  it('gives focus back to the field that had it', () => {
    const el = editor('<div>foo</div>');
    el.tabIndex = 0; // jsdom only focuses elements it considers focusable
    const field = document.createElement('input');
    document.body.appendChild(field);
    field.focus();
    replaceRanges(el, findTextRanges(el, substringMatches('foo')), 'bar');
    expect(document.activeElement).toBe(field);
  });
});

describe('planTagInsertion', () => {
  it('starts an empty last line (or an empty note) with the tag', () => {
    expect(planTagInsertion('', 'work')).toEqual({ newLine: false, text: '#work' });
    expect(planTagInsertion('  ', 'work')).toEqual({ newLine: false, text: '#work' });
  });

  it('appends to a trailing line of tags', () => {
    expect(planTagInsertion('#home', 'work')).toEqual({ newLine: false, text: ' #work' });
    expect(planTagInsertion('#home #errand ', 'work')).toEqual({ newLine: false, text: '#work' });
  });

  it('always opens a new line after a checklist row — an empty one, or one already holding tags', () => {
    expect(planTagInsertion(' ', 'work', true)).toEqual({ newLine: true, text: '#work' });
    expect(planTagInsertion(' #home', 'work', true)).toEqual({ newLine: true, text: '#work' });
  });

  it('opens a new line after prose — including prose that merely contains a tag', () => {
    expect(planTagInsertion('hello world', 'work')).toEqual({ newLine: true, text: '#work' });
    expect(planTagInsertion('call about #home', 'work')).toEqual({ newLine: true, text: '#work' });
  });
});
