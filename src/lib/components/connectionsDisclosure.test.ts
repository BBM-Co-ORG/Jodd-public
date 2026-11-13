// @vitest-environment jsdom
//
// Task 5.4 (spec 2026-10-20 §6.5): Links to, Linked from and Sources fold into
// one disclosure line. The component owns the line, the remembered open state
// and the loading/failed wording; the caller owns counts and content.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import { writable, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(false) };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import ConnectionsDisclosure from './ConnectionsDisclosure.svelte';
import { isAndroid as isAndroidRO } from '../stores/platform';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const KEY = 'jodd.connections.open';

type Props = {
  sources: number; links: number; backlinks: number;
  sourcesState: 'ok' | 'loading' | 'failed'; onRetrySources: () => void;
};

let host: HTMLElement;
let app: Record<string, unknown> | undefined;
const body = createRawSnippet(() => ({ render: () => '<p class="body-probe">groups</p>' }));

function render(p: Partial<Props> = {}) {
  host = document.createElement('div');
  document.body.appendChild(host);
  app = mount(ConnectionsDisclosure, {
    target: host,
    props: { sources: 0, links: 0, backlinks: 0, sourcesState: 'ok', onRetrySources: () => {}, ...p, children: body },
  }) as Record<string, unknown>;
  flushSync();
}
const toggle = () => host.querySelector('button.toggle') as HTMLButtonElement;
const line = () => host.querySelector('.line')?.textContent?.replace(/\s+/g, ' ').trim();

beforeEach(() => { localStorage.clear(); isAndroid.set(false); invoke.mockReset(); });
afterEach(() => { if (app) unmount(app); app = undefined; host?.remove(); });

describe('ConnectionsDisclosure', () => {
  it('collapsed line reads "Sources 4 · Links 3 · Linked from 2"', () => {
    render({ sources: 4, links: 3, backlinks: 2 });
    expect(line()).toBe('Sources 4 · Links 3 · Linked from 2');
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(host.querySelector('.body-probe')).toBeNull();
  });

  it('omits a segment whose count is 0', () => {
    render({ sources: 4, links: 0, backlinks: 2 });
    expect(line()).toBe('Sources 4 · Linked from 2');
  });

  it('is hidden when all counts are 0 and sources are ok', () => {
    render();
    expect(host.querySelector('.connections-disclosure')).toBeNull();
    expect(host.textContent).toBe('');
  });

  it('failed shows "Sources unavailable · Retry" while collapsed, and Retry does not toggle', () => {
    const retry = vi.fn();
    render({ sourcesState: 'failed', onRetrySources: retry });
    expect(line()).toBe('Sources unavailable · Retry');
    (host.querySelector('button.retry') as HTMLButtonElement).click();
    flushSync();
    expect(retry).toHaveBeenCalledTimes(1);
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  it('loading with nothing known says "Sources…", with other counts alongside', () => {
    render({ sourcesState: 'loading', links: 3 });
    expect(line()).toBe('Sources… · Links 3');
  });

  it('opens on click: aria-controls points at the panel that holds the groups', () => {
    render({ sources: 1 });
    toggle().click(); flushSync();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    const panel = document.getElementById(toggle().getAttribute('aria-controls')!);
    expect(panel?.querySelector('.body-probe')).not.toBeNull();
  });

  it('is a real button, so Enter and Space operate it natively', () => {
    render({ sources: 1 });
    expect(toggle().tagName).toBe('BUTTON');
    expect(toggle().getAttribute('type')).toBe('button');
  });

  it('open state persists: written on toggle, read by the next mount', () => {
    render({ sources: 2 });
    toggle().click(); flushSync();
    expect(localStorage.getItem(KEY)).toBe('1');
    unmount(app!); host.remove();
    render({ sources: 2 });
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(host.querySelector('.body-probe')).not.toBeNull();
    toggle().click(); flushSync();
    expect(localStorage.getItem(KEY)).toBe('0');
  });

  it('defaults closed when storage throws, and a toggle does not throw', () => {
    const get = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked'); });
    const set = vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked'); });
    try {
      render({ sources: 2 });
      expect(toggle().getAttribute('aria-expanded')).toBe('false');
      expect(() => { toggle().click(); flushSync(); }).not.toThrow();
      expect(toggle().getAttribute('aria-expanded')).toBe('true');
    } finally { get.mockRestore(); set.mockRestore(); }
  });

  it('never reaches the backend', () => {
    render({ sources: 2, links: 1 });
    toggle().click(); flushSync();
    expect(invoke.mock.calls.filter(([c]) => c !== 'platform_name')).toEqual([]);
  });

  it('takes the touch-sized hit area on Android only', () => {
    render({ sources: 1 });
    expect(host.querySelector('.connections-disclosure')!.classList.contains('touch')).toBe(false);
    unmount(app!); host.remove();
    isAndroid.set(true);
    render({ sources: 1 });
    expect(host.querySelector('.connections-disclosure')!.classList.contains('touch')).toBe(true);
  });
});
