// The gear (account settings) and ✕ (remove account) buttons on each row of
// the Accounts panel are hidden with `opacity: 0` and revealed by
// `.account-row:hover`. **A touch screen never hovers**, so on Android both
// were invisible — permanently, and reported as "หายไป" (missing) from a real
// device on 2026-09-09.
//
// Invisible is the lesser half. `opacity: 0` hides paint, not hit-testing, so
// the row still carried a live **Remove account** button the user could not
// see and could hit by accident. Discovering account removal by mis-tapping is
// the failure this guards against.
//
// **Why the assertion is on the stylesheet's shape rather than on a rendered
// button.** Probed before writing this: mounting Sidebar in jsdom puts BOTH
// buttons in the DOM and reports `getComputedStyle(...).opacity === "1"`,
// because Svelte's scoped CSS never reaches the document there — 0 `<style>`
// tags — and `window.matchMedia` is undefined. A computed-style test would
// therefore pass against the broken code and prove nothing. jsdom has no CSS
// engine to ask; the source is the only artifact a unit test can see. The
// behaviour itself was verified on the device.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const source = readFileSync(
  fileURLToPath(new URL('./Sidebar.svelte', import.meta.url)),
  'utf8'
);

/**
 * CSS comments removed first — prose that merely mentions a declaration is not
 * one. This file's own explanatory comment quotes `opacity: 0`, and matching it
 * reported the fixed rule as still broken.
 */
function withoutComments(css: string): string {
  return css.replace(/\/\*[\s\S]*?\*\//g, '');
}

/** The stylesheet with every `@media` block removed, brace-matched. */
function withoutMediaBlocks(css: string): string {
  let out = '';
  let i = 0;
  while (i < css.length) {
    const at = css.indexOf('@media', i);
    if (at === -1) {
      out += css.slice(i);
      break;
    }
    out += css.slice(i, at);
    const open = css.indexOf('{', at);
    let depth = 0;
    let j = open;
    for (; j < css.length; j++) {
      if (css[j] === '{') depth++;
      else if (css[j] === '}' && --depth === 0) break;
    }
    i = j + 1;
  }
  return out;
}

/** Flat `selector { body }` rules whose selector names either control. */
function rulesTargetingTheControls(css: string): { selector: string; body: string }[] {
  const found: { selector: string; body: string }[] = [];
  for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selector = m[1].trim();
    if (/\.account-row-(remove|settings)\b/.test(selector)) {
      found.push({ selector, body: m[2] });
    }
  }
  return found;
}

describe('Accounts panel row controls', () => {
  it('never hides the gear and remove buttons outside a hover-capable media query', () => {
    const unconditional = rulesTargetingTheControls(
      withoutMediaBlocks(withoutComments(source))
    ).filter((r) => /opacity:\s*0(\D|$)/.test(r.body));

    expect(
      unconditional.map((r) => r.selector),
      'a rule hides these controls for every device, so a touch screen can never reveal them'
    ).toEqual([]);
  });

  it('still reveals them on hover for pointer devices', () => {
    // The desktop affordance must survive the fix: the reveal belongs inside a
    // `hover: hover` query, not deleted outright.
    expect(source).toMatch(/@media\s*\(\s*hover:\s*hover\s*\)/);
  });
});
