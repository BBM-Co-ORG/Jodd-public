// @vitest-environment jsdom
//
// The editor's line-number gutter (⋯ › Show line numbers). It numbers
// noteLines(editor) — the tooltip's "N lines" is its last number — at each
// line's measured top, re-measures on any edit, and draws only near the view.
// jsdom does no layout: each element's box comes from its data-top / data-h.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import LineGutter from './LineGutter.svelte';
import { noteStats } from '../editor/noteLines';

let host: HTMLElement;
let editor: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let cmp: any;

function rect(top: number, height: number): DOMRect {
  const width = height > 0 ? 100 : 0;
  return { top, height, width, bottom: top + height, left: 0, right: width, x: 0, y: top, toJSON: () => ({}) } as DOMRect;
}

beforeEach(() => {
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
    return rect(Number(this.dataset.top ?? 0), Number(this.dataset.h ?? 20));
  });
  host = document.createElement('div');
  document.body.appendChild(host);
  editor = document.createElement('div');
  editor.dataset.top = '0';
  Object.defineProperty(editor, 'clientHeight', { configurable: true, value: 500 });
  host.appendChild(editor);
});
afterEach(() => {
  if (cmp) unmount(cmp);
  cmp = null;
  host.remove();
  vi.restoreAllMocks();
});

function render(html: string) {
  editor.innerHTML = html;
  cmp = mount(LineGutter, { target: host, props: { editor } });
  flushSync();
}
const numbers = () => [...host.querySelectorAll<HTMLElement>('.line-no')].map((n) => ({ n: n.textContent, top: n.style.top }));

describe('LineGutter', () => {
  it('numbers each line at its measured top', async () => {
    render('<div data-top="10">a</div><div data-top="30">b</div><div data-top="50">c</div>');
    await vi.waitFor(() => expect(numbers()).toEqual([
      { n: '1', top: '10px' }, { n: '2', top: '30px' }, { n: '3', top: '50px' },
    ]));
  });

  it('its last number is the tooltip\'s line count: a closed <details> is its summary', async () => {
    const html = '<div data-top="0">a</div><ul><li data-top="20">b</li><li data-top="40">c</li></ul>'
      + '<details data-top="60"><summary data-top="60">Src</summary><div data-top="80">hidden</div></details>';
    render(html);
    await vi.waitFor(() => expect(numbers().at(-1)?.n).toBe(String(noteStats('', html).lines)));
    expect(numbers().map((x) => x.n)).toEqual(['1', '2', '3', '4']);
  });

  it('re-measures after an edit', async () => {
    render('<div data-top="0">a</div>');
    await vi.waitFor(() => expect(numbers()).toHaveLength(1));
    const d = document.createElement('div');
    d.dataset.top = '20';
    d.textContent = 'b';
    editor.appendChild(d);
    await vi.waitFor(() => expect(numbers().map((x) => x.n)).toEqual(['1', '2']));
  });

  it('a line with no box keeps its number but draws none', async () => {
    render('<div data-top="0">a</div><div data-top="0" data-h="0">gone</div><div data-top="20">c</div>');
    await vi.waitFor(() => expect(numbers().map((x) => x.n)).toEqual(['1', '3']));
  });

  it('draws only the lines near the view, and follows the scroll', async () => {
    render('<div data-top="0">a</div><div data-top="5000">far</div>');
    await vi.waitFor(() => expect(numbers().map((x) => x.n)).toEqual(['1']));
    editor.scrollTop = 4800;
    editor.dispatchEvent(new Event('scroll'));
    flushSync();
    expect(numbers().map((x) => x.n)).toContain('2');
    expect((host.querySelector('.line-gutter-layer') as HTMLElement).style.transform).toBe('translateY(-4800px)');
  });

  it('is decoration: hidden from assistive tech, and nothing of it is inside the editor', async () => {
    render('<div data-top="0">a</div>');
    await vi.waitFor(() => expect(numbers()).toHaveLength(1));
    expect(host.querySelector('[data-testid="line-gutter"]')!.getAttribute('aria-hidden')).toBe('true');
    expect(editor.querySelector('.line-no')).toBeNull();
  });
});
