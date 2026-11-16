// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';
import {
  calendarPref,
  getCalendarPref,
  setCalendarPref,
  resolveCalendar,
  initCalendarPrefOnce,
  formatNoteDateTime,
  formatNoteMetaDate,
  formatListDate,
} from './dates';

// 12:26 UTC is mid-day in every zone from -11 to +11, so the calendar date
// (and so the year) never depends on the machine's timezone.
const STAMP = '2026-10-19T12:26:00Z';

beforeEach(() => {
  localStorage.clear();
  calendarPref.set('auto');
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('formatNoteDateTime', () => {
  it('empty → empty, garbage → raw', () => {
    expect(formatNoteDateTime('')).toBe('');
    expect(formatNoteDateTime('nope')).toBe('nope');
  });

  it('buddhist pref yields a BE year', () => {
    setCalendarPref('buddhist');
    expect(formatNoteDateTime(STAMP)).toMatch(/2569/);
  });

  it('gregory pref yields 2026', () => {
    setCalendarPref('gregory');
    const out = formatNoteDateTime(STAMP);
    expect(out).toMatch(/2026/);
    expect(out).not.toMatch(/2569/);
  });

  it('includes a time component', () => {
    setCalendarPref('gregory');
    expect(formatNoteDateTime(STAMP)).toMatch(/\d{1,2}:\d{2}/);
  });
});

describe('calendar preference', () => {
  it('defaults to auto', () => {
    expect(getCalendarPref()).toBe('auto');
  });

  it('setCalendarPref persists to localStorage and updates the store', () => {
    setCalendarPref('buddhist');
    expect(localStorage.getItem('jodd.calendar')).toBe('buddhist');
    expect(getCalendarPref()).toBe('buddhist');
    expect(get(calendarPref)).toBe('buddhist');
  });

  it('notifies store subscribers so open views re-render', () => {
    const seen: string[] = [];
    const unsub = calendarPref.subscribe((v) => seen.push(v));
    setCalendarPref('gregory');
    setCalendarPref('buddhist');
    unsub();
    expect(seen).toEqual(['auto', 'gregory', 'buddhist']);
  });

  it('ignores an unknown stored value', () => {
    localStorage.setItem('jodd.calendar', 'lunar');
    expect(getCalendarPref()).toBe('auto');
  });

  it('throwing localStorage → auto', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('x');
    });
    expect(getCalendarPref()).toBe('auto');
  });

  it('throwing setItem still updates the store and does not throw', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('x');
    });
    expect(() => setCalendarPref('buddhist')).not.toThrow();
    expect(get(calendarPref)).toBe('buddhist');
  });

  it('resolveCalendar maps auto to undefined', () => {
    expect(resolveCalendar('auto')).toBeUndefined();
    expect(resolveCalendar('gregory')).toBe('gregory');
    expect(resolveCalendar('buddhist')).toBe('buddhist');
  });
});

describe('initCalendarPrefOnce', () => {
  it('upgrade continuity: Bangkok + gregorian + no stored pref → buddhist', () => {
    localStorage.removeItem('jodd.calendar');
    initCalendarPrefOnce('Asia/Bangkok', 'gregory');
    expect(getCalendarPref()).toBe('buddhist');
    expect(get(calendarPref)).toBe('buddhist');
  });

  it('does not override a stored pref', () => {
    setCalendarPref('gregory');
    initCalendarPrefOnce('Asia/Bangkok', 'gregory');
    expect(getCalendarPref()).toBe('gregory');
    expect(get(calendarPref)).toBe('gregory');
  });

  it('other zones stay auto', () => {
    localStorage.removeItem('jodd.calendar');
    initCalendarPrefOnce('Europe/London', 'gregory');
    expect(getCalendarPref()).toBe('auto');
    expect(localStorage.getItem('jodd.calendar')).toBeNull();
  });

  it('Bangkok with a locale that is already buddhist stays auto', () => {
    initCalendarPrefOnce('Asia/Bangkok', 'buddhist');
    expect(getCalendarPref()).toBe('auto');
  });

  it('does not throw when localStorage throws', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('x');
    });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('x');
    });
    expect(() => initCalendarPrefOnce('Asia/Bangkok', 'gregory')).not.toThrow();
  });
});

