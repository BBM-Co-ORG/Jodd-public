import { describe, it, expect } from 'vitest';
import { changelogInline } from './changelogInline';

const t = (text: string, bold = false, code = false) => ({ text, bold, code });

describe('changelogInline', () => {
  it('splits **bold** spans out of plain text', () => {
    expect(changelogInline('**Lead.** Rest.')).toEqual([t('Lead.', true), t(' Rest.')]);
  });
  it('leaves an unmatched ** as text', () => {
    expect(changelogInline('a ** b')).toEqual([t('a ** b')]);
  });
  it('handles several spans and none', () => {
    expect(changelogInline('x **a** y **b**')).toEqual([t('x '), t('a', true), t(' y '), t('b', true)]);
    expect(changelogInline('plain')).toEqual([t('plain')]);
    expect(changelogInline('')).toEqual([]);
  });

  // Galaxy Tab S7 live pass of v0.35.2 (2026-10-21): What's New showed
  // `**` with its backticks.
  describe('`code`', () => {
    it('is a code run without its backticks', () => {
      expect(changelogInline('Type `**` around a word')).toEqual([t('Type '), t('**', false, true), t(' around a word')]);
    });
    it('keeps ** inside it literal: code is read before bold', () => {
      expect(changelogInline('`**a**` and **b**')).toEqual([t('**a**', false, true), t(' and '), t('b', true)]);
    });
    it('inside a bold run is bold code', () => {
      expect(changelogInline('**Use `jodd://capture`.** Then')).toEqual([
        t('Use ', true), t('jodd://capture', true, true), t('.', true), t(' Then'),
      ]);
    });
    it('a lone backtick stays literal', () => {
      expect(changelogInline('it`s fine')).toEqual([t('it`s fine')]);
    });
    it('markup inside code stays text for the caller to escape', () => {
      expect(changelogInline('`<b>&amp;</b>`')).toEqual([t('<b>&amp;</b>', false, true)]);
    });
  });
});
