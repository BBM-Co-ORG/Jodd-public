import { describe, it, expect } from 'vitest';
import { formatBytes, noteTextBytes, sizeLine } from './noteSize';

describe('noteTextBytes', () => {
  it('counts UTF-8 bytes, as the cache stores them: a Thai letter is three', () => {
    expect(noteTextBytes('', 'abc')).toBe(3);
    expect(noteTextBytes('ก', '')).toBe(3);
    expect(noteTextBytes('T', '<div>T</div>')).toBe(1 + 12);
  });

  it('counts what a closed <details> hides: it is stored all the same', () => {
    const body = '<details><summary>S</summary><div>hidden</div></details>';
    expect(noteTextBytes('', body)).toBe(body.length);
  });
});

describe('formatBytes', () => {
  it.each([
    [0, '0 bytes'], [1, '1 byte'], [1023, '1023 bytes'], [1024, '1.0 KB'], [1536, '1.5 KB'],
    [10 * 1024, '10 KB'], [48 * 1024 + 300, '48 KB'], [1024 * 1024, '1.0 MB'], [25 * 1024 * 1024, '25 MB'],
  ])('%i → %s', (n, s) => expect(formatBytes(n)).toBe(s));
});

describe('sizeLine', () => {
  it('the text alone until the attachments are known, or when there are none', () => {
    expect(sizeLine(2048, null)).toBe('2.0 KB on this device');
    expect(sizeLine(2048, { count: 0, bytes: 0 })).toBe('2.0 KB on this device');
  });

  it('a total, then the note and its attachments', () => {
    expect(sizeLine(2048, { count: 1, bytes: 1024 * 1024 })).toBe('1.0 MB on this device (note 2.0 KB · 1 attachment 1.0 MB)');
    expect(sizeLine(1024, { count: 3, bytes: 2048 })).toBe('3.0 KB on this device (note 1.0 KB · 3 attachments 2.0 KB)');
  });
});
