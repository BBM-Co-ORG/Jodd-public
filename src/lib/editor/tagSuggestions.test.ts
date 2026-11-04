import { describe, it, expect } from 'vitest';
import { rankTagSuggestions } from './tagSuggestions';

const TAGS = ['ai-agentic-workflows', 'ai-tools', 'live-commerce', 'thai-local', 'lessons'];

describe('rankTagSuggestions', () => {
  // Live pass 2026-10-21 (S23 FE): typing "l" listed #ai-agentic-workflows
  // and #ai-tools first, ahead of #live-commerce.
  it('prefix matches come first, then word starts, then any other substring', () => {
    expect(rankTagSuggestions(TAGS, 'l')).toEqual([
      'live-commerce',
      'lessons',
      'thai-local',
      'ai-agentic-workflows',
      'ai-tools',
    ]);
  });
  it('keeps the given order within a tier', () => {
    expect(rankTagSuggestions(['b-x', 'a-x', 'x-1', 'x-2'], 'x')).toEqual(['x-1', 'x-2', 'b-x', 'a-x']);
  });
  it('an empty query lists everything, in order; the limit applies after ranking', () => {
    expect(rankTagSuggestions(TAGS, '')).toEqual(TAGS);
    const many = [...Array.from({ length: 10 }, (_, i) => `zz-l${i}`), 'live'];
    expect(rankTagSuggestions(many, 'l', 8)[0]).toBe('live');
    expect(rankTagSuggestions(many, 'l', 8)).toHaveLength(8);
  });
  it('treats nested-tag segments as word starts', () => {
    expect(rankTagSuggestions(['work/plan', 'explain', 'plans'], 'pla')).toEqual(['plans', 'work/plan', 'explain']);
  });
});
