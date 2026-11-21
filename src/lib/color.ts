// Pure colour maths for the design-token guard tests. No DOM, no Svelte.
// Exists so contrast is asserted rather than eyeballed -- dark themes fail
// differently from light ones and light-text-on-mid-grey is the usual
// casualty, which is exactly the failure an eyeball waves through.

export type RGB = [number, number, number];

function srgbToLinear(channel: number): number {
  const s = channel / 255;
  return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
}

export function luminance([r, g, b]: RGB): number {
  return 0.2126 * srgbToLinear(r) + 0.7152 * srgbToLinear(g) + 0.0722 * srgbToLinear(b);
}

export function contrast(fg: RGB, bg: RGB): number {
  const [hi, lo] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}

// Alpha stacks: a badge on an alpha background inside a row with its own
// alpha background is three layers deep, and that is where failures hide.
export function composite(fg: RGB, alpha: number, bg: RGB): RGB {
  return [0, 1, 2].map((i) => Math.round(fg[i] * alpha + bg[i] * (1 - alpha))) as RGB;
}

// --- Perceptual colour difference -------------------------------------- *
// For CATEGORICAL palettes (the graph's four relation types), contrast ratio
// is the wrong tool: it measures luminance, and hues deliberately placed at
// equal lightness score ~1.0 against each other while being trivially
// distinguishable. Lab dE measures perceived colour difference instead.
// dE > 20 == "clearly a different colour".
export function toLab([r, g, b]: RGB): [number, number, number] {
  const [R, G, B] = [r, g, b].map(srgbToLinear);
  let X = (R * 0.4124 + G * 0.3576 + B * 0.1805) / 0.95047;
  let Y = R * 0.2126 + G * 0.7152 + B * 0.0722;
  let Z = (R * 0.0193 + G * 0.1192 + B * 0.9505) / 1.08883;
  const f = (t: number) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
  [X, Y, Z] = [f(X), f(Y), f(Z)];
  return [116 * Y - 16, 500 * (X - Y), 200 * (Y - Z)];
}

export function deltaE(a: RGB, b: RGB): number {
  const A = toLab(a);
  const B = toLab(b);
  return Math.hypot(A[0] - B[0], A[1] - B[1], A[2] - B[2]);
}

export function parseColor(value: string): { rgb: RGB; alpha: number } | null {
  const v = value.trim();

  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(v);
  if (hex) {
    const h = hex[1];
    const full = h.length === 3 ? h.split('').map((c) => c + c).join('') : h;
    return {
      rgb: [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16)) as RGB,
      alpha: 1,
    };
  }

  // Anchored: a box-shadow value contains an rgba() but is not itself a colour.
  const fn = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)$/i.exec(v);
  if (fn) {
    return {
      rgb: [Number(fn[1]), Number(fn[2]), Number(fn[3])],
      alpha: fn[4] === undefined ? 1 : Number(fn[4]),
    };
  }

  return null;
}

// Extracts one selector's custom properties. Comment-stripped first so a
// commented-out token cannot masquerade as a live one.
export function parseTokens(css: string, selector: string): Record<string, string> {
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const idx = clean.indexOf(selector);
  if (idx === -1) return {};
  const open = clean.indexOf('{', idx + selector.length);
  if (open === -1) return {};

  let depth = 1;
  let i = open + 1;
  for (; i < clean.length && depth > 0; i++) {
    if (clean[i] === '{') depth++;
    else if (clean[i] === '}') depth--;
  }

  const body = clean.slice(open + 1, i - 1);
  const out: Record<string, string> = {};
  for (const m of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    out[m[1]] = m[2].trim();
  }
  return out;
}
