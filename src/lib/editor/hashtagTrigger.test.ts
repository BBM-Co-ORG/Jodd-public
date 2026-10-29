// @vitest-environment jsdom
// Task 6.3 (spec 2026-10-20 §6.4): the pure half of #-autocomplete — when a
// `#` the user is typing asks for the tag list, and which tags it shows.
import { describe, it, expect } from 'vitest';
import { hashtagQueryAtCaret, rankTags, pickerKeyAction, hashtagVocabulary, enterPicks, caretTextContext, HASHTAG_PICKER_LIMIT } from './hashtagTrigger';

describe('hashtagQueryAtCaret', () => {
  it('returns the prefix typed after a # that starts a word', () => {
    expect(hashtagQueryAtCaret('hello #stu', false)).toBe('stu');
    expect(hashtagQueryAtCaret('#stu', false)).toBe('stu');
    expect(hashtagQueryAtCaret('#', false)).toBe('');
    expect(hashtagQueryAtCaret('hello #', false)).toBe('');
  });

  it('triggers after punctuation, not after a letter or digit', () => {
    expect(hashtagQueryAtCaret('(#st', false)).toBe('st');
    expect(hashtagQueryAtCaret('“#st', false)).toBe('st');
    expect(hashtagQueryAtCaret('a#b', false)).toBeNull();
    expect(hashtagQueryAtCaret('1#b', false)).toBeNull();
    // Not after `_` or `-`: db::extract_hashtags would not read the result
    // as a tag either.
    expect(hashtagQueryAtCaret('foo-#b', false)).toBeNull();
    expect(hashtagQueryAtCaret('foo_#b', false)).toBeNull();
  });

  it('closes once the prefix holds whitespace or punctuation', () => {
    expect(hashtagQueryAtCaret('#stu ', false)).toBeNull();
    expect(hashtagQueryAtCaret('#stu dy', false)).toBeNull();
    expect(hashtagQueryAtCaret('#stu.', false)).toBeNull();
    expect(hashtagQueryAtCaret('#stu,', false)).toBeNull();
    expect(hashtagQueryAtCaret('#stu ', false)).toBeNull();
    // A hyphen is a tag character (kebab-case tags), as in extractHashtags.
    expect(hashtagQueryAtCaret('#local-fi', false)).toBe('local-fi');
  });

  it('is off in code, a link, or a URL-looking token', () => {
    expect(hashtagQueryAtCaret('x #code', true)).toBeNull();
    expect(hashtagQueryAtCaret('see https://x.com/#anchor', false)).toBeNull();
    expect(hashtagQueryAtCaret('see x.com/#', false)).toBeNull();
    expect(hashtagQueryAtCaret('mailto:#x', false)).toBeNull();
  });

  it('counts Thai letters and marks as tag characters (Review Focus 2)', () => {
    expect(hashtagQueryAtCaret('งาน #ภาษ', false)).toBe('ภาษ');
    // ิ and ่ are combining marks (\p{M}), not word breaks.
    expect(hashtagQueryAtCaret('#ศิลป์', false)).toBe('ศิลป์');
    expect(hashtagQueryAtCaret('#ที่', false)).toBe('ที่');
    // A # straight after a Thai letter or mark is inside a word.
    expect(hashtagQueryAtCaret('งาน#ภาษ', false)).toBeNull();
    expect(hashtagQueryAtCaret('ที่#ภาษ', false)).toBeNull();
  });
});

describe('rankTags', () => {
  it('prefix matches first, then substring matches', () => {
    expect(rankTags('st', ['study', 'test', 'other'])).toEqual(['study', 'test']);
    expect(rankTags('st', ['test', 'study', 'stack'])).toEqual(['study', 'stack', 'test']);
  });

  it('matches Thai', () => {
    expect(rankTags('ภาษ', ['ภาษาไทย', 'english'])).toEqual(['ภาษาไทย']);
    expect(rankTags('ไทย', ['ภาษาไทย', 'ไทยแลนด์'])).toEqual(['ไทยแลนด์', 'ภาษาไทย']);
  });

  it('normalizes the prefix like a tag (case, a stray #)', () => {
    expect(rankTags('ST', ['study'])).toEqual(['study']);
    expect(rankTags('#st', ['study'])).toEqual(['study']);
  });

  it('keeps the vocabulary order within each group', () => {
    expect(rankTags('a', ['banana', 'apple', 'cat', 'avocado'])).toEqual(['apple', 'avocado', 'banana', 'cat']);
  });

  it('an empty prefix lists the vocabulary, up to the limit', () => {
    const many = Array.from({ length: 12 }, (_, i) => `t${String(i).padStart(2, '0')}`);
    expect(HASHTAG_PICKER_LIMIT).toBe(8);
    expect(rankTags('', many)).toEqual(many.slice(0, 8));
    expect(rankTags('t', many, 3)).toEqual(many.slice(0, 3));
  });

  it('no match is an empty list, and duplicates appear once', () => {
    expect(rankTags('zz', ['study', 'test'])).toEqual([]);
    expect(rankTags('st', ['study', 'study'])).toEqual(['study']);
  });
});

