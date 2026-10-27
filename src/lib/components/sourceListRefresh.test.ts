// @vitest-environment jsdom
//
// Reported 2026-10-03: typing in a long note made the text jump a moment after
// each autosave. Every save re-reads the note's citations, and while that read
// was in flight SourceList swapped its whole list for one "Loading sources…"
// line — the panel shrank, the editor above it grew by the same amount, and
// the browser clamped its scrollTop (measured live: clientHeight 284 → 415,
// scrollTop 1299 → 1168, with no code touching scroll). Then it grew back.
// A refresh of a list already on screen must not change its height.
import { describe, it, expect, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import SourceList from './SourceList.svelte';

const URLS = ['https://a.example/1', 'https://b.example/2', 'https://c.example/3', 'https://d.example/4'];

describe('SourceList while its citations are re-read', () => {
  let host: HTMLElement;
  let app: Record<string, unknown> | undefined;

  afterEach(() => {
    if (app) unmount(app);
    host?.remove();
  });

  function render(props: { urls: string[]; loading: boolean }) {
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(SourceList, { target: host, props }) as Record<string, unknown>;
    flushSync();
  }

  it('keeps the sources it already shows instead of collapsing to a loading line', () => {
    render({ urls: URLS, loading: true });
    expect(host.querySelectorAll('li').length).toBe(3);
    expect(host.textContent).not.toContain('Loading sources');
  });

  it('still says it is loading when there is nothing to show yet', () => {
    render({ urls: [], loading: true });
    expect(host.textContent).toContain('Loading sources');
  });
});
