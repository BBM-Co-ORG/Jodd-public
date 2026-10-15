// @vitest-environment jsdom
//
// Android tablet layout (Galaxy Tab S7, reported 2026-09-28 from v0.29.1):
// the ☰ Folders toggle was `position: absolute` over the note list, so it
// took no space. The note list's search box started at y=0, on the same row
// as the status bar clock, with the ☰ button covering its left edge. The fix
// is an in-flow top bar, like the phone layout's .phone-pane-header: it
// clears the status bar and everything else starts below it.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import { readFileSync } from 'node:fs';

// The drawer's Sidebar is the real app sidebar (IPC, stores, accounts);
// this test is about the frame around it, not its contents.
vi.mock('./Sidebar.svelte', async () => ({
  default: (await import('./__fixtures__/StubSidebar.svelte')).default,
}));

import TabletChrome from './TabletChrome.svelte';

let host: HTMLElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let component: any;

function mountChrome() {
  host = document.createElement('div');
  document.body.appendChild(host);
  const children = createRawSnippet(() => ({
    render: () => '<div class="pane-under-test"><input placeholder="Search this account…" /></div>',
  }));
  component = mount(TabletChrome, { target: host, props: { children } });
  flushSync();
}

afterEach(() => {
  if (component) unmount(component);
  host?.remove();
});

describe('Android tablet top bar', () => {
  it('puts the ☰ toggle inside an in-flow top bar that precedes the panes', () => {
    mountChrome();
    const layout = host.querySelector('.tablet-layout')!;
    const bar = layout.querySelector(':scope > .tablet-pane-header');
    const panes = layout.querySelector(':scope > .tablet-panes');
    expect(bar).not.toBeNull();
    expect(panes).not.toBeNull();
    // The bar comes first in document order, so the panes lay out below it.
    expect(bar!.compareDocumentPosition(panes!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const toggle = bar!.querySelector('button[aria-label="Folders"]');
    expect(toggle).not.toBeNull();
    // The panes (note list with its search box) render inside .tablet-panes,
    // not beside a floating button.
    expect(panes!.querySelector('.pane-under-test input')).not.toBeNull();
    expect(bar!.contains(panes!)).toBe(false);
  });

  it('opens and closes the folders drawer from the top-bar toggle', () => {
    mountChrome();
    expect(host.querySelector('.tablet-drawer')).toBeNull();
    (host.querySelector('.tablet-pane-header button[aria-label="Folders"]') as HTMLButtonElement).click();
    flushSync();
    expect(host.querySelector('.tablet-drawer .stub-sidebar')).not.toBeNull();
    (host.querySelector('.tablet-drawer-backdrop') as HTMLElement).click();
    flushSync();
    expect(host.querySelector('.tablet-drawer')).toBeNull();
  });

  // jsdom does not apply component CSS, so the geometry is pinned at the
  // source: the bar reserves the status bar (same expression as the phone
  // header), nothing in the tablet frame floats, and the panes' 100vh
  // children are not allowed to overflow the space under the bar.
  it('clears the status bar in flow, without absolute positioning', () => {
    const src = readFileSync('src/lib/components/TabletChrome.svelte', 'utf8');
    const css = src.slice(src.indexOf('<style>'));
    const rule = (sel: string) => {
      const m = css.match(new RegExp(`\\n\\s*${sel.replace('.', '\\.')}\\s*\\{([^}]*)\\}`));
      return m ? m[1] : '';
    };
    expect(rule('.tablet-pane-header')).toMatch(/padding:\s*max\(8px,\s*env\(safe-area-inset-top\)\)/);
    expect(rule('.tablet-pane-header')).not.toMatch(/position:\s*(absolute|fixed)/);
    expect(rule('.tablet-panes')).toMatch(/min-height:\s*0/);
    expect(css).toMatch(/\.tablet-panes\s*>\s*:global\(\*\)\s*\{[^}]*height:\s*auto/);
  });
});
