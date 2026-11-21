import { describe, it, expect } from 'vitest';
import {
  propertyTriggerAt,
  insertAddPropertyLine,
  dateTriggerStart,
  rankKeys,
  rankValues,
  propertyEnterPicks,
  fixItAt,
  fixItEditAt,
  dueMarkerEdit,
  isoDay,
  dateChoices,
  type PropertyVocabulary,
} from './propertyTrigger';

const TODAY = new Date(2026, 10, 19); // 2026-11-19, local

describe('propertyTriggerAt', () => {
  it('offers keys while a key is typed on a block line', () => {
    expect(propertyTriggerAt(0, 'Sta')).toEqual({ kind: 'key', prefix: 'Sta' });
    expect(propertyTriggerAt(2, 'Due d')).toEqual({ kind: 'key', prefix: 'Due d' });
    expect(propertyTriggerAt(0, 'สถา')).toEqual({ kind: 'key', prefix: 'สถา' });
  });

  it('offers values after `Key: `', () => {
    expect(propertyTriggerAt(0, 'Status: ac')).toEqual({ kind: 'value', key: 'Status', prefix: 'ac' });
    expect(propertyTriggerAt(1, 'Status: ')).toEqual({ kind: 'value', key: 'Status', prefix: '' });
    // Apple's no-break space reads as a space (Review Focus 1).
    expect(propertyTriggerAt(0, 'Status: ac')).toEqual({ kind: 'value', key: 'Status', prefix: 'ac' });
    expect(propertyTriggerAt(0, 'สถานะ: ร')).toEqual({ kind: 'value', key: 'สถานะ', prefix: 'ร' });
  });

  it('offers nothing off the block', () => {
    expect(propertyTriggerAt(null, 'Sta')).toBeNull();
    expect(propertyTriggerAt(null, 'Status: ac')).toBeNull();
  });

  it('offers nothing for what cannot become a property line', () => {
    expect(propertyTriggerAt(0, '')).toBeNull(); // a blank line: no list until a key is typed
    expect(propertyTriggerAt(0, '1st')).toBeNull(); // a key starts with a letter
    expect(propertyTriggerAt(0, 'Status:ac')).toBeNull(); // no space: fix-it's job
    expect(propertyTriggerAt(0, 'a'.repeat(33))).toBeNull(); // longer than a key can be
    expect(propertyTriggerAt(0, 'Note, then')).toBeNull();
    expect(propertyTriggerAt(0, 'https://x.com')).toBeNull();
  });

  it('opens the date picker after Due: / 📅 / due:, on any line', () => {
    expect(propertyTriggerAt(null, 'ship 📅 ')).toEqual({ kind: 'date' });
    expect(propertyTriggerAt(null, 'ship \u{1F4C5}️ ')).toEqual({ kind: 'date' });
    expect(propertyTriggerAt(null, 'ship due: ')).toEqual({ kind: 'date' });
    expect(propertyTriggerAt(null, 'ship DUE: ')).toEqual({ kind: 'date' });
    expect(propertyTriggerAt(0, 'Due: ')).toEqual({ kind: 'date' });
    expect(propertyTriggerAt(0, 'Due: 2026-1')).toEqual({ kind: 'date' });
  });

  it('closes the date picker once a whole date is there, or the marker is part of a word', () => {
    expect(propertyTriggerAt(null, 'ship 📅 2026-11-25')).toBeNull();
    expect(propertyTriggerAt(0, 'Due: 2026-11-25')).toEqual({ kind: 'value', key: 'Due', prefix: '2026-11-25' });
    expect(propertyTriggerAt(null, 'overdue: ')).toBeNull();
    expect(propertyTriggerAt(null, 'ship 📅')).toBeNull(); // the space comes first
  });
});

describe('dateTriggerStart', () => {
  it('is where the date goes: after the marker and its space, over a partial date', () => {
    expect(dateTriggerStart('ship 📅 ')).toBe('ship 📅 '.length);
    expect(dateTriggerStart('Due: 2026-1')).toBe('Due: '.length);
    expect(dateTriggerStart('ship')).toBeNull();
  });
});

