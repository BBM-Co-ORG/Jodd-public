// One place that turns a note's date string into display text, so the editor,
// the note list and the trash all agree, and so the calendar (Gregorian vs
// Buddhist Era) is a setting rather than a guess from the timezone.
import { writable, get } from 'svelte/store';

export type CalendarPref = 'auto' | 'gregory' | 'buddhist';

const STORAGE_KEY = 'jodd.calendar';

function isCalendarPref(v: unknown): v is CalendarPref {
  return v === 'auto' || v === 'gregory' || v === 'buddhist';
}

function readStored(): CalendarPref | null {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return isCalendarPref(v) ? v : null;
  } catch {
    return null;
  }
}

export function getCalendarPref(): CalendarPref {
  return readStored() ?? 'auto';
}

/** Source of truth for rendering: components depend on it (`$calendarPref`)
 *  so open views re-render when the preference changes. */
export const calendarPref = writable<CalendarPref>(getCalendarPref());

export function setCalendarPref(p: CalendarPref): void {
  try {
    localStorage.setItem(STORAGE_KEY, p);
  } catch {
    // Storage blocked (private window, cleared site data): the store still
    // carries the choice for this session.
  }
  calendarPref.set(p);
}

/** The Intl `calendar` option for a preference; `undefined` lets the locale decide. */
export function resolveCalendar(pref: CalendarPref): string | undefined {
  return pref === 'auto' ? undefined : pref;
}

/**
 * Upgrade continuity. Before this setting existed, the editor forced the
 * Buddhist Era whenever the system timezone was Thailand's. A user in that
 * position with no stored choice keeps seeing BE, even if their locale would
 * format Gregorian. Anyone else stays on `auto`. Never overrides a stored choice.
 */
export function initCalendarPrefOnce(
  tz: string = Intl.DateTimeFormat().resolvedOptions().timeZone,
  resolvedCalendar: string = Intl.DateTimeFormat().resolvedOptions().calendar,
): void {
  if (readStored() === null && tz === 'Asia/Bangkok' && resolvedCalendar === 'gregory') {
    setCalendarPref('buddhist');
    return;
  }
  calendarPref.set(getCalendarPref());
}

function parse(s: string): Date | null {
  const d = new Date(s);
  return isNaN(d.getTime()) ? null : d;
}

/**
 * Full date and time, for a tooltip or an aria-label: the locale's medium
 * date (with its era under the Buddhist calendar) and short time. The meta
 * line itself shows formatNoteMetaDate.
 */
export function formatNoteDateTime(s: string, _now?: Date): string {
  if (!s) return '';
  const d = parse(s);
  if (!d) return s;
  const calendar = resolveCalendar(get(calendarPref));
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
    calendar,
  }).format(d);
}

/** Whole local calendar days from `d` to `now` (0 = today). Local, not UTC
 *  ms / 86_400_000, which would mis-bucket near-midnight events. */
function daysAgo(d: Date, now: Date): number {
  const dStart = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const nowStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  return Math.round((nowStart - dStart) / 86400000);
}

function time24(d: Date, calendar: string | undefined, locale?: string): string {
  return d.toLocaleTimeString(locale ?? [], { hour: '2-digit', minute: '2-digit', hour12: false, calendar });
}

/**
 * One short-month style everywhere: en-GB's ICU writes September "Sept" and
 * every other month in three letters, so the list showed "1 Sept" beside
 * "21 Oct" (live pass 2026-10-21).
 */
function shortMonth(value: string): string {
  return value === 'Sept' ? 'Sep' : value;
}

/**
 * Day, short month and year in the locale's order, separated by spaces —
 * no comma and no era ("21 Oct 2569", not "21 Oct 2569 BE"). The calendar
 * preference still decides the year.
 */
function dayMonthYear(d: Date, calendar: string | undefined, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { day: 'numeric', month: 'short', year: 'numeric', calendar })
    .formatToParts(d)
    .filter((p) => p.type === 'day' || p.type === 'month' || p.type === 'year')
    .map((p) => (p.type === 'month' ? shortMonth(p.value) : p.value))
    .join(' ');
}

/**
 * The editor's metadata line: the list's relative scheme, with the time —
 * "Today 00:58", "Yesterday 14:20", "Friday 09:05" within the week, else
 * "21 Oct 2569 · 00:58". Live pass 2026-10-21: the medium date style read
 * "21 Oct 2569 BE, 00:58" under the Buddhist calendar. The full timestamp
 * stays in the line's tooltip (formatNoteDateTime). `locale` is for tests;
 * the app passes none and gets the system's.
 */
export function formatNoteMetaDate(s: string, now: Date = new Date(), locale?: string): string {
  if (!s) return '';
  const d = parse(s);
  if (!d) return s;
  const calendar = resolveCalendar(get(calendarPref));
  const t = time24(d, calendar, locale);
  const days = daysAgo(d, now);
  if (days === 0) return `Today ${t}`;
  if (days === 1) return `Yesterday ${t}`;
  if (days > 1 && days < 7) return `${d.toLocaleDateString(locale ?? [], { weekday: 'long', calendar })} ${t}`;
  return `${dayMonthYear(d, calendar, locale)} · ${t}`;
}

/** Compact date for list rows: time today, 'Yesterday', weekday this week, else month + day. */
export function formatListDate(s: string, now: Date = new Date(), locale?: string): string {
  if (!s) return '';
  const d = parse(s);
  if (!d) return s;
  const calendar = resolveCalendar(get(calendarPref));
  const days = daysAgo(d, now);
  if (days === 0) return time24(d, calendar, locale);
  if (days === 1) return 'Yesterday';
  if (days < 7) return d.toLocaleDateString(locale ?? [], { weekday: 'long', calendar });
  return new Intl.DateTimeFormat(locale, { month: 'short', day: 'numeric', calendar })
    .formatToParts(d)
    .map((p) => (p.type === 'month' ? shortMonth(p.value) : p.value))
    .join('');
}
