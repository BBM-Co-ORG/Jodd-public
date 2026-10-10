import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { parseTokens, parseColor, contrast, composite, deltaE, type RGB } from '../lib/color';

const CSS = readFileSync('src/styles/tokens.css', 'utf8');
const LIGHT = parseTokens(CSS, ':root');
const DARK_MEDIA = parseTokens(CSS, '@media (prefers-color-scheme: dark)');
const DARK_ATTR = parseTokens(CSS, ":root[data-theme='dark']");
const LIGHT_ATTR = parseTokens(CSS, ":root[data-theme='light']");

// Structural check for the generator, not a value check. parseTokens's
// brace-depth matcher finds a selector's OUTER braces and then regex-scrapes
// `--x: y;` pairs at ANY nesting depth inside them -- so if the generator
// ever emits a nested selector by mistake (e.g. a stray `:root { ... }`
// inside `:root[data-theme='dark'] { ... }`, which desugars under CSS
// Nesting to the descendant selector `:root[data-theme='dark'] :root` and
// can never match, since <html> cannot be its own descendant), parseTokens
// still finds the right VALUES and every toEqual() comparison in this file
// stays green on fully inert CSS. That exact bug shipped in this file's own
// history (round 2 of Task 8's dark-mode work) and was caught only by a
// human reading the emitted CSS, not by any test. This returns the RAW body
// text of a selector so structure -- not values -- can be asserted.
function rawBlockBody(css: string, selector: string): string {
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const idx = clean.indexOf(selector);
  if (idx === -1) throw new Error(`selector not found: ${selector}`);
  const open = clean.indexOf('{', idx + selector.length);
  let depth = 1;
  let i = open + 1;
  for (; i < clean.length && depth > 0; i++) {
    if (clean[i] === '{') depth++;
    else if (clean[i] === '}') depth--;
  }
  return clean.slice(open + 1, i - 1);
}

// Text tokens x the surfaces they can actually land on.
//
// MANUALLY MAINTAINED -- not derived from a grep, on purpose. An automated
// sweep over `color: var(--x)` sites can find the token but not its real
// backdrop: e.g. DupReviewModal's `.keeper-tag { color: var(--success) }`
// sits on `--success-wash` composited over `--surface-editor`, not on any
// bare surface token, and resolving that needs the component's markup
// nesting, not just the CSS text. A weak/partial auto-derivation would be
// worse than this list: it would look authoritative while still missing the
// exact class of bug it exists to catch (see the --success incident below).
//
// So: when adding a new `color: var(--token)` rule to a component, add it
// here too. To audit for drift, run this and manually check every match
// against a row (or a deliberate omission, like --text-disabled) below:
//   grep -rn "color: var(--" src/lib/components src/App.svelte
//
// --success was missing from this list entirely (not measured-and-passed,
// just never audited) until the gap was caught by that grep: #46a35e
// measured 2.65:1 on plain --surface-sidebar and as low as 2.13 on
// sidebar+active -- a real AA failure in both of --success's live uses
// (DupReviewModal .keeper-tag, Sidebar .account-chip-dot). Corrected to
// #0b6823, see the wash-composited chip tests further down for the
// non-PAIRS backdrop that .keeper-tag actually sits on.
//
// --accent is a DELIBERATE omission, same as --text-disabled: tokens.css's
// own comment states the contract ("--accent is for fills, tints and
// borders with NO text on them; --accent-action is for anything carrying or
// being text"), and it was a call-site bug, not a token defect, when
// Sidebar's `.account-row-dot` broke that contract (color: var(--accent),
// 2.77:1 on --surface-sidebar, failing WCAG 1.4.11's 3:1 for a non-text
// indicator). Fixed at the call site (now --accent-action, >=4.5:1) rather
// than by auditing --accent as text here -- the grep above confirms every
// remaining `var(--accent)` site is a border-color/border-top-color, i.e.
// exactly the fill/border role the token is contracted for. If a future
// `color: var(--accent)` site appears, that is the same class of misuse,
// not a reason to add --accent to this list.
const PAIRS: [string, string][] = [
  ['--text', '--surface-editor'], ['--text', '--surface-list'],
  ['--text', '--surface-sidebar'], ['--text', '--surface-panel'],
  ['--text-secondary', '--surface-editor'], ['--text-secondary', '--surface-list'],
  ['--text-secondary', '--surface-sidebar'], ['--text-secondary', '--surface-panel'],
  ['--text-muted', '--surface-editor'], ['--text-muted', '--surface-list'],
  ['--text-muted', '--surface-sidebar'], ['--text-muted', '--surface-panel'],
  ['--accent-action', '--surface-editor'], ['--accent-action', '--surface-list'],
  ['--accent-action', '--surface-sidebar'], ['--accent-action', '--surface-panel'],
  ['--danger', '--surface-sidebar'], ['--danger', '--surface-panel'],
  ['--success', '--surface-sidebar'], ['--success', '--surface-panel'],
  ['--accent-on-tint', '--accent-tint'],
  ['--text-inverse', '--accent-action'],
  ['--text', '--surface-badge'], ['--text-muted', '--surface-badge'],
];

