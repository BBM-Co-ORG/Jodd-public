// @vitest-environment jsdom
//
// Live 2026-11-12: a note pasted from Gmail on the web carried
// `color: rgb(34, 34, 34)` beside the white background NoteEditor already
// clears — dark-grey text on the dark editor, lines 1–12 unreadable. Near-black
// text now takes the theme's colour; every other colour is content and stays.
//
// The rule's own selectors, read from NoteEditor.svelte and run against real
// style strings with jsdom's matcher: a substring selector is easy to get
// subtly wrong (`background-color: rgb(34, 34, 34)` contains
// `color: rgb(34, 34, 34)`).
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const css = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
const start = css.indexOf(".editor-body :global([style^='color: rgb(0, 0, 0)'");
const rule = css.slice(start, css.indexOf('}', start) + 1);
const selectors = [...rule.slice(0, rule.indexOf('{')).matchAll(/:global\((\[style[^\]]+\])\)/g)].map((m) => m[1]);

function hit(style: string): boolean {
  const el = document.createElement('div');
  el.setAttribute('style', style);
  return selectors.some((s) => el.matches(s));
}

describe('near-black pasted text', () => {
  it('is one rule that hands the colour back to the theme', () => {
    expect(start).toBeGreaterThan(0);
    expect(selectors.length).toBe(10);
    expect(rule).toMatch(/\{\s*color: inherit !important;\s*\}/);
  });

  it('matches the note that was unreadable, verbatim, and the other near-blacks', () => {
    expect(hit('color: rgb(34, 34, 34); font-family: Arial, Helvetica, sans-serif; font-size: small; background-color: rgb(255, 255, 255); scroll-behavior: auto !important;')).toBe(true);
    expect(hit('direction: ltr; color: rgb(34, 34, 34); font-family: Arial, Helvetica, sans-serif;')).toBe(true);
    expect(hit('caret-color: rgb(0, 0, 0); color: rgb(0, 0, 0);')).toBe(true);
    expect(hit('color: rgba(0, 0, 0, 0.85)')).toBe(true);
    expect(hit('color: rgb(32, 33, 36)')).toBe(true);
    expect(hit('COLOR: RGB(51, 51, 51)')).toBe(true);
  });

  it('leaves every other colour, and other properties with a near-black value', () => {
    for (const s of [
      'color: rgb(255, 0, 0)',
      'color: rgb(17, 85, 204)',
      'color: rgb(0, 0, 0.5)',
      'background-color: rgb(34, 34, 34)',
      'border-color: rgb(0, 0, 0)',
      'caret-color: rgb(0, 0, 0)',
      'font-family: Arial; text-decoration-color: rgb(51, 51, 51)',
      'font-size: small; background-color: rgb(34, 34, 34); color: rgb(200, 0, 0)',
    ]) expect(hit(s), s).toBe(false);
  });
});
