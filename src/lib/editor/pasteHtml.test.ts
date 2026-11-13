// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { sanitizePastedHtml } from './pasteHtml';

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
