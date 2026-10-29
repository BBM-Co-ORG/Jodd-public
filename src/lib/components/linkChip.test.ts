// @vitest-environment jsdom
//
// The link chip's one hard rule: nothing in it takes focus from the note. The
// caret is the editor's selection; a chip button that focuses itself drops it
// (and the soft keyboard on Android).
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import LinkChip from './LinkChip.svelte';
import type { LinkContext } from '../links/handlers';
import type { LinkTarget } from '../links/target';

function makeCtx(): LinkContext {
  return {
    openUrl: vi.fn().mockResolvedValue(undefined),
    openNote: vi.fn().mockResolvedValue(undefined),
    scrollToAnchor: vi.fn(),
    copy: vi.fn(),
    editLink: vi.fn(),
    removeLink: vi.fn(),
  };
}

let host: HTMLElement;
let editor: HTMLElement;
let comp: ReturnType<typeof mount> | null = null;

beforeEach(() => {
  host = document.createElement('div');
  editor = document.createElement('div');
  editor.contentEditable = 'true';
  editor.tabIndex = 0;
  document.body.append(editor, host);
});
afterEach(() => {
  if (comp) unmount(comp);
  comp = null;
  host.remove();
  editor.remove();
});

function show(target: LinkTarget, preview: unknown, ctx = makeCtx(), onclose = vi.fn()) {
  comp = mount(LinkChip, {
    target: host,
    props: { target, preview: preview as never, style: 'left: 10px; top: 20px', ctx, onclose },
  });
  flushSync();
  return { ctx, onclose };
}

const buttons = () => [...host.querySelectorAll('button')];
const labels = () => buttons().map((b) => b.textContent?.trim());

const web = (): LinkTarget => ({
  kind: 'web',
  uri: 'https://example.com/a/very/long/path?q=1',
  label: 'example',
  anchor: document.createElement('a'),
});

it('buttons never take focus', () => {
  show(web(), null);
  editor.focus();
  expect(document.activeElement).toBe(editor);
  const bs = buttons();
  expect(bs.length).toBeGreaterThan(1);
  for (const b of bs) {
    for (const type of ['pointerdown', 'mousedown']) {
      const ev = new Event(type, { bubbles: true, cancelable: true });
      b.dispatchEvent(ev);
      expect(ev.defaultPrevented, `${type} on ${b.textContent}`).toBe(true);
      expect(document.activeElement).toBe(editor);
    }
  }
});

it('shows the full URI and one button per chip action', () => {
  show(web(), null);
  expect(host.textContent).toContain('https://example.com/a/very/long/path?q=1');
  expect(labels()).toEqual(['Open in browser', 'Copy link', 'Edit link', 'Remove link']);
  const root = host.querySelector('[role="toolbar"]') as HTMLElement;
  expect(root.getAttribute('aria-label')).toBe('Link');
  expect(root.getAttribute('style')).toContain('left: 10px');
});

it('a resolved note shows its title and first line', () => {
  const t: LinkTarget = { kind: 'note', uri: 'Plan', label: 'Plan', key: 'Plan' };
  show(t, { title: 'Plan 2026', firstLine: 'Ship the thing' });
  expect(host.textContent).toContain('Plan 2026');
  expect(host.textContent).toContain('Ship the thing');
  expect(labels()).toEqual(['Open', 'Copy text']);
});

it('missing note shows only Copy', () => {
  const t: LinkTarget = { kind: 'note', uri: 'Nope', label: 'Nope', key: 'Nope' };
  const { ctx } = show(t, 'missing');
  expect(host.textContent).toContain('No note called “Nope”');
  expect(labels()).toEqual(['Copy text']);
  buttons()[0].click();
  expect(ctx.copy).toHaveBeenCalledWith('[[Nope]]');
  expect(ctx.openNote).not.toHaveBeenCalled();
});

it('Open runs and closes', () => {
  const { ctx, onclose } = show(web(), null);
  buttons()[0].click();
  expect(ctx.openUrl).toHaveBeenCalledWith('https://example.com/a/very/long/path?q=1');
  expect(onclose).toHaveBeenCalledTimes(1);
});

it('Copy runs and closes', () => {
  const { ctx, onclose } = show(web(), null);
  buttons()[1].click();
  expect(ctx.copy).toHaveBeenCalledWith('https://example.com/a/very/long/path?q=1');
  expect(onclose).toHaveBeenCalledTimes(1);
});

it('closes even when the action throws, and lets the error surface', () => {
  const ctx = makeCtx();
  const boom = new Error('clipboard denied');
  (ctx.copy as ReturnType<typeof vi.fn>).mockImplementation(() => {
    throw boom;
  });
  const { onclose } = show(web(), null, ctx);
  // A click handler's throw reaches window as an `error` event; catch it so the
  // run's output stays clean, and assert it was NOT swallowed.
  const seen: unknown[] = [];
  const onError = (e: ErrorEvent) => {
    seen.push(e.error);
    e.preventDefault();
  };
  window.addEventListener('error', onError);
  try {
    buttons()[1].click();
  } finally {
    window.removeEventListener('error', onError);
  }
  expect(ctx.copy).toHaveBeenCalledTimes(1);
  expect(onclose).toHaveBeenCalledTimes(1);
  expect(seen).toContain(boom);
});

it('runs the action before closing', () => {
  const order: string[] = [];
  const ctx = makeCtx();
  (ctx.copy as ReturnType<typeof vi.fn>).mockImplementation(() => order.push('run'));
  show(web(), null, ctx, vi.fn(() => order.push('close')));
  buttons()[1].click();
  expect(order).toEqual(['run', 'close']);
});

it('renders hostile text as text, never as markup', () => {
  const evil = '<img src=x onerror=alert(1)>';
  show({ kind: 'web', uri: evil, label: evil }, null);
  expect(host.querySelector('img')).toBeNull();
  expect(host.textContent).toContain(evil);
  unmount(comp!);
  comp = null;
  const t: LinkTarget = { kind: 'note', uri: 'x', label: 'x', key: 'x' };
  show(t, { title: evil, firstLine: evil });
  expect(host.querySelector('img')).toBeNull();
  expect(host.textContent).toContain(evil);
});