describe('insertAddPropertyLine', () => {
  it('puts the new line directly under the title (body line 0) when there is no block', () => {
    expect(insertAddPropertyLine([])).toEqual({ lineIndex: 0 });
    expect(insertAddPropertyLine(['Some text', 'Status: active'])).toEqual({ lineIndex: 0 });
  });

  it('puts it at the end of the block, keeping the block contiguous', () => {
    expect(insertAddPropertyLine(['Status: active', 'Owner: Kai', '', 'body'])).toEqual({ lineIndex: 2 });
    expect(insertAddPropertyLine(['Status: active', 'Owner: Kai'])).toEqual({ lineIndex: 2 });
    expect(insertAddPropertyLine(['Status: active', 'Status:draft'])).toEqual({ lineIndex: 1 });
  });
});

const VOCAB: PropertyVocabulary = [
  ['Status', 9, ['active', 'draft', 'Active-ish']],
  ['Owner', 4, ['Kai']],
  ['Due', 3, ['2026-11-25']],
  ['Project status', 2, []],
  ['สถานะ', 1, ['รอ']],
];

describe('rankKeys', () => {
  it('lists keys starting with the prefix first, most used first, then those containing it', () => {
    expect(rankKeys('sta', VOCAB)).toEqual(['Status', 'Project status']);
    expect(rankKeys('', VOCAB)).toEqual(['Status', 'Owner', 'Due', 'Project status', 'สถานะ']);
    expect(rankKeys('สถ', VOCAB)).toEqual(['สถานะ']);
    expect(rankKeys('zzz', VOCAB)).toEqual([]);
  });
  it('keeps the exact key, so Enter completes it with `: `', () => {
    expect(rankKeys('Owner', VOCAB)).toEqual(['Owner']);
  });
});

describe('rankValues', () => {
  it("lists the key's values, matched case-insensitively against the key", () => {
    expect(rankValues('status', '', VOCAB)).toEqual(['active', 'draft', 'Active-ish']);
    expect(rankValues('Status', 'ac', VOCAB)).toEqual(['active', 'Active-ish']);
    expect(rankValues('Nope', '', VOCAB)).toEqual([]);
  });
  it('drops the value already typed in full, so Enter after it is a new line', () => {
    expect(rankValues('Owner', 'Kai', VOCAB)).toEqual([]);
    expect(rankValues('Status', 'active', VOCAB)).toEqual(['Active-ish']);
  });
});

describe('propertyEnterPicks', () => {
  it('picks a prefix match, like the # list', () => {
    expect(propertyEnterPicks('sta', ['Status'], false)).toBe(true);
    expect(propertyEnterPicks('tus', ['Status'], false)).toBe(false);
  });
  it('never picks on an empty prefix unless the highlight was moved', () => {
    expect(propertyEnterPicks('', ['Status'], false)).toBe(false);
    expect(propertyEnterPicks('', ['Status'], true)).toBe(true);
  });
  it('never picks from an empty list', () => {
    expect(propertyEnterPicks('sta', [], true)).toBe(false);
  });
  it('with no block yet, picks only a row the highlight was moved to (ruling R13)', () => {
    expect(propertyEnterPicks('Note', ['Notes'], false, true)).toBe(false);
    expect(propertyEnterPicks('Project', ['Project'], false, true)).toBe(false);
    expect(propertyEnterPicks('Note', ['Notes'], true, true)).toBe(true);
  });
});

describe('fixItAt — offered exactly where the wavy underline is', () => {
  it('fixes a slashed date under a date key, and a Buddhist year', () => {
    expect(fixItAt(['Due: 25/11'], 0, TODAY)).toBe('Due: 2026-11-25');
    expect(fixItAt(['Status: active', 'Due: 25/11/2569'], 1, TODAY)).toBe('Due: 2026-11-25');
    expect(fixItAt(['Started: 25/11/2569'], 0, TODAY)).toBe('Started: 2026-11-25');
  });

  it('adds the missing space on the line that ends the block', () => {
    expect(fixItAt(['Status:active'], 0, TODAY)).toBe('Status: active');
    expect(fixItAt(['Owner: Kai', 'Status:active'], 1, TODAY)).toBe('Status: active');
    expect(fixItAt(['Due:25/11'], 0, TODAY)).toBe('Due: 2026-11-25');
  });

  it('never offers a date for a ratio, only the space', () => {
    expect(fixItAt(['Ratio: 1/2'], 0, TODAY)).toBeNull();
    expect(fixItAt(['Ratio:1/2'], 0, TODAY)).toBe('Ratio: 1/2');
  });

  it('never offers anything for a time', () => {
    expect(fixItAt(['Meeting at 10:30'], 0, TODAY)).toBeNull();
    expect(fixItAt(['Status: active', 'Meeting at 10:30'], 1, TODAY)).toBeNull();
  });

  it('nothing below the block position, on a valid line, or on a blank one', () => {
    expect(fixItAt(['Owner: Kai', 'text', 'Status:active'], 2, TODAY)).toBeNull();
    expect(fixItAt(['Status: active'], 0, TODAY)).toBeNull();
    expect(fixItAt(['', 'Status:active'], 0, TODAY)).toBeNull();
    expect(fixItAt(['', 'Status:active'], 1, TODAY)).toBeNull();
    expect(fixItAt(['see https://x.com'], 0, TODAY)).toBeNull();
    expect(fixItAt([], 0, TODAY)).toBeNull();
  });
});