describe('formatListDate', () => {
  const now = new Date('2026-10-20T10:00:00');

  it('empty → empty, garbage → raw', () => {
    expect(formatListDate('', now)).toBe('');
    expect(formatListDate('nope', now)).toBe('nope');
  });

  it('today shows a 24h time', () => {
    expect(formatListDate('2026-10-20T09:05:00', now)).toMatch(/^09:05$/);
    expect(formatListDate('2026-10-20T21:05:00', now)).toMatch(/^21:05$/);
  });

  it('yesterday', () => {
    expect(formatListDate('2026-10-19T09:00:00', now)).toBe('Yesterday');
  });

  it('within a week shows the weekday', () => {
    const d = new Date('2026-10-17T09:00:00');
    expect(formatListDate('2026-10-17T09:00:00', now)).toBe(
      d.toLocaleDateString([], { weekday: 'long' }),
    );
  });

  it('older shows short month and day, in the chosen calendar', () => {
    setCalendarPref('gregory');
    const d = new Date('2026-09-01T09:00:00');
    expect(formatListDate('2026-09-01T09:00:00', now)).toBe(
      d.toLocaleDateString([], { month: 'short', day: 'numeric', calendar: 'gregory' }),
    );
  });

  // The Buddhist calendar differs from the Gregorian only in its year, and
  // the list shows no year: a weekday and a month/day read the same in both.
  // A Buddhist year leaking into a list date ("Sep 1, 2569 BE") would differ.
  it('weekday and month/day read the same under the Buddhist calendar as the Gregorian', () => {
    setCalendarPref('gregory');
    const weekday = formatListDate('2026-10-17T09:00:00', now);
    const older = formatListDate('2026-09-01T09:00:00', now);
    setCalendarPref('buddhist');
    expect(formatListDate('2026-10-17T09:00:00', now)).toBe(weekday);
    expect(formatListDate('2026-09-01T09:00:00', now)).toBe(older);
    expect(older).toBe(new Date('2026-09-01T09:00:00').toLocaleDateString([], { month: 'short', day: 'numeric' }));
  });
});

// Live pass 2026-10-21 (S23 FE): the editor's meta line read
// "21 Oct 2569 BE, 00:58" — dateStyle 'medium' in an English locale with the
// Buddhist calendar appends the era. The user chose the list's relative
// scheme, with no "BE" and no comma, keeping the calendar preference.
describe('formatNoteMetaDate', () => {
  const now = new Date('2026-10-20T10:00:00');

  it('empty → empty, garbage → raw', () => {
    expect(formatNoteMetaDate('', now)).toBe('');
    expect(formatNoteMetaDate('nope', now)).toBe('nope');
  });

  it('today and yesterday carry a 24h time', () => {
    expect(formatNoteMetaDate('2026-10-20T00:58:00', now)).toBe('Today 00:58');
    expect(formatNoteMetaDate('2026-10-19T14:20:00', now)).toBe('Yesterday 14:20');
  });

  it('within a week: the weekday and the time', () => {
    const weekday = new Date('2026-10-17T09:00:00').toLocaleDateString([], { weekday: 'long' });
    expect(formatNoteMetaDate('2026-10-17T09:05:00', now)).toBe(`${weekday} 09:05`);
  });

  it('older: day, month and year in the chosen calendar, then · and the time — no era, no comma', () => {
    setCalendarPref('buddhist');
    const be = formatNoteMetaDate('2026-09-01T07:30:00', now, 'en-GB');
    expect(be).toBe('1 Sep 2569 · 07:30');
    setCalendarPref('gregory');
    expect(formatNoteMetaDate('2026-09-01T07:30:00', now, 'en-GB')).toBe('1 Sep 2026 · 07:30');
    // Locale order is kept; only the comma and the era go.
    setCalendarPref('buddhist');
    expect(formatNoteMetaDate('2026-09-01T07:30:00', now, 'en-US')).toBe('Sep 1 2569 · 07:30');
  });

  it('the full timestamp is still available for the tooltip', () => {
    setCalendarPref('buddhist');
    expect(formatNoteDateTime('2026-09-01T07:30:00')).toMatch(/2569/);
  });
});

describe('short month style is one style everywhere', () => {
  // en-GB's ICU abbreviates September "Sept" but every other month to three
  // letters, so the list showed "1 Sept" next to "21 Oct".
  it('the list writes Sep, not Sept', () => {
    const now = new Date('2026-10-20T10:00:00');
    expect(formatListDate('2026-09-01T09:00:00', now, 'en-GB')).toBe('1 Sep');
    expect(formatListDate('2026-08-01T09:00:00', now, 'en-GB')).toBe('1 Aug');
  });
});
