import { describe, it, expect } from 'vitest';
import { appendHtml, bodyContent } from './noteHtml';

const DOC = '<html><head></head><body style="overflow-wrap: break-word;"><div>human text</div></body></html>';

describe('appendHtml', () => {
  it('appends inside the body of a whole document (gotcha #37)', () => {
    expect(appendHtml(DOC, '<p>Related: [[x]]</p>')).toBe(
      '<html><head></head><body style="overflow-wrap: break-word;"><div>human text</div><p>Related: [[x]]</p></body></html>',
    );
  });
  it('appends to a fragment unchanged', () => {
    expect(appendHtml('<div>a</div>', '<p>b</p>')).toBe('<div>a</div><p>b</p>');
  });
  it('moves content an older build stranded after </html> back inside, in order', () => {
    expect(appendHtml(`${DOC}<p>stranded</p>\n`, '<p>new</p>')).toBe(
      '<html><head></head><body style="overflow-wrap: break-word;"><div>human text</div><p>stranded</p>\n<p>new</p></body></html>',
    );
  });
});

describe('bodyContent', () => {
  it('takes what is inside <body>', () => {
    expect(bodyContent(DOC)).toBe('<div>human text</div>');
  });
  it('passes a fragment through', () => {
    expect(bodyContent('<div>frag</div>')).toBe('<div>frag</div>');
  });
  it('keeps content stranded after </html>, so the editor shows it and its next save keeps it', () => {
    expect(bodyContent(`${DOC}<p>Appended by an agent</p>\n`)).toBe('<div>human text</div><p>Appended by an agent</p>\n');
  });
});
