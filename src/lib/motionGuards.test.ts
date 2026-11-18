// Motion as a system (2026-11-09 workspace review: "one --ease token, global
// 0.01ms clamp"). Durations and easing are tokens, and reduced motion is
// decided per kind of motion instead of by clamping every element:
//
//   - colour, background and opacity changes stay: they move nothing;
//   - anything that moves takes --dur-move, which reduced motion sets to 0;
//   - a looping spinner stops and shows a still busy state of its own.
//
// The old clamp froze every spinner mid-turn (NoteList and AuthScreen had
// to patch the broken arc it left) and would have cut any future fade to
// nothing for people who only asked for less movement.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { parseTokens } from './color';
import { findAll, sourceFiles } from './designGuards.test';

const CSS = readFileSync('src/styles/tokens.css', 'utf8');
const ROOT = parseTokens(CSS, ':root');

describe('motion tokens', () => {
  it('names every duration and the easing', () => {
    for (const t of ['--dur-instant', '--dur-fast', '--dur-move', '--dur-spin', '--ease-out']) {
      expect(ROOT[t], t).toBeDefined();
    }
    // Exponential ease-out: fast start, long settle.
    expect(ROOT['--ease-out']).toMatch(/^cubic-bezier\(/);
  });

  it('reduced motion stops movement, after the generated theme blocks so it wins over them', () => {
    const end = CSS.indexOf('/* END generated data-theme blocks */');
    const block = CSS.indexOf('@media (prefers-reduced-motion: reduce)');
    expect(block, 'no reduced-motion block in tokens.css').toBeGreaterThan(-1);
    // :root[data-theme='light'] restates --dur-move with more specificity
    // than :root, so a plain `:root` override would lose to it.
    expect(block).toBeGreaterThan(end);
    const body = CSS.slice(block, CSS.indexOf('}', CSS.indexOf('}', block) + 1) + 1);
    expect(body).toMatch(/\[data-theme\]/);
    expect(body).toMatch(/--dur-move:\s*0m?s/);
  });
});

describe('motion in components', () => {
  it('no transition or animation spells its own duration', () => {
    const literal = /(?:transition|animation)(?:-duration)?\s*:[^;{}]*?(?<![\w-])\d*\.?\d+m?s\b[^;{}]*;/g;
    expect(findAll(literal)).toEqual([]);
  });

  it('nothing clamps every element: the old global 0.01ms rule is gone', () => {
    expect(findAll(/0\.01ms/g)).toEqual([]);
  });

  it('every looping animation has a still state under reduced motion', () => {
    const missing = sourceFiles()
      .filter(({ text }) => /animation:[^;]*infinite/.test(text))
      .filter(({ text }) => {
        const m = text.match(/@media \(prefers-reduced-motion: reduce\)\s*\{([\s\S]*?)\n  \}/g) ?? [];
        return !m.some((b) => /animation:\s*none/.test(b));
      })
      .map(({ path }) => path);
    expect(missing).toEqual([]);
  });
});
