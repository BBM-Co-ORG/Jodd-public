import { describe, expect, it } from 'vitest';
import { suggestFix } from './fixIt';

const today = new Date(2026, 10, 19);

describe('suggestFix', () => {
  it('adds the missing space', () => {
    expect(suggestFix('Status:active', today)).toBe('Status: active');
  });
  it('completes DD/MM with this year', () => {
    expect(suggestFix('Due: 25/11', today)).toBe('Due: 2026-11-25');
  });
  it('converts a Buddhist year', () => {
    expect(suggestFix('Due: 25/11/2569', today)).toBe('Due: 2026-11-25');
  });
  it('converts DD/MM/YYYY', () => {
    expect(suggestFix('Due: 25/11/2026', today)).toBe('Due: 2026-11-25');
  });
  it('leaves a valid line alone', () => {
    expect(suggestFix('Status: active', today)).toBeNull();
  });
  it('leaves prose alone', () => {
    expect(suggestFix('met the client', today)).toBeNull();
  });
  it('does not mangle URLs', () => {
    expect(suggestFix('https://example.com', today)).toBeNull();
  });
  it('rejects impossible dates', () => {
    expect(suggestFix('Due: 31/02', today)).toBeNull();
  });
});
