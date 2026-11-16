// @vitest-environment jsdom
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { plainTextToPasteHtml, pastePlainText } from './pastePlainText';

// jsdom has no execCommand and no undo history, so what these tests can pin is
// the command issued and the markup handed to it. That the result is a separate
// undo step per paste is WebKit's behaviour, measured in
// scripts/webkit-editor-harness (keys/paste-*.txt).
let exec: ReturnType<typeof vi.fn>;
beforeEach(() => {
  exec = vi.fn(() => true);
  Object.defineProperty(document, 'execCommand', { value: exec, configurable: true, writable: true });
});

describe('plainTextToPasteHtml', () => {
  it('leaves a single line as bare escaped text, so it joins the caret line', () => {
    expect(plainTextToPasteHtml('https://example.com/a?x=1&y=<2>')).toBe(
      'https://example.com/a?x=1&amp;y=&lt;2&gt;',
    );
  });

  it('gives every line after the first its own <div>, an empty one a <br>', () => {
    // The shape WebKit's own insertText makes of "aa\nbb\n\ncc" — measured.
    expect(plainTextToPasteHtml('aa\nbb\n\ncc')).toBe('aa<div>bb</div><div><br></div><div>cc</div>');
  });

  it('keeps trailing spaces (and the leading ones of later lines) as &nbsp;', () => {
    // insertText does this itself and plain trailing spaces are collapsed away
    // once the body is rendered — measured: 'c   ' came back as 'c &nbsp;&nbsp;'.
    expect(plainTextToPasteHtml('c   ')).toBe('c &nbsp;&nbsp;');
    expect(plainTextToPasteHtml('a  \n  b  ')).toBe('a &nbsp;<div>&nbsp;&nbsp;b &nbsp;</div>');
    expect(plainTextToPasteHtml(' a  b ')).toBe(' a  b&nbsp;');
  });

  it('treats CRLF and lone CR as line breaks', () => {
    expect(plainTextToPasteHtml('a\r\nb\rc')).toBe('a<div>b</div><div>c</div>');
  });
});

describe('pastePlainText', () => {
  it('inserts through insertHTML, never insertText', () => {
    // insertText joins the still-open typing command, so two pastes (and the
    // typing before them) undo together — measured in WebKit.
    pastePlainText('https://example.com/a');
    expect(exec).toHaveBeenCalledTimes(1);
    expect(exec).toHaveBeenCalledWith('insertHTML', false, 'https://example.com/a');
  });

  it('does nothing for an empty clipboard string (an empty insertHTML splits the block)', () => {
    expect(pastePlainText('')).toBe(false);
    expect(exec).not.toHaveBeenCalled();
  });
});
