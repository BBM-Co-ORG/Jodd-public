// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest';
import { externalHref, handleEditorLinkClick } from './linkOpen';

function editor(html: string) {
  const root = document.createElement('div');
  root.contentEditable = 'true';
  root.innerHTML = html;
  return root;
}

function click(target: Element, mods: Partial<MouseEvent> = {}) {
  return {
    target,
    button: 0,
    metaKey: false,
    ctrlKey: false,
    shiftKey: false,
    altKey: false,
    preventDefault: vi.fn(),
    ...mods,
  } as unknown as MouseEvent;
}

describe('externalHref', () => {
  it('finds the link around a nested target', () => {
    const root = editor('<a href="https://a.example/x"><b>bold</b></a>');
    expect(externalHref(root.querySelector('b'))).toBe('https://a.example/x');
  });

  it('accepts http, https and mailto only', () => {
    for (const ok of ['http://a.example/', 'https://a.example/', 'mailto:me@a.example']) {
      expect(externalHref(editor(`<a href="${ok}">x</a>`).querySelector('a'))).toBe(ok);
    }
    for (const bad of ['javascript:alert(1)', 'data:text/html,x', '#meeting-1', '/relative', 'notaurl']) {
      expect(externalHref(editor(`<a href="${bad}">x</a>`).querySelector('a')), bad).toBeNull();
    }
  });

  it('is null off a link', () => {
    expect(externalHref(editor('<div>plain</div>').querySelector('div'))).toBeNull();
    expect(externalHref(null)).toBeNull();
  });
});

describe('handleEditorLinkClick', () => {
  const html = '<div>see <a href="https://a.example/x">https://a.example/x</a></div>';

  it('Cmd+click opens the link on macOS', () => {
    const root = editor(html);
    const open = vi.fn();
    const e = click(root.querySelector('a')!, { metaKey: true });
    expect(handleEditorLinkClick(e, true, open)).toBe(true);
    expect(open).toHaveBeenCalledWith('https://a.example/x');
    expect(e.preventDefault).toHaveBeenCalledOnce();
  });

  it('Ctrl+click opens the link off macOS', () => {
    const root = editor(html);
    const open = vi.fn();
    expect(handleEditorLinkClick(click(root.querySelector('a')!, { ctrlKey: true }), false, open)).toBe(true);
    expect(open).toHaveBeenCalledWith('https://a.example/x');
  });

  it('a plain click only places the caret: nothing opens, the webview does not navigate', () => {
    const root = editor(html);
    const open = vi.fn();
    const e = click(root.querySelector('a')!);
    expect(handleEditorLinkClick(e, true, open)).toBe(false);
    expect(open).not.toHaveBeenCalled();
    // preventDefault on the click (the caret moved on mousedown) is what
    // keeps the opener plugin's window-level listener off it.
    expect(e.preventDefault).toHaveBeenCalledOnce();
  });

  it('Shift+click extends the selection rather than launching the browser', () => {
    const root = editor(html);
    const open = vi.fn();
    const e = click(root.querySelector('a')!, { shiftKey: true });
    expect(handleEditorLinkClick(e, true, open)).toBe(false);
    expect(open).not.toHaveBeenCalled();
    expect(e.preventDefault).toHaveBeenCalledOnce();
  });

  it('Ctrl+click on macOS is not the open chord', () => {
    const root = editor(html);
    const open = vi.fn();
    expect(handleEditorLinkClick(click(root.querySelector('a')!, { ctrlKey: true }), true, open)).toBe(false);
    expect(open).not.toHaveBeenCalled();
  });

  it('leaves clicks off external links alone', () => {
    const root = editor('<div>text</div><a href="#meeting-1">evidence</a>');
    const open = vi.fn();
    const onText = click(root.querySelector('div')!, { metaKey: true });
    expect(handleEditorLinkClick(onText, true, open)).toBe(false);
    expect(onText.preventDefault).not.toHaveBeenCalled();
    const onEvidence = click(root.querySelector('a')!, { metaKey: true });
    expect(handleEditorLinkClick(onEvidence, true, open)).toBe(false);
    expect(onEvidence.preventDefault).not.toHaveBeenCalled();
    expect(open).not.toHaveBeenCalled();
  });

  it('ignores a non-primary button', () => {
    const root = editor(html);
    const open = vi.fn();
    expect(handleEditorLinkClick(click(root.querySelector('a')!, { metaKey: true, button: 1 }), true, open)).toBe(false);
    expect(open).not.toHaveBeenCalled();
  });

  it('never rewrites the stored HTML', () => {
    const root = editor(html);
    const before = root.innerHTML;
    handleEditorLinkClick(click(root.querySelector('a')!, { metaKey: true }), true, vi.fn());
    handleEditorLinkClick(click(root.querySelector('a')!), true, vi.fn());
    expect(root.innerHTML).toBe(before);
  });
});