function solve(tokens: Record<string, string>, name: string, fallbackBg: RGB): RGB {
  const raw = tokens[name];
  if (!raw) throw new Error(`token ${name} is not defined`);
  const parsed = parseColor(raw);
  if (!parsed) throw new Error(`token ${name} is not a colour: ${raw}`);
  return parsed.alpha === 1 ? parsed.rgb : composite(parsed.rgb, parsed.alpha, fallbackBg);
}

// State overlays that composite ON TOP of a surface before text lands on it.
// --surface-badge is deliberately EXCLUDED: it is opaque precisely so an
// overlay never composites onto it (that is the whole reason it is opaque).
const OVERLAID = ['--surface-sunken', '--surface-hover', '--surface-active'];

function auditTheme(label: string, tokens: Record<string, string>, pageBg: RGB) {
  describe(`${label} theme contrast`, () => {
    for (const [fg, bg] of PAIRS) {
      it(`${fg} on ${bg} clears 4.5:1`, () => {
        const bgRgb = solve(tokens, bg, pageBg);
        const fgRgb = solve(tokens, fg, bgRgb);
        const ratio = contrast(fgRgb, bgRgb);
        expect(ratio, `${fg} on ${bg} measured ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
      });
    }

    // The failure mode a plain fg-on-bg audit CANNOT see. Measured on the real
    // dark palette: text-muted passed on every plain surface (4.93-6.70) while
    // failing at 4.01 on --surface-panel under --surface-active. Alpha stacks;
    // a hovered or selected row is a different backdrop from the row itself.
    for (const [fg, bg] of PAIRS) {
      if (bg === '--surface-badge') continue; // opaque by design; nothing composites on it
      for (const ov of OVERLAID) {
        it(`${fg} on ${bg} under ${ov} clears 4.5:1`, () => {
          const base = solve(tokens, bg, pageBg);
          const parsed = parseColor(tokens[ov]);
          if (!parsed) throw new Error(`${ov} is not a colour: ${tokens[ov]}`);
          const backdrop = composite(parsed.rgb, parsed.alpha, base);
          const fgRgb = solve(tokens, fg, backdrop);
          const ratio = contrast(fgRgb, backdrop);
          expect(ratio, `${fg} on ${bg}+${ov} measured ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
        });
      }
    }
  });
}

auditTheme('light', LIGHT, [255, 255, 255]);

