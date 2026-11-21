// @vitest-environment jsdom
//
// The editor shows what Jodd recognized — the property block and task due
// dates — as CSS highlights, never as markup (spec 2026-11-19 §7). jsdom has
// neither CSS.highlights nor layout, so highlights are a stubbed Map.
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import bodyLineCases from '../../../tests/fixtures/body-lines.json';
import {
  PROP_HL_GONE,
  PROP_HL_PRIORITY,
  bodyLines,
  createPropertyPainter,
  highlightName,
  propertyHintAt,
  propertyRanges,
  relativeDay,
  type PaintKind,
} from './propertyPaint';

class FakeHighlight {
  ranges: Range[];
  priority?: number;
  constructor(...ranges: Range[]) {
    this.ranges = ranges;
  }
}

let highlights: Map<string, FakeHighlight>;
let deleted: string[];

beforeEach(() => {
  highlights = new Map();
  deleted = [];
  const original = highlights.delete.bind(highlights);
  highlights.delete = ((k: string) => {
    deleted.push(k);
    return original(k);
  }) as typeof highlights.delete;
  vi.stubGlobal('CSS', { highlights });
  vi.stubGlobal('Highlight', FakeHighlight);
});
afterEach(() => {
  vi.unstubAllGlobals();
});

// 2026-11-19, local noon: no time-zone edge.
const TODAY = new Date(2026, 10, 19, 12, 0, 0);

function body(html: string): HTMLElement {
  const root = document.createElement('div');
  root.innerHTML = html;
  return root;
}
const painted = (root: Element) =>
  propertyRanges(root, TODAY).map((i) => ({ kind: i.kind, text: i.range.toString(), line: i.line, hint: i.hint }));
const kinds = (root: Element) => propertyRanges(root, TODAY).map((i) => [i.kind, i.range.toString()]);

const task = (text: string, checked = false) =>
  `<div class="jodd-task"><input type="checkbox"${checked ? ' checked=""' : ''}>&nbsp;${text}</div>`;

describe('bodyLines agrees with the deriver (tests/fixtures/body-lines.json, Rust html_to_lines)', () => {
  for (const c of bodyLineCases) {
    it(c.name, () => {
      expect(bodyLines(body(c.html)).map((l) => l.text)).toEqual(c.lines);
    });
  }
});

describe('propertyRanges — the property block', () => {
  it('paints each block line as a muted key and a value', () => {
    const root = body('<div>Status:&nbsp;active</div><div>Owner: Kai</div><div><br></div><div>text</div>');
    expect(painted(root)).toEqual([
      { kind: 'prop-key', text: 'Status', line: 0, hint: undefined },
      { kind: 'prop-value', text: 'active', line: 0, hint: undefined },
      { kind: 'prop-key', text: 'Owner', line: 1, hint: undefined },
      { kind: 'prop-value', text: 'Kai', line: 1, hint: undefined },
    ]);
  });

  it('a Thai key and value paint', () => {
    expect(kinds(body('<div>สถานะ:&nbsp;รอ</div>'))).toEqual([
      ['prop-key', 'สถานะ'],
      ['prop-value', 'รอ'],
    ]);
  });

  it('a line below a blank line paints nothing', () => {
    expect(kinds(body('<div>Status: active</div><div><br></div><div>Type: project</div>'))).toEqual([
      ['prop-key', 'Status'],
      ['prop-value', 'active'],
    ]);
  });

  it('nothing paints when the first line is not a property', () => {
    expect(kinds(body('<div>met the client</div><div>Status: active</div>'))).toEqual([]);
  });

  it('a value spread over inline elements is one range', () => {
    expect(kinds(body('<div><b>Status:</b> <i>very</i> active</div>'))).toEqual([
      ['prop-key', 'Status'],
      ['prop-value', 'very active'],
    ]);
  });

  // Where the editor's own noteLines and the deriver's html_to_lines could
  // disagree: noteLines makes `<div>a<br>b</div>` ONE line ("Status: a Owner:
  // b", one property whose value swallows the second); the deriver makes two.
  it('a <br> inside a block splits lines as the deriver does, not as noteLines does', () => {
    const root = body('<div>Status: a<br>Owner: b</div>');
    expect(kinds(root)).toEqual([
      ['prop-key', 'Status'],
      ['prop-value', 'a'],
      ['prop-key', 'Owner'],
      ['prop-value', 'b'],
    ]);
    // Both sit on gutter line 0: noteLines numbers the <div> once.
    expect(propertyRanges(root, TODAY).map((i) => i.line)).toEqual([0, 0, 0, 0]);
  });

  it('a list item, an h1–h3 heading or a task row is not a property line (the deriver prefixes them)', () => {
    expect(kinds(body('<ul><li>Status: x</li></ul>'))).toEqual([]);
    expect(kinds(body('<h2>Status: x</h2>'))).toEqual([]);
    expect(kinds(body(task('Status: x')))).toEqual([]);
  });

  it('an empty leading <div> is a blank first line to the deriver: no block', () => {
    expect(kinds(body('<div></div><div>Status: x</div>'))).toEqual([]);
  });

  it('gutter lines are noteLines indices', () => {
    const root = body('<div>Status: a</div><div>Owner: b</div><div><br></div><div>x</div>');
    expect(propertyRanges(root, TODAY).filter((i) => i.kind === 'prop-key').map((i) => i.line)).toEqual([0, 1]);
  });
});

