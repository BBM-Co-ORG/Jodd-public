import { describe, it, expect } from 'vitest';
import { parseColor, contrast, composite, deltaE, parseTokens } from './color';

describe('parseColor', () => {
  it('parses 6-digit hex', () => {
    expect(parseColor('#c97c1f')).toEqual({ rgb: [201, 124, 31], alpha: 1 });
  });
  it('parses 3-digit hex', () => {
    expect(parseColor('#fff')).toEqual({ rgb: [255, 255, 255], alpha: 1 });
  });
  it('parses rgba with alpha', () => {
    expect(parseColor('rgba(0, 0, 0, 0.35)')).toEqual({ rgb: [0, 0, 0], alpha: 0.35 });
  });
  it('parses rgb without alpha', () => {
    expect(parseColor('rgb(34, 34, 34)')).toEqual({ rgb: [34, 34, 34], alpha: 1 });
  });
  it('returns null for a non-colour', () => {
    expect(parseColor('0 6px 24px rgba(0,0,0,0.16)')).toBeNull();
  });
});

describe('contrast', () => {
  // Published WCAG reference values.
  it('black on white is 21:1', () => {
    expect(contrast([0, 0, 0], [255, 255, 255])).toBeCloseTo(21, 2);
  });
  it('white on white is 1:1', () => {
    expect(contrast([255, 255, 255], [255, 255, 255])).toBeCloseTo(1, 5);
  });
  it('is symmetric', () => {
    const a = contrast([201, 124, 31], [255, 255, 255]);
    const b = contrast([255, 255, 255], [201, 124, 31]);
    expect(a).toBeCloseTo(b, 10);
  });
  it('reproduces the Tier 1 measurement for --accent-action', () => {
    // tokens.css records "white on it 5.41:1".
    expect(contrast([255, 255, 255], [156, 90, 18])).toBeCloseTo(5.41, 1);
  });
});

describe('deltaE', () => {
  it('is zero for identical colours', () => {
    expect(deltaE([201, 124, 31], [201, 124, 31])).toBeCloseTo(0, 6);
  });
  it('separates the graph palette well past the categorical threshold', () => {
    // Measured minimum across all six pairs of the real dark palette: 34.8.
    expect(deltaE([217, 169, 92], [110, 168, 216])).toBeGreaterThan(20);
  });
  it('rates equal-luminance hues as different even though contrast says 1.0', () => {
    // The exact failure mode a contrast-ratio gate has on categorical palettes.
    const a: [number, number, number] = [217, 169, 92];
    const b: [number, number, number] = [108, 192, 138];
    expect(contrast(a, b)).toBeLessThan(1.35);
    expect(deltaE(a, b)).toBeGreaterThan(20);
  });
});

describe('composite', () => {
  it('collapses to the backdrop at alpha 0', () => {
    expect(composite([0, 0, 0], 0, [255, 255, 255])).toEqual([255, 255, 255]);
  });
  it('collapses to the overlay at alpha 1', () => {
    expect(composite([0, 0, 0], 1, [255, 255, 255])).toEqual([0, 0, 0]);
  });
  it('blends linearly in between', () => {
    expect(composite([0, 0, 0], 0.5, [255, 255, 255])).toEqual([128, 128, 128]);
  });
});

describe('parseTokens', () => {
  const css = `
    :root { --a: #fff; --b: rgba(0, 0, 0, 0.5); }
    :root[data-theme='dark'] { --a: #000; }
  `;
  it('reads a named block', () => {
    expect(parseTokens(css, ':root')).toEqual({ '--a': '#fff', '--b': 'rgba(0, 0, 0, 0.5)' });
  });
  it('reads an attribute-qualified block', () => {
    expect(parseTokens(css, ":root[data-theme='dark']")).toEqual({ '--a': '#000' });
  });
});
