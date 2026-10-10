// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { compileFind, editorLines, findBodyMatches, expandReplacement, type LineMatcher } from './findMatches';

function body(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  return el;
}

function matcher(query: string, matchCase = false, regex = false): LineMatcher {
  const c = compileFind(query, { matchCase, regex });
  if (!c.ok) throw new Error(c.error);
  return c.match;
}

const texts = (root: HTMLElement, m: LineMatcher) => findBodyMatches(root, m).map((b) => b.range.toString());

describe('editorLines: a line is what the note shows on one line', () => {
  it('splits on blocks and <br>, and joins inline formatting', () => {
    const root = body('<div>one <b>two</b></div><div>three<br>four</div><ul><li>five</li><li>six</li></ul>');
    expect(editorLines(root).map((l) => l.text).filter(Boolean)).toEqual(['one two', 'three', 'four', 'five', 'six']);
  });

  it('breaks on newlines only inside <pre>', () => {
    const root = body('<div>a\nb</div><pre>c\nd</pre>');
    expect(editorLines(root).map((l) => l.text).filter(Boolean)).toEqual(['a\nb', 'c', 'd']);
  });

  it('ignores the formatting whitespace between blocks', () => {
    const root = body('<div>a</div>\n  <div>b</div>');
    expect(editorLines(root).map((l) => l.text).filter((t) => t !== '')).toEqual(['a', 'b']);
  });
});

describe('compileFind: plain', () => {
  it('ignores case by default, keeps it when asked', () => {
    const root = body('<div>Line line LINE</div>');
    expect(texts(root, matcher('line'))).toEqual(['Line', 'line', 'LINE']);
    expect(texts(root, matcher('line', true))).toEqual(['line']);
  });

  it('treats regex characters literally when regex is off', () => {
    const root = body('<div>a.c abc ^a</div>');
    expect(texts(root, matcher('a.c'))).toEqual(['a.c']);
    expect(texts(root, matcher('^a'))).toEqual(['^a']);
  });

  it('finds a match that crosses inline formatting', () => {
    const root = body('<div>li<b>ne</b>1</div>');
    expect(texts(root, matcher('line'))).toEqual(['line']);
  });

  it('an empty query matches nothing', () => {
    expect(texts(body('<div>a</div>'), matcher(''))).toEqual([]);
  });
});

describe('compileFind: regex', () => {
  it('anchors ^ and $ to every line', () => {
    const root = body('<div>line1 line</div><div>line2</div><div>xline</div>');
    expect(texts(root, matcher('^line', false, true))).toEqual(['line', 'line']);
    expect(texts(root, matcher('line$', false, true))).toEqual(['line', 'line']);
    expect(findBodyMatches(root, matcher('^line\\d$', false, true)).map((b) => b.range.toString())).toEqual(['line2']);
  });

  it('respects match case', () => {
    const root = body('<div>Line line</div>');
    expect(texts(root, matcher('l\\w+', false, true))).toEqual(['Line', 'line']);
    expect(texts(root, matcher('l\\w+', true, true))).toEqual(['line']);
  });

  it('never matches across lines', () => {
    const root = body('<div>ab</div><div>cd</div>');
    expect(texts(root, matcher('b.c', false, true))).toEqual([]);
    expect(texts(root, matcher('b\\s*c', false, true))).toEqual([]);
  });

  it('skips zero-length matches', () => {
    const root = body('<div>ab</div><div></div>');
    expect(texts(root, matcher('^', false, true))).toEqual([]);
    expect(texts(root, matcher('x*', false, true))).toEqual([]);
    expect(texts(root, matcher('b?', false, true))).toEqual(['b']);
  });

  it('reports an invalid pattern instead of throwing', () => {
    const c = compileFind('(unclosed', { matchCase: false, regex: true });
    expect(c.ok).toBe(false);
  });

  it('accepts patterns only the non-unicode flavour allows', () => {
    const root = body('<div>a-b</div>');
    expect(texts(root, matcher('a\\-b', false, true))).toEqual(['a-b']);
  });

  it('carries capture groups', () => {
    const root = body('<div>John Smith</div>');
    const [m] = findBodyMatches(root, matcher('^(\\w+) (\\w+)$', false, true));
    expect(m.match.groups).toEqual(['John Smith', 'John', 'Smith']);
  });
});

describe('expandReplacement', () => {
  const m = { start: 0, end: 10, groups: ['John Smith', 'John', 'Smith', undefined] };

  it('substitutes $1..$9, $& and $$ in regex mode', () => {
    expect(expandReplacement('$2, $1', m, true)).toBe('Smith, John');
    expect(expandReplacement('[$&]', m, true)).toBe('[John Smith]');
    expect(expandReplacement('$$1', m, true)).toBe('$1');
  });

  it('an unmatched group is empty, a missing one stays literal', () => {
    expect(expandReplacement('<$3>', m, true)).toBe('<>');
    expect(expandReplacement('<$7>', m, true)).toBe('<$7>');
    expect(expandReplacement('$x $', m, true)).toBe('$x $');
  });

  it('is literal when regex is off', () => {
    expect(expandReplacement('$2 $& $$', m, false)).toBe('$2 $& $$');
  });
});

describe('findBodyMatches: note-wide offsets', () => {
  // Replace remembers what it inserted by these offsets, not by Range: a
  // Gmail save re-renders a fresh editor element and every Range dies with
  // the old one, while the offsets of the same text survive.
  it('counts every earlier line plus one break per line', () => {
    const root = body('<div>ab</div><div>c<b>d</b></div>');
    const [m] = findBodyMatches(root, matcher('cd'));
    expect([m.absStart, m.absEnd]).toEqual([3, 5]);
  });

  it('gives the same offsets for the same text in a fresh element', () => {
    const html = '<div>line1</div><div>line2</div>';
    const a = findBodyMatches(body(html), matcher('line')).map((m) => [m.absStart, m.absEnd]);
    const b = findBodyMatches(body(html), matcher('line')).map((m) => [m.absStart, m.absEnd]);
    expect(a).toEqual([[0, 4], [6, 10]]);
    expect(b).toEqual(a);
  });
});