describe('propertyRanges — prop-warn (conservative)', () => {
  it('Due: 25/11 in the block warns on its value; the key still paints', () => {
    expect(kinds(body('<div>Due: 25/11</div>'))).toEqual([
      ['prop-key', 'Due'],
      ['prop-warn', '25/11'],
    ]);
  });

  it('a Buddhist-year date warns whatever the key', () => {
    expect(kinds(body('<div>Kickoff: 25/11/2569</div>'))).toEqual([
      ['prop-key', 'Kickoff'],
      ['prop-warn', '25/11/2569'],
    ]);
  });

  it('Status:active in the block position warns the whole line', () => {
    expect(kinds(body('<div>Status:active</div><div>text</div>'))).toEqual([['prop-warn', 'Status:active']]);
    expect(kinds(body('<div>Owner: Kai</div><div>Status:active</div>'))).toEqual([
      ['prop-key', 'Owner'],
      ['prop-value', 'Kai'],
      ['prop-warn', 'Status:active'],
    ]);
  });

  it('Ratio: 1/2 is a value, not a misspelt date', () => {
    expect(kinds(body('<div>Ratio: 1/2</div>'))).toEqual([
      ['prop-key', 'Ratio'],
      ['prop-value', '1/2'],
    ]);
  });

  it('Meeting at 10:30 is prose, not a property missing its space', () => {
    expect(kinds(body('<div>Meeting at 10:30</div>'))).toEqual([]);
  });

  it('a near miss below the block position never warns', () => {
    expect(kinds(body('<div>Status: a</div><div><br></div><div>Due:25/11</div>'))).toEqual([
      ['prop-key', 'Status'],
      ['prop-value', 'a'],
    ]);
    expect(kinds(body('<div>text</div><div>Status:active</div>'))).toEqual([]);
  });

  it('a URL on the first line is not a near miss', () => {
    expect(kinds(body('<div>https://example.com/a</div>'))).toEqual([]);
  });
});

describe('propertyRanges — due dates on task rows', () => {
  it('yesterday is overdue, with the hint เลย 1 วัน', () => {
    expect(painted(body(task('ship 📅 2026-11-18')))).toEqual([
      { kind: 'due-overdue', text: '2026-11-18', line: 0, hint: 'เลย 1 วัน' },
    ]);
  });

  it('today, tomorrow and later', () => {
    const root = body(task('a 📅 2026-11-19') + task('b due: 2026-11-20') + task('c 📅 2026-11-26'));
    expect(painted(root)).toEqual([
      { kind: 'due-today', text: '2026-11-19', line: 0, hint: 'วันนี้' },
      { kind: 'due-future', text: '2026-11-20', line: 1, hint: 'พรุ่งนี้' },
      { kind: 'due-future', text: '2026-11-26', line: 2, hint: 'อีก 7 วัน' },
    ]);
  });

  it('the last marker wins, and a later plain date is not it', () => {
    const root = body(task('a 📅 2026-11-25 then due: 2026-11-10 (was 2026-11-10)'));
    const items = propertyRanges(root, TODAY);
    expect(items).toHaveLength(1);
    expect(items[0].kind).toBe('due-overdue');
    // The one right after `due:`, not the one in the parentheses.
    const textNode = items[0].range.startContainer as Text;
    expect(textNode.data.slice(0, items[0].range.startOffset)).toMatch(/due: $/);
  });

  it('a date split across inline elements still paints whole', () => {
    expect(kinds(body('<div><input type="checkbox">&nbsp;ship 📅 <b>2026-11</b>-30</div>'))).toEqual([
      ['due-future', '2026-11-30'],
    ]);
  });

  it('a done task is never overdue', () => {
    expect(kinds(body(task('ship 📅 2026-11-01', true)))).toEqual([['due-future', '2026-11-01']]);
  });

  it('a row without a due date, or with an invalid one, paints nothing', () => {
    expect(kinds(body(task('ship it') + task('ship 📅 2026-02-30')))).toEqual([]);
  });

  it('a date outside a task row is plain text', () => {
    expect(kinds(body('<div>text</div><div>ship 📅 2026-11-18</div>'))).toEqual([]);
  });

  it('block and task rows together, in document order', () => {
    const root = body('<div>Status: a</div><div><br></div>' + task('ship 📅 2026-11-19'));
    expect(painted(root).map((p) => [p.kind, p.line])).toEqual([
      ['prop-key', 0],
      ['prop-value', 0],
      ['due-today', 2],
    ]);
  });
});

