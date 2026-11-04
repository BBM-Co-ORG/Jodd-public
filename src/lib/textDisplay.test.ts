import { describe, it, expect } from 'vitest';
import { displayTitle, leadingOrphanMarks, startsWithOrphanCombiningMark } from './textDisplay';

const DOTTED = '◌';
const SARA_UEE = 'ื'; // ื — the exact character from the live report
const MAI_EK = '่'; // ่ — a tone mark

describe('displayTitle', () => {
  it('shows a dotted circle before a lone orphaned Thai vowel (the live bug)', () => {
    expect(displayTitle(SARA_UEE)).toBe(DOTTED + SARA_UEE);
  });

  it('shows a dotted circle before a leading orphaned mark, then the rest', () => {
    expect(displayTitle(SARA_UEE + 'note test')).toBe(DOTTED + SARA_UEE + 'note test');
  });

  it('leaves ordinary Thai untouched — every mark has its consonant base', () => {
    // สวัสดี: the vowels/tone marks follow their consonants, so no dotted circle.
    const hello = 'สวัสดี';
    expect(displayTitle(hello)).toBe(hello);
  });

  it('leaves an ordinary ASCII title untouched', () => {
    expect(displayTitle('note test')).toBe('note test');
    expect(displayTitle('')).toBe('');
  });

  it('stacks multiple leading orphaned marks onto ONE dotted circle', () => {
    // Two orphaned marks in a row get a single base to sit on, like iOS.
    expect(displayTitle(SARA_UEE + MAI_EK + 'x')).toBe(DOTTED + SARA_UEE + MAI_EK + 'x');
  });

  it('does not add a dotted circle for a mark that appears mid-word with a base', () => {
    // A base letter, then a combining mark on it — legitimate, no dotted circle.
    expect(displayTitle('a' + MAI_EK)).toBe('a' + MAI_EK);
  });
});

describe('leadingOrphanMarks / startsWithOrphanCombiningMark', () => {
  it('detects a leading orphaned mark', () => {
    expect(startsWithOrphanCombiningMark(SARA_UEE + 'note')).toBe(true);
    expect(leadingOrphanMarks(SARA_UEE + MAI_EK + 'note')).toBe(SARA_UEE + MAI_EK);
  });

  it('is false for ordinary titles', () => {
    expect(startsWithOrphanCombiningMark('note test')).toBe(false);
    expect(startsWithOrphanCombiningMark('สวัสดี')).toBe(false);
    expect(startsWithOrphanCombiningMark('')).toBe(false);
    expect(leadingOrphanMarks('note')).toBe('');
  });
});
