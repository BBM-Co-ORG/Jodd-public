import { describe, it, expect } from 'vitest';
import { OUTPUTS, OUTPUT_GROUPS, progressLine, estimateLine, relatedReasons, type StudioPlan, type RelatedNote } from './studio';

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
  it('offers the eight outputs in order, Briefing first', () => {
    expect(OUTPUTS.map((o) => o.value)).toEqual([
      'briefing', 'faq', 'timeline', 'study_guide',
      'slide_deck', 'course_module', 'podcast_script', 'video_script',
    ]);
    expect(OUTPUTS.map((o) => o.label)).toEqual([
      'Briefing', 'FAQ', 'Timeline', 'Study guide',
      'Slide deck', 'Course module', 'Podcast script', 'Video script',
    ]);
    expect(OUTPUTS[0].description).toBe('Summary, key points, disagreements and open questions');
    expect(OUTPUTS[1].description).toBe('Questions a reader would ask, answered from the notes');
    expect(OUTPUTS[2].description).toBe('Dated events in order; undated ones listed apart');
    expect(OUTPUTS[3].description).toBe('Key concepts, glossary, and questions with answers');
    expect(OUTPUTS[4].description).toBe('A title slide, then a headline, bullets and speaker notes per slide');
    expect(OUTPUTS[5].description).toBe('Objectives, lessons in order, exercises, and a quiz with answers last');
    expect(OUTPUTS[6].description).toBe('Two hosts talk it through, with an intro and an outro');
    expect(OUTPUTS[7].description).toBe('Scenes with what is on screen, the narration and a duration');
  });

  it('splits them into two groups: four to understand, then four to deliver', () => {
    expect(OUTPUT_GROUPS).toEqual([
      { value: 'understand', label: 'Understand the notes' },
      { value: 'deliver', label: 'Draft a deliverable' },
    ]);
    expect(OUTPUTS.map((o) => o.group)).toEqual([
      'understand', 'understand', 'understand', 'understand',
      'deliver', 'deliver', 'deliver', 'deliver',
    ]);
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

describe('relatedReasons', () => {
  const note = (over: Partial<RelatedNote>): RelatedNote => ({
    uuid: 'x1', title: 'Elsewhere', label: 'Notes/Other', date_ms: 0, chars: 10,
    linked_with: 0, tags: [], terms: [], ...over,
  });

  it('counts links, singular and plural', () => {
    expect(relatedReasons(note({ linked_with: 1 }))).toEqual(['Linked with 1 note']);
    expect(relatedReasons(note({ linked_with: 3 }))).toEqual(['Linked with 3 notes']);
  });

  it('shows at most three shared tags as one chip', () => {
    expect(relatedReasons(note({ tags: ['a', 'b', 'c', 'd'] }))).toEqual(['#a #b #c']);
  });

  it('shows at most two matched terms as one chip', () => {
    expect(relatedReasons(note({ terms: ['battery', 'ev', 'cell'] }))).toEqual(['Mentions “battery”, “ev”']);
    expect(relatedReasons(note({ terms: ['battery'] }))).toEqual(['Mentions “battery”']);
  });

  it('orders the chips links, tags, words', () => {
    expect(relatedReasons(note({ linked_with: 2, tags: ['ev'], terms: ['battery'] }))).toEqual([
      'Linked with 2 notes',
      '#ev',
      'Mentions “battery”',
    ]);
  });

  it('has no chip without a signal', () => {
    expect(relatedReasons(note({}))).toEqual([]);
  });
});