describe('relativeDay', () => {
  it('names the day relative to today', () => {
    expect(relativeDay('2026-11-19', TODAY)).toBe('วันนี้');
    expect(relativeDay('2026-11-20', TODAY)).toBe('พรุ่งนี้');
    expect(relativeDay('2026-11-21', TODAY)).toBe('อีก 2 วัน');
    expect(relativeDay('2026-11-18', TODAY)).toBe('เลย 1 วัน');
    expect(relativeDay('2026-11-17', TODAY)).toBe('เลย 2 วัน');
    // Across a month and a year end.
    expect(relativeDay('2027-01-01', new Date(2026, 11, 31, 23, 59))).toBe('พรุ่งนี้');
  });
});

describe('createPropertyPainter', () => {
  const ALL: PaintKind[] = ['prop-key', 'prop-value', 'due-overdue', 'due-today', 'due-future', 'prop-warn'];

  it('one highlight per kind, at the props priority', () => {
    const root = body('<div>Status: a</div><div>Due: 25/11</div><div><br></div>' + task('x 📅 2026-11-18'));
    const items = propertyRanges(root, TODAY);
    createPropertyPainter(() => {}).paint(items);
    for (const k of ALL) {
      const h = highlights.get(highlightName(k));
      expect(h, k).toBeDefined();
      expect(h!.priority).toBe(PROP_HL_PRIORITY);
      expect(h!.ranges).toEqual(items.filter((i) => i.kind === k).map((i) => i.range));
    }
    expect(highlights.get(highlightName('prop-key'))!.ranges.map(String)).toEqual(['Status', 'Due']);
  });

  it('what was painted moves through the transparent gone highlight, never delete', () => {
    let later: (() => void) | null = null;
    const p = createPropertyPainter((cb) => (later = cb));
    const root = body('<div>Status: a</div>');
    const first = propertyRanges(root, TODAY);
    p.paint(first);
    p.clear();
    expect(highlights.get(PROP_HL_GONE)!.ranges).toEqual(first.map((i) => i.range));
    for (const k of ALL) expect(highlights.get(highlightName(k))!.ranges).toEqual([]);
    later!();
    expect(highlights.get(PROP_HL_GONE)!.ranges).toEqual([]);
    expect(deleted).toEqual([]);
  });

  it('gone is registered before the kinds, so a range still painted stays painted', () => {
    createPropertyPainter(() => {}).paint([]);
    expect([...highlights.keys()][0]).toBe(PROP_HL_GONE);
  });

  it('a stale after-paint callback does not empty a newer gone', () => {
    const cbs: (() => void)[] = [];
    const p = createPropertyPainter((cb) => cbs.push(cb));
    const root = body('<div>Status: a</div>');
    p.paint(propertyRanges(root, TODAY));
    p.paint(propertyRanges(root, TODAY));
    const gone = highlights.get(PROP_HL_GONE)!.ranges.length;
    cbs[0]();
    expect(highlights.get(PROP_HL_GONE)!.ranges.length).toBe(gone);
    cbs[1]();
    expect(highlights.get(PROP_HL_GONE)!.ranges).toEqual([]);
  });

  it('without CSS.highlights it does nothing', () => {
    vi.unstubAllGlobals();
    expect(() => createPropertyPainter(() => {}).paint(propertyRanges(body('<div>A: b</div>'), TODAY))).not.toThrow();
  });
});

describe('propertyHintAt', () => {
  it('returns the hint of the due range under the point', () => {
    const root = body(task('ship 📅 2026-11-18'));
    const items = propertyRanges(root, TODAY);
    const r = { left: 10, right: 90, top: 0, bottom: 20 } as DOMRect;
    items[0].range.getClientRects = () => [r] as unknown as DOMRectList;
    expect(propertyHintAt(50, 10, items)).toBe('เลย 1 วัน');
    expect(propertyHintAt(95, 10, items)).toBe('');
  });
});

describe('decoration only', () => {
  it('painting_never_mutates_the_body', () => {
    const html =
      '<div>Status:&nbsp;active</div><div>Due: 25/11</div><div>Owner:x</div><div><br></div>' +
      task('ship 📅 2026-11-18') +
      task('done 📅 2026-11-19', true) +
      '<ul><li>item due: 2026-11-30</li></ul>';
    const root = body(html);
    const before = root.innerHTML;
    const p = createPropertyPainter(() => {});
    p.paint(propertyRanges(root, TODAY));
    p.paint(propertyRanges(root, TODAY));
    p.clear();
    propertyHintAt(0, 0, propertyRanges(root, TODAY));
    expect(root.innerHTML).toBe(before);
    expect(root.attributes.length).toBe(0);
  });
});
