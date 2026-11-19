// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { sanitizePastedHtml, isSafePastedHref } from './pasteHtml';

describe('sanitizePastedHtml', () => {
  it('keeps structural tags and strips their attributes', () => {
    expect(sanitizePastedHtml('<b style="x">a</b><div class="c">b<br></div>')).toBe('<b>a</b><div>b<br></div>');
  });

  it('a block left empty by unwrapping is dropped (an <img> or empty <span> in a <div>)', () => {
    expect(sanitizePastedHtml('<div>a</div><div><img src="t.png"></div><div><span></span></div><div>b</div>')).toBe('<div>a</div><div>b</div>');
  });

  it('a block whose only child was an empty block goes too', () => {
    expect(sanitizePastedHtml('<div><div></div></div><div>a</div>')).toBe('<div>a</div>');
  });

  it('a blank line (<div><br></div>) and an empty list item survive', () => {
    expect(sanitizePastedHtml('<div>a</div><div><br></div><ul><li></li></ul>')).toBe('<div>a</div><div><br></div><ul><li></li></ul>');
  });

  it('a link inside a block keeps the block', () => {
    expect(sanitizePastedHtml('<div><a href="https://x.test" target="_b">x</a></div>')).toBe('<div><a href="https://x.test">x</a></div>');
  });
});

describe('a pasted paragraph is a line', () => {
  // Measured 2026-11-17 in WKWebView: a copy out of Notes.app (or any Cocoa
  // app) puts RTF on the pasteboard, and WebKit turns each RTF paragraph into
  // a <p> with an inline `margin: 0`. Stripping the style left a bare <p>,
  // which shows a one-line gap above and below — in Jodd, Gmail and Notes.app.
  const FROM_RTF =
    '<p style="margin: 0.0px 0.0px 0.0px 0.0px; font: 13.0px \'Helvetica Neue\'">#หนังสั้น<span class="Apple-converted-space">&nbsp;</span></p>';

  it('an RTF paragraph becomes a <div> line, its text unchanged', () => {
    expect(sanitizePastedHtml(FROM_RTF)).toBe('<div>#หนังสั้น&nbsp;</div>');
  });

  it('every <p> becomes a <div>, keeping what is inside it', () => {
    expect(sanitizePastedHtml('<p>a <b>b</b></p><p><a href="https://x.test">c</a></p>')).toBe(
      '<div>a <b>b</b></div><div><a href="https://x.test">c</a></div>',
    );
  });

  it('an empty paragraph is dropped like an empty <div>, a blank one stays a blank line', () => {
    expect(sanitizePastedHtml('<p>a</p><p></p><p><br></p>')).toBe('<div>a</div><div><br></div>');
  });

  it('never leaves a <p> anywhere in the result', () => {
    expect(sanitizePastedHtml('<blockquote><p>q</p></blockquote><ul><li><p>i</p></li></ul>')).not.toMatch(/<p[\s>]/);
  });
});

describe('pasted link hrefs', () => {
  it('a script or data href is dropped and the link text stays', () => {
    const out = sanitizePastedHtml('<div><a href="java\nscript:alert(1)">x</a> <a href="DATA:text/html,hi">y</a></div>');
    expect(out).not.toContain('href');
    expect(out).toContain('>x</a>');
  });

  it('web, mail and app links keep their href', () => {
    for (const href of ['https://example.com/a?b=1', 'mailto:a@example.com', 'applenotes:note/1', 'jodd://capture', '#top', '/relative']) {
      expect(isSafePastedHref(href), href).toBe(true);
    }
    expect(sanitizePastedHtml('<div><a href="https://example.com">x</a></div>')).toContain('href="https://example.com"');
  });
});