describe('dark theme completeness', () => {
  it('defines a dark block for both the media query and the attribute override', () => {
    expect(Object.keys(DARK_MEDIA).length).toBeGreaterThan(0);
    expect(Object.keys(DARK_ATTR).length).toBeGreaterThan(0);
  });

  it('overrides every themeable light token in both dark blocks', () => {
    // A colour token the dark block forgets keeps its light value and becomes
    // an invisible white-on-white (or black-on-black) bug. Geometry and type
    // tokens are theme-independent and must NOT be restated.
    const THEME_INDEPENDENT = /^--(radius|ease$|font-|size|leading)/;
    const themeable = Object.keys(LIGHT).filter((k) => !THEME_INDEPENDENT.test(k));
    expect(themeable.filter((k) => !(k in DARK_MEDIA))).toEqual([]);
    expect(themeable.filter((k) => !(k in DARK_ATTR))).toEqual([]);
  });

  it('keeps the two dark blocks identical', () => {
    // The media query and the manual override must agree, or "Dark" and
    // "System (dark)" render differently.
    expect(DARK_ATTR).toEqual(DARK_MEDIA);
  });

  it('keeps the two light blocks identical', () => {
    // Same drift risk, same generator, same fix: the manual override
    // (":root[data-theme='light']") is generated from the same source
    // ":root" block that DARK_MEDIA and DARK_ATTR are generated from, and
    // nothing enforced that it actually matches until now.
    expect(LIGHT_ATTR).toEqual(LIGHT);
  });

  it('emits flat declaration blocks for both generated overrides, not nested selectors', () => {
    // Structural, not a value comparison -- see the comment on rawBlockBody
    // above for why toEqual() on parsed values cannot catch this class of
    // bug. A bare `:root { ... }` nested inside `:root[data-theme='dark']
    // { ... }` desugars under CSS Nesting to the descendant selector
    // `:root[data-theme='dark'] :root`, which can never match (<html> is
    // not its own descendant) -- the entire block becomes inert CSS while
    // every value-level test in this file stays green. Both generated
    // blocks must be flat: no `{` anywhere in their raw body.
    const darkAttrBody = rawBlockBody(CSS, ":root[data-theme='dark']");
    const lightAttrBody = rawBlockBody(CSS, ":root[data-theme='light']");
    expect(darkAttrBody).not.toContain('{');
    expect(lightAttrBody).not.toContain('{');
  });

  it('keeps --surface-badge opaque in dark, as Tier 1 argued for light', () => {
    // Badges sit inside rows whose background changes on hover, so an alpha
    // badge composites with the row and silently loses contrast as it
    // darkens. The trap is identical inverted.
    expect(parseColor(DARK_ATTR['--surface-badge'])?.alpha).toBe(1);
  });
});

// --- Text ramp separation: --text / --text-secondary / --text-muted ---- *
// A ramp value that clears the OVERLAID contrast floor can still collapse
// into its neighbour perceptually -- Round 1's --text-muted correction to
// #595959 cleared 4.5:1 everywhere but landed dE 1.68 from --text-secondary
// #555555, the same colour to a viewer across 32 + 81 use-sites. Contrast
// ratio and perceptual separation are different questions; both need a
// guard, or a "passing" ramp can still visually collapse to two steps.
function auditTextRampSeparation(label: string, tokens: Record<string, string>, pageBg: RGB) {
  it(`${label}: --text / --text-secondary / --text-muted are pairwise distinguishable (dE > 5)`, () => {
    const text = solve(tokens, '--text', pageBg);
    const secondary = solve(tokens, '--text-secondary', pageBg);
    const muted = solve(tokens, '--text-muted', pageBg);
    const pairs: [string, RGB, string, RGB][] = [
      ['--text', text, '--text-secondary', secondary],
      ['--text-secondary', secondary, '--text-muted', muted],
      ['--text', text, '--text-muted', muted],
    ];
    for (const [nameA, a, nameB, b] of pairs) {
      const d = deltaE(a, b);
      expect(d, `${nameA} vs ${nameB} is only dE ${d.toFixed(2)} apart`).toBeGreaterThan(5);
    }
  });
}

describe('text ramp separation', () => {
  auditTextRampSeparation('light', LIGHT, [255, 255, 255]);
  auditTextRampSeparation('dark', DARK_ATTR, [0, 0, 0]);
});

auditTheme('dark', DARK_ATTR, [0, 0, 0]);

// --- Graph palette: categorical, audited in BOTH themes ---------------- *
// CORRECTED (Task 8 follow-up 2): this used to run against DARK_ATTR only.
// The light palette had actually converged -- --graph-folder and
// --graph-tag measured dE 5.3, the same swatch to a viewer -- and nothing
// ever caught it, because the coverage was half. Both themes are audited
// from here on.
const GRAPH_NAMES = ['--graph-folder', '--graph-tag', '--graph-link', '--graph-backlink'];

