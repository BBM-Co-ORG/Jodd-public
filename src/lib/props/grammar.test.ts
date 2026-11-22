import { describe, expect, it } from 'vitest';
import blockCases from '../../../tests/fixtures/property-block.json';
import dueCases from '../../../tests/fixtures/due-dates.json';
import { parseDue, parsePropertyBlock } from './grammar';

describe('property block conformance', () => {
  for (const c of blockCases) {
    it(c.name, () => {
      const want = c.props.map((p, pos) => ({
        pos,
        key: p.key,
        keyDisplay: p.key_display,
        value: p.value,
        valueDate: p.value_date,
      }));
      expect(parsePropertyBlock(c.lines)).toEqual(want);
    });
  }
});

describe('due date conformance', () => {
  for (const c of dueCases) {
    it(c.name, () => {
      expect(parseDue(c.text)).toBe(c.due);
    });
  }
});
