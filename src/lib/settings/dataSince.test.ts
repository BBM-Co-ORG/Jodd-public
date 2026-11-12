import { describe, it, expect } from 'vitest';
import { dataSinceText } from './dataSince';
import { formatNoteDateTime } from '../format/dates';

const MS = Date.UTC(2026, 10, 10, 13, 45);
const WHEN = formatNoteDateTime(new Date(MS).toISOString());

describe('dataSinceText', () => {
  it('states the start of a fresh folder', () => {
    expect(dataSinceText({ since_ms: MS, found_existing: false })).toBe(`Jodd's data on this device starts ${WHEN}.`);
  });
  it('says "or earlier" when settings were already there', () => {
    expect(dataSinceText({ since_ms: MS, found_existing: true })).toBe(
      `Jodd's data on this device dates from ${WHEN} or earlier.`,
    );
  });
  it('shows nothing without a usable date', () => {
    expect(dataSinceText(null)).toBeNull();
    expect(dataSinceText({ since_ms: 0, found_existing: false })).toBeNull();
    expect(dataSinceText({ since_ms: Number.NaN, found_existing: true })).toBeNull();
  });
});
