import { describe, it, expect } from 'vitest';
import { byUse, unionTags } from './tagOrder';

describe('byUse', () => {
  it('puts the tags on the most notes first, ties A–Z', () => {
    const tags = [
      { tag: 'ai', count: 1 }, { tag: 'work', count: 9 }, { tag: 'music', count: 3 },
      { tag: 'idea', count: 3 }, { tag: 'ดนตรี', count: 9 },
    ];
    expect(byUse(tags).map((t) => t.tag)).toEqual(['work', 'ดนตรี', 'idea', 'music', 'ai']);
  });

  it('leaves its input alone', () => {
    const tags = [{ tag: 'b', count: 1 }, { tag: 'a', count: 2 }];
    byUse(tags);
    expect(tags.map((t) => t.tag)).toEqual(['b', 'a']);
  });
});

describe('unionTags', () => {
  it('one tag per name, counts summed across accounts', () => {
    const u = unionTags([
      [{ tag: 'music', count: 2 }, { tag: 'work', count: 1 }],
      [{ tag: 'music', count: 3 }, { tag: 'ดนตรี', count: 1 }],
    ]);
    expect(byUse(u)).toEqual([
      { tag: 'music', count: 5 }, { tag: 'work', count: 1 }, { tag: 'ดนตรี', count: 1 },
    ]);
  });
});