function auditGraphPalette(label: string, tokens: Record<string, string>, pageBg: RGB) {
  describe(`${label} graph palette`, () => {
    it('keeps the four relation types distinguishable (CIE Lab dE > 20)', () => {
      // This MUST use perceptual colour difference (CIE Lab dE), NOT contrast
      // ratio. Contrast ratio is a LUMINANCE metric. Four categorical hues
      // chosen to sit at similar lightness -- which is exactly what you want
      // for a legend, so no one relation type screams louder than the others
      // -- score near 1.0 against each other and would "fail" any contrast
      // gate. dE > 20 is the threshold for "clearly a different colour".
      const cols = GRAPH_NAMES.map((n) => solve(tokens, n, pageBg));
      for (let i = 0; i < cols.length; i++) {
        for (let j = i + 1; j < cols.length; j++) {
          const d = deltaE(cols[i], cols[j]);
          expect(d, `${GRAPH_NAMES[i]} vs ${GRAPH_NAMES[j]} is only dE ${d.toFixed(1)} apart`).toBeGreaterThan(20);
        }
      }
    });

    it('clears 3:1 on --surface-editor as a non-text graphical object (WCAG 1.4.11)', () => {
      // These tokens are SVG stroke/dot colours (NoteEditor .g-edge stroke,
      // .g-* rect stroke, .lg-* legend glyphs) on the graph modal's
      // --surface-editor background -- not text, so the bar is 3:1, not 4.5.
      // The original light set measured 1.47-1.88: nowhere close, and never
      // audited (this test did not exist before this follow-up).
      const editor = solve(tokens, '--surface-editor', pageBg);
      for (const name of GRAPH_NAMES) {
        const fg = solve(tokens, name, editor);
        const ratio = contrast(fg, editor);
        expect(ratio, `${name} on --surface-editor measured ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
      }
    });
  });
}

auditGraphPalette('light', LIGHT, [255, 255, 255]);
auditGraphPalette('dark', DARK_ATTR, [0, 0, 0]);

// --- Wash-chip backdrops: NOT a bare surface token -------------------- *
// DupReviewModal's .keeper-tag / .orphan-tag set `color: var(--success)` /
// `var(--danger)` on `background: var(--success-wash)` / `var(--danger-
// wash)`, which itself sits on the modal's `--surface-editor`. PAIRS can't
// express this (its `bg` side is always a surface token composited straight
// onto the page background), so it gets its own pair of tests rather than a
// bad fit shoehorned into the generic loop. This is exactly the case the
// PAIRS-coverage comment above calls out: found by grepping `color: var(--`
// sites and tracing each one to its real backdrop by hand.
function auditWashChip(label: string, tokens: Record<string, string>, pageBg: RGB) {
  it(`${label}: --success on --success-wash over --surface-editor clears 4.5:1`, () => {
    const editor = solve(tokens, '--surface-editor', pageBg);
    const wash = parseColor(tokens['--success-wash']);
    if (!wash) throw new Error(`--success-wash is not a colour: ${tokens['--success-wash']}`);
    const chipBg = composite(wash.rgb, wash.alpha, editor);
    const fg = solve(tokens, '--success', chipBg);
    const ratio = contrast(fg, chipBg);
    expect(ratio, `--success on --success-wash+editor measured ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
  });

  it(`${label}: --danger on --danger-wash over --surface-editor clears 4.5:1`, () => {
    const editor = solve(tokens, '--surface-editor', pageBg);
    const wash = parseColor(tokens['--danger-wash']);
    if (!wash) throw new Error(`--danger-wash is not a colour: ${tokens['--danger-wash']}`);
    const chipBg = composite(wash.rgb, wash.alpha, editor);
    const fg = solve(tokens, '--danger', chipBg);
    const ratio = contrast(fg, chipBg);
    expect(ratio, `--danger on --danger-wash+editor measured ${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
  });
}

auditWashChip('light', LIGHT, [255, 255, 255]);
auditWashChip('dark', DARK_ATTR, [0, 0, 0]);
