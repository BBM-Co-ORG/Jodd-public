import { describe, it, expect } from 'vitest';
import { bodyTextLength, shrinkRefusal } from './shrinkGuard';

// An AI-made note: a short visible summary, then the verbatim source in a
// CLOSED <details> (llm/markdown.rs source_block). The live-pass note that
// found this (2026-10-29) was 12,620 visible chars over a 67,118-char source.
const SOURCE = 'source text '.repeat(500); // 6,000 chars, folded away
const aiNote = (summary: string) =>
  `<html><head></head><body><p>${summary}</p><hr>\n<details>\n<summary>Source (verbatim)</summary>\n<pre>${SOURCE}</pre>\n</details>\n</body></html>`;

describe('bodyTextLength', () => {
  it('counts text the same way whatever the markup', () => {
    expect(bodyTextLength('<div>a&nbsp;b</div>')).toBe(3);
    expect(bodyTextLength('<p>a</p>\n\n<p>b</p>')).toBe(3);
    expect(bodyTextLength('')).toBe(0);
  });

  it('counts the text inside a closed <details> — it is part of the note', () => {
    expect(bodyTextLength(aiNote('summary'))).toBeGreaterThan(SOURCE.trim().length);
  });
});

describe('shrinkRefusal', () => {
  it('lets an edit to an AI-made note save: the source block is counted on both sides', () => {
    const stored = aiNote('A short summary of the page.');
    const edited = aiNote('A short summary of the page. smoketest212');
    expect(shrinkRefusal(stored, edited)).toBeNull();
  });

  it('still refuses a save that would drop most of the note', () => {
    const stored = aiNote('A short summary of the page.');
    const msg = shrinkRefusal(stored, '<html><head></head><body><div>x</div></body></html>');
    expect(msg).toMatch(/^Autosave blocked: would shrink note from \d+ to 1 chars\./);
  });

  it('refuses at under a quarter of the stored text, allows at a quarter', () => {
    const stored = `<div>${'a'.repeat(100)}</div>`;
    expect(shrinkRefusal(stored, `<div>${'a'.repeat(24)}</div>`)).not.toBeNull();
    expect(shrinkRefusal(stored, `<div>${'a'.repeat(25)}</div>`)).toBeNull();
  });

  it('does not judge a note too short to tell an edit from a wipe', () => {
    expect(shrinkRefusal('<div>nineteen chars!!</div>', '')).toBeNull();
  });

  it('does not judge without a baseline (a draft the backend never confirmed)', () => {
    expect(shrinkRefusal(undefined, '')).toBeNull();
  });

  it('counts only the body: a stylesheet in the head is not note text', () => {
    const style = `<style>${'p { color: red } '.repeat(50)}</style>`;
    const stored = `<html><head>${style}</head><body><div>${'a'.repeat(40)}</div></body></html>`;
    expect(shrinkRefusal(stored, `<html><head></head><body><div>${'a'.repeat(40)} b</div></body></html>`)).toBeNull();
  });
});
