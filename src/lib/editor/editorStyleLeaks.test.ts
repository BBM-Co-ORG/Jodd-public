// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { stripLeakedEditorStyles } from './editorStyleLeaks';

// What WebKit wrote into a live note (2026-11-17) when Backspace merged a link
// line into the line above: the editor root's own font declarations, copied
// onto the moved text.
const LEAK = 'font-family: var(--font-note); font-size: var(--size-md);';

describe('stripLeakedEditorStyles', () => {
  it('unwraps the span WebKit adds on a line merge, keeping its text', () => {
    expect(stripLeakedEditorStyles(`<p><span style="${LEAK}">https://x.test/a</span></p>`)).toBe('<p>https://x.test/a</p>');
  });

  it('keeps a declaration that is not one of the editor\'s variables', () => {
    expect(stripLeakedEditorStyles('<span style="color: red; font-size: var(--size-md);">x</span>')).toBe(
      '<span style="color: red;">x</span>',
    );
  });

  it('keeps an element that is not a bare span, dropping only the leaked style', () => {
    expect(stripLeakedEditorStyles(`<div style="${LEAK}">a</div><b style="${LEAK}">b</b>`)).toBe('<div>a</div><b>b</b>');
    expect(stripLeakedEditorStyles(`<span class="c" style="${LEAK}">x</span>`)).toBe('<span class="c">x</span>');
  });

  it('leaves a body with no leaked style byte-identical', () => {
    const body = '<div>a&nbsp;b</div><span style="color: red">c</span><p>d</p>';
    expect(stripLeakedEditorStyles(body)).toBe(body);
  });

  it('never touches text that merely reads like a variable', () => {
    const body = '<div>use var(--size-md) in CSS</div>';
    expect(stripLeakedEditorStyles(body)).toBe(body);
  });

  it('cleans nested leaks in one pass', () => {
    expect(stripLeakedEditorStyles(`<div>a<span style="${LEAK}">b<span style="${LEAK}">c</span></span></div>`)).toBe('<div>abc</div>');
  });
});