describe('dueMarkerEdit', () => {
  it('appends ` 📅 <date>` to a row without one', () => {
    expect(dueMarkerEdit(' ship it', '2026-11-25')).toEqual({ start: 8, end: 8, text: ' 📅 2026-11-25' });
    expect(dueMarkerEdit('ship it ', '2026-11-25')).toEqual({ start: 8, end: 8, text: '📅 2026-11-25' });
    expect(dueMarkerEdit('', '2026-11-25')).toEqual({ start: 0, end: 0, text: '📅 2026-11-25' });
  });

  it('replaces the date that counts instead of adding a second marker', () => {
    const row = ' ship 📅 2026-11-20 then';
    const e = dueMarkerEdit(row, '2026-11-25');
    expect(e).toEqual({ start: row.indexOf('2026'), end: row.indexOf('2026') + 10, text: '2026-11-25' });
    // The last marker wins in the parser, so that is the one replaced.
    const two = 'a due: 2026-01-01 b 📅 2026-02-02';
    expect(dueMarkerEdit(two, '2026-11-25')).toEqual({ start: two.lastIndexOf('2026'), end: two.length, text: '2026-11-25' });
    // A plain copy of the date later in the text is not the marker's.
    const copy = 'x 📅 2026-11-20 (moved from 2026-11-20)';
    expect(dueMarkerEdit(copy, '2026-11-25').start).toBe(copy.indexOf('2026'));
  });

  it('fills a marker typed without a date', () => {
    expect(dueMarkerEdit('ship 📅 ', '2026-11-25')).toEqual({ start: 8, end: 8, text: '2026-11-25' });
    expect(dueMarkerEdit('ship 📅', '2026-11-25')).toEqual({ start: 7, end: 7, text: ' 2026-11-25' });
  });
});

describe('dates', () => {
  it('isoDay is the local calendar day', () => {
    expect(isoDay(TODAY)).toBe('2026-11-19');
    expect(isoDay(new Date(2026, 0, 5, 23, 59))).toBe('2026-01-05');
  });
  it('dateChoices: today, tomorrow, in a week', () => {
    expect(dateChoices(TODAY)).toEqual([
      { label: 'Today', value: '2026-11-19' },
      { label: 'Tomorrow', value: '2026-11-20' },
      { label: 'In a week', value: '2026-11-26' },
    ]);
    expect(dateChoices(new Date(2026, 11, 31))[1].value).toBe('2027-01-01');
  });
});

describe('fixItEditAt — the minimal edit (ruling R12)', () => {
  it('a missing space is one inserted space after the colon', () => {
    expect(fixItEditAt(['Source:Article'], 0, TODAY)).toEqual({
      edits: [{ start: 7, end: 7, text: ' ' }],
      label: 'add a space after “Source:”',
    });
  });
  it('a date fix replaces only the date characters', () => {
    expect(fixItEditAt(['Status: a', 'Due: 25/11/2569'], 1, TODAY)).toEqual({
      edits: [{ start: 5, end: 15, text: '2026-11-25' }],
      label: '25/11/2569 → 2026-11-25',
    });
  });
  it('both at once on the line that ends the block', () => {
    expect(fixItEditAt(['Due:25/11'], 0, TODAY)).toEqual({
      edits: [{ start: 4, end: 9, text: ' 2026-11-25' }],
      label: 'add a space after “Due:”, 25/11 → 2026-11-25',
    });
  });
  it('still nothing for a ratio or a time', () => {
    expect(fixItEditAt(['Ratio: 1/2'], 0, TODAY)).toBeNull();
    expect(fixItEditAt(['Meeting at 10:30'], 0, TODAY)).toBeNull();
    expect(fixItEditAt(['Ratio:1/2'], 0, TODAY)!.edits).toEqual([{ start: 6, end: 6, text: ' ' }]);
  });
});
