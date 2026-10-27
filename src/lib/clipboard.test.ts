// @vitest-environment jsdom
//
// `copyText`: `navigator.clipboard` rejects in the Tauri window (live run, see
// AccountPage's copy buttons), so a rejection falls back to a selection-based
// copy. That fallback selects a hidden textarea — and the window selection is
// the editor's caret (gotcha #34), so it must come back exactly as it was.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { copyText } from './clipboard';

let execCopy: ((cmd: string) => boolean) | undefined;
let copied: string[] = [];

function stubClipboard(writeText: ((t: string) => Promise<void>) | undefined) {
  Object.defineProperty(navigator, 'clipboard', {
    configurable: true,
    value: writeText ? { writeText: vi.fn(writeText) } : undefined,
  });
}

beforeEach(() => {
  copied = [];
  document.body.innerHTML = '<div id="ed" contenteditable="true">hello world</div>';
  // jsdom has no execCommand; this one records what the hidden textarea held.
  execCopy = (cmd: string) => {
    if (cmd !== 'copy') return false;
    const ta = document.body.querySelector(':scope > textarea') as HTMLTextAreaElement | null;
    if (!ta) return false;
    copied.push(ta.value);
    return true;
  };
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string) => execCopy!(cmd),
  });
  // What a real webview does: selecting the textarea moves the document
  // selection into it, dropping the caret that was there.
  vi.spyOn(HTMLTextAreaElement.prototype, 'select').mockImplementation(function (this: HTMLTextAreaElement) {
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    const r = document.createRange();
    r.selectNodeContents(this);
    sel.addRange(r);
  });
});
afterEach(() => {
  delete (navigator as unknown as Record<string, unknown>).clipboard;
  delete (document as unknown as Record<string, unknown>).execCommand;
  vi.restoreAllMocks();
  window.getSelection()?.removeAllRanges();
  document.body.innerHTML = '';
});

function caretAt(offset: number): Text {
  const text = document.getElementById('ed')!.firstChild as Text;
  const r = document.createRange();
  r.setStart(text, offset);
  r.collapse(true);
  window.getSelection()!.removeAllRanges();
  window.getSelection()!.addRange(r);
  return text;
}

it('uses navigator.clipboard when it works, and touches nothing else', async () => {
  stubClipboard(() => Promise.resolve());
  const text = caretAt(5);
  expect(await copyText('abc')).toBe(true);
  expect((navigator.clipboard.writeText as ReturnType<typeof vi.fn>)).toHaveBeenCalledWith('abc');
  expect(copied).toEqual([]);
  const r = window.getSelection()!.getRangeAt(0);
  expect([r.startContainer, r.startOffset, r.collapsed]).toEqual([text, 5, true]);
});

it('falls back when navigator.clipboard rejects: copies, restores the selection, removes the textarea', async () => {
  stubClipboard(() => Promise.reject(new DOMException('denied', 'NotAllowedError')));
  const text = caretAt(5);
  expect(await copyText('https://example.com/x')).toBe(true);
  expect(copied).toEqual(['https://example.com/x']);
  const sel = window.getSelection()!;
  expect(sel.rangeCount).toBe(1);
  const r = sel.getRangeAt(0);
  expect([r.startContainer, r.startOffset, r.collapsed]).toEqual([text, 5, true]);
  expect(document.querySelectorAll('textarea').length).toBe(0);
});

it('falls back when there is no navigator.clipboard at all', async () => {
  stubClipboard(undefined);
  caretAt(2);
  expect(await copyText('x')).toBe(true);
  expect(copied).toEqual(['x']);
});

it('restores a non-collapsed selection too', async () => {
  stubClipboard(() => Promise.reject(new Error('no')));
  const text = document.getElementById('ed')!.firstChild as Text;
  const r0 = document.createRange();
  r0.setStart(text, 0);
  r0.setEnd(text, 5);
  window.getSelection()!.removeAllRanges();
  window.getSelection()!.addRange(r0);
  await copyText('x');
  expect(window.getSelection()!.toString()).toBe('hello');
});

it('returns false when both paths fail, and still cleans up', async () => {
  stubClipboard(() => Promise.reject(new Error('no')));
  execCopy = () => false;
  const text = caretAt(3);
  expect(await copyText('x')).toBe(false);
  expect(document.querySelectorAll('textarea').length).toBe(0);
  const r = window.getSelection()!.getRangeAt(0);
  expect([r.startContainer, r.startOffset]).toEqual([text, 3]);
});

it('returns false when execCommand throws (or does not exist)', async () => {
  stubClipboard(() => Promise.reject(new Error('no')));
  delete (document as unknown as Record<string, unknown>).execCommand;
  caretAt(1);
  expect(await copyText('x')).toBe(false);
  expect(document.querySelectorAll('textarea').length).toBe(0);
});
