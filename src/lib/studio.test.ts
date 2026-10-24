import { describe, it, expect } from 'vitest';
import { OUTPUTS, progressLine, estimateLine, type StudioPlan } from './studio';

describe('progressLine', () => {
  it('names each stage in the copy the dialog shows', () => {
    expect(progressLine({ stage: 'reading', total: 12 })).toBe('Reading 12 notes…');
    expect(progressLine({ stage: 'summarizing', index: 2, total: 5 })).toBe('Summarising 2 of 5…');
    expect(progressLine({ stage: 'writing' })).toBe('Writing…');
    expect(progressLine({ stage: 'saving' })).toBe('Saving…');
    expect(progressLine({ stage: 'done' })).toBe('Done');
  });
});

describe('OUTPUTS', () => {
  it('offers the four outputs in order, Briefing first', () => {
    expect(OUTPUTS.map((o) => o.value)).toEqual(['briefing', 'faq', 'timeline', 'study_guide']);
    expect(OUTPUTS.map((o) => o.label)).toEqual(['Briefing', 'FAQ', 'Timeline', 'Study guide']);
    expect(OUTPUTS[0].description).toBe('Summary, key points, disagreements and open questions');
    expect(OUTPUTS[1].description).toBe('Questions a reader would ask, answered from the notes');
    expect(OUTPUTS[2].description).toBe('Dated events in order; undated ones listed apart');
    expect(OUTPUTS[3].description).toBe('Key concepts, glossary, and questions with answers');
  });
});

describe('estimateLine', () => {
  const plan = (calls: number, refusal: string | null, notes = 1): StudioPlan => ({
    notes: Array.from({ length: notes }, (_, i) => ({
      uuid: `u${i}`, title: `t${i}`, label: 'Notes/R', date_ms: 0, chars: 10, is_studio: false,
    })),
    not_shown: 0,
    selected: [],
    estimate: { calls, bytes: 100, map_reduce: calls > 1, refusal },
  });

  it('counts calls, singular and plural', () => {
    expect(estimateLine(plan(1, null))).toBe('About 1 AI call.');
    expect(estimateLine(plan(4, null))).toBe('About 4 AI calls.');
  });

  it('shows the refusal in place of the count', () => {
    expect(estimateLine(plan(0, 'Too much text.'))).toBe('Too much text.');
  });

  it('says so when no note can be read', () => {
    expect(estimateLine(plan(0, 'Tick at least one note.', 0))).toBe('No notes here that AI can read.');
  });
});