describe('pickerKeyAction', () => {
  it('moves with the arrows, wrapping', () => {
    expect(pickerKeyAction('ArrowDown', 0, 3)).toEqual({ move: 1 });
    expect(pickerKeyAction('ArrowDown', 2, 3)).toEqual({ move: 0 });
    expect(pickerKeyAction('ArrowUp', 0, 3)).toEqual({ move: 2 });
  });
  it('Enter and Tab pick, Escape closes, anything else is not the picker’s', () => {
    expect(pickerKeyAction('Enter', 1, 3)).toBe('pick');
    expect(pickerKeyAction('Tab', 1, 3)).toBe('pick');
    expect(pickerKeyAction('Escape', 1, 3)).toBe('close');
    expect(pickerKeyAction('a', 1, 3)).toBeNull();
  });
  it('an empty list takes no keys', () => {
    expect(pickerKeyAction('Enter', 0, 0)).toBeNull();
    expect(pickerKeyAction('ArrowDown', 0, 0)).toBeNull();
  });
  it('takes no keys mid-composition (IME: Thai, Android keyboards)', () => {
    expect(pickerKeyAction('Enter', 0, 3, { isComposing: true })).toBeNull();
    expect(pickerKeyAction('Enter', 0, 3, { keyCode: 229 })).toBeNull();
    expect(pickerKeyAction('ArrowDown', 0, 3, { keyCode: 229 })).toBeNull();
  });
});

describe('hashtagVocabulary', () => {
  // The open note's tags reach tagsByAccount live as they are typed
  // (refreshBodyTags), so `#stu` half-typed is already a "tag" there. Listing
  // it would suggest what the user just typed, and no match could ever be
  // empty.
  const cloud = [
    { tag: 'stu', count: 1 },
    { tag: 'study', count: 3 },
    { tag: 'work', count: 1 },
  ];
  it('leaves out the tag being typed when only the open note has it', () => {
    expect(hashtagVocabulary(cloud, ['stu', 'work'], 'stu')).toEqual(['study', 'work']);
  });
  it('keeps it when another note has it too', () => {
    expect(hashtagVocabulary([{ tag: 'stu', count: 2 }], ['stu'], 'stu')).toEqual(['stu']);
  });
  it('keeps it when the open note is not in the store (a tmp: note)', () => {
    expect(hashtagVocabulary(cloud, [], 'stu')).toEqual(['stu', 'study', 'work']);
  });
  it('keeps the open note\'s other tags', () => {
    expect(hashtagVocabulary(cloud, ['stu', 'work'], 'st')).toEqual(['stu', 'study', 'work']);
  });
});

describe('enterPicks (ruling R31)', () => {
  it('a bare # never picks: Enter is a new line', () => {
    expect(enterPicks('', ['study'], false)).toBe(false);
    expect(enterPicks('', ['study'], true)).toBe(false);
  });
  it('picks when the top row is a prefix match', () => {
    expect(enterPicks('st', ['study', 'test'], false)).toBe(true);
    expect(enterPicks('ภาษ', ['ภาษาไทย'], false)).toBe(true);
  });
  it('a substring-only top row picks only once the user moved the highlight', () => {
    expect(enterPicks('ud', ['study'], false)).toBe(false);
    expect(enterPicks('ud', ['study'], true)).toBe(true);
  });
  it('an empty list never picks', () => {
    expect(enterPicks('st', [], true)).toBe(false);
  });
});

describe('caretTextContext', () => {
  function ctx(html: string, pick: (root: HTMLElement) => [Text, number]) {
    const root = document.createElement('div');
    root.innerHTML = html;
    const [node, offset] = pick(root);
    return caretTextContext(root, node, offset);
  }
  const text = (root: HTMLElement, sel: string) => root.querySelector(sel)!.firstChild as Text;

  it('reads across inline elements, so <span>hello</span>#st is hello#st', () => {
    const c = ctx('<div><span>hello</span>#st</div>', (r) => [r.querySelector('div')!.lastChild as Text, 3]);
    expect(c.before).toBe('hello#st');
    expect(hashtagQueryAtCaret(c.before, false)).toBeNull();
    expect(c.locate(5)!.offset).toBe(0);
    expect(c.locate(5)!.node.data).toBe('#st');
  });

  it('a <br> or the block start is a line start', () => {
    const c = ctx('<div>word<br>#st</div>', (r) => [r.querySelector('div')!.lastChild as Text, 3]);
    expect(c.before).toBe('word\n#st');
    expect(hashtagQueryAtCaret(c.before, false)).toBe('st');
    const d = ctx('<div>other line</div><div>#st</div>', (r) => [r.children[1].firstChild as Text, 3]);
    expect(d.before).toBe('#st');
  });

  it('a prefix split over elements maps back to the # node', () => {
    const c = ctx('<div>a #s<b>tu</b></div>', (r) => [text(r, 'b'), 2]);
    expect(c.before).toBe('a #stu');
    expect(c.locate(2)!.node.data).toBe('a #s');
    expect(c.locate(2)!.offset).toBe(2);
  });

  it('knows when the caret is inside a word (#wo|rk), across inline elements too', () => {
    expect(ctx('<div>#work</div>', (r) => [text(r, 'div'), 3]).continuesWord).toBe(true);
    expect(ctx('<div>#wo<b>rk</b></div>', (r) => [text(r, 'div'), 3]).continuesWord).toBe(true);
    expect(ctx('<div>#work</div>', (r) => [text(r, 'div'), 5]).continuesWord).toBe(false);
    expect(ctx('<div>#work and</div>', (r) => [text(r, 'div'), 5]).continuesWord).toBe(false);
    expect(ctx('<div>#work<br>next</div>', (r) => [text(r, 'div'), 5]).continuesWord).toBe(false);
    expect(ctx('<div>#ภา</div>', (r) => [text(r, 'div'), 2]).continuesWord).toBe(true);
  });
});
