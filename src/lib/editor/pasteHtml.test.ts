// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { sanitizePastedHtml, isSafePastedHref } from './pasteHtml';

describe('sanitizePastedHtml', () => {
  it('keeps structural tags and strips their attributes', () => {
    expect(sanitizePastedHtml('<p style="x">a</p><div class="c">b<br></div>')).toBe('<p>a</p><div>b<br></div>');
  });

  it('a block left empty by unwrapping is dropped (an <img> or empty <span> in a <div>)', () => {
    expect(sanitizePastedHtml('<div>a</div><div><img src="t.png"></div><div><span></span></div><p>b</p>')).toBe('<div>a</div><p>b</p>');
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
