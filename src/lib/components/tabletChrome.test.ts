// @vitest-environment jsdom
//
// Android tablet layout (Galaxy Tab S7, reported 2026-09-28 from v0.29.1):
// the ☰ Folders toggle was `position: absolute` over the note list, so it
// took no space. The note list's search box started at y=0, on the same row
// as the status bar clock, with the ☰ button covering its left edge. The fix
// is an in-flow top bar, like the phone layout's .phone-pane-header: it
// clears the status bar and everything else starts below it.
//
// Spec 2026-10-20 §5.4: on a wide tablet ('tabletWide', ≥ 1024) the folders
// are a persistent in-flow column the ☰ collapses (kept in localStorage); on
// a portrait tablet ('tablet') they are an overlay drawer that owns a history
// entry — Android Back, the scrim and a pick all close it.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import { writable, type Writable } from 'svelte/store';
import { readFileSync } from 'node:fs';

// The drawer's Sidebar is the real app sidebar (IPC, stores, accounts);
// this test is about the frame around it, not its contents. The stub fires
// the Sidebar's `pick` and `collapse` events from two buttons.
vi.mock('./Sidebar.svelte', async () => ({
  default: (await import('./__fixtures__/StubSidebar.svelte')).default,
}));
vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(true),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('tablet'),
}));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: async () => [],
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));

import TabletChrome from './TabletChrome.svelte';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import {
  accounts, currentAccount, searchQuery, selectedFolder, selectedNote, selectedSmartFolder, selectedTags,
} from '../stores/notes';
import { forwardNoteIdentity, clearNoteAliases } from '../noteIdentity';
import type { Note } from '../types';

const layout = layoutReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;
const OPEN_KEY = 'jodd.tablet.sidebarOpen';
const ACCT = 'gmail:a@b.com';

const note = (uuid: string): Note => ({
  uuid, id: `m-${uuid}`, account_id: ACCT, title: `Note ${uuid}`, body_html: '<p>x</p>',
  date: '2026-10-20T00:00:00Z', label: 'Notes', x_mail_created_date: null,
} as Note);

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

function unmountChrome() {
  if (component) unmount(component);
  component = undefined;
  flushSync();
}

const toggle = () => host.querySelector<HTMLButtonElement>('.tablet-pane-header button[aria-label="Folders"]')!;
const click = (el: Element | null) => { (el as HTMLElement).click(); flushSync(); };
const drawer = () => host.querySelector('.tablet-drawer');
const scrim = () => host.querySelector('.tablet-drawer-backdrop');
const column = () => host.querySelector('.tablet-column');
const overlay = () => overlayFromHistoryState(history.state);

async function openDrawer() {
  click(toggle());
  expect(drawer()).not.toBeNull();
  await vi.waitFor(() => expect(overlay()).toBe('drawer'));
}

/** Closed, with its history entry gone (popOverlay's back has landed). */
async function expectDrawerGone() {
  expect(drawer()).toBeNull();
  expect(scrim()).toBeNull();
  await vi.waitFor(() => expect(overlay()).toBeNull());
}

beforeEach(() => {
  // A fresh page: no entries of an earlier test above this one.
  history.replaceState(null, '');
  try { localStorage.removeItem(OPEN_KEY); } catch { /* ignore */ }
  layout.set('tablet');
  accounts.set([{ id: ACCT, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set(ACCT);
  selectedFolder.set('Notes/Projects');
  selectedSmartFolder.set(null);
  selectedTags.set(new Set());
  searchQuery.set('');
  selectedNote.set(null);
  clearNoteAliases();
  window.addEventListener('popstate', handlePopState);
});

afterEach(async () => {
  unmountChrome();
  host?.remove();
  // Whatever a test left open, its entry must be gone before the next.
  await vi.waitFor(() => expect(overlay()).toBeNull());
  window.removeEventListener('popstate', handlePopState);
  vi.restoreAllMocks();
});

describe('Android tablet top bar', () => {
  it('puts the ☰ toggle inside an in-flow top bar that precedes the panes', () => {
    mountChrome();
    const layoutEl = host.querySelector('.tablet-layout')!;
    const bar = layoutEl.querySelector(':scope > .tablet-pane-header');
    const panes = layoutEl.querySelector(':scope > .tablet-panes');
    expect(bar).not.toBeNull();
    expect(panes).not.toBeNull();
    // The bar comes first in document order, so the panes lay out below it.
    expect(bar!.compareDocumentPosition(panes!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const t = bar!.querySelector('button[aria-label="Folders"]');
    expect(t).not.toBeNull();
    // The panes (note list with its search box) render inside .tablet-panes,
    // not beside a floating button.
    expect(panes!.querySelector('.pane-under-test input')).not.toBeNull();
    expect(bar!.contains(panes!)).toBe(false);
  });

  it('top bar shows view name', () => {
    mountChrome();
    const title = () => host.querySelector('.tablet-pane-header .tablet-title')?.textContent?.trim();
    expect(title()).toBe('Projects');
    selectedSmartFolder.set({ account: ACCT, kind: 'orphaned' } as never);
    flushSync();
    expect(title()).toBe('Orphaned');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set(['study']));
    flushSync();
    expect(title()).toBe('#study');
  });

  // jsdom does not apply component CSS, so the geometry is pinned at the
  // source: the bar reserves the status bar (same expression as the phone
  // header), nothing in the tablet frame floats, and the panes' 100dvh
  // children are not allowed to overflow the space under the bar.
  it('clears the status bar in flow, without absolute positioning', () => {
    const src = readFileSync('src/lib/components/TabletChrome.svelte', 'utf8');
    const css = src.slice(src.indexOf('<style>'));
    const rule = (sel: string) => {
      const m = css.match(new RegExp(`\\n\\s*${sel.replace('.', '\\.')}\\s*\\{([^}]*)\\}`));
      return m ? m[1] : '';
    };
    // An 8px floor: the literal, or --space-2 (8px), which the Sidebar's
    // aligned header copies (2026-10-21).
    expect(rule('.tablet-pane-header')).toMatch(/padding:\s*max\((8px|var\(--space-2\)),\s*env\(safe-area-inset-top\)\)/);
    expect(rule('.tablet-pane-header')).not.toMatch(/position:\s*(absolute|fixed)/);
    expect(rule('.tablet-panes')).toMatch(/min-height:\s*0/);
    expect(css).toMatch(/\.tablet-panes\s*>\s*:global\(\*\)\s*\{[^}]*height:\s*auto/);
    // The drawer stacks on the layer token, and the new rules use spacing
    // tokens (the bar's status-bar floor is the one pinned literal).
    expect(rule('.tablet-drawer-backdrop')).toMatch(/z-index:\s*var\(--z-drawer\)/);
    expect(rule('.tablet-drawer')).toMatch(/z-index:\s*calc\(var\(--z-drawer\)\s*\+\s*1\)/);
    const withoutFloor = css.replace(/max\((8px|var\(--space-2\)),\s*env\(safe-area-inset-top\)\)/g, '');
    expect(withoutFloor).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
  });
});

describe('portrait tablet: overlay drawer', () => {
  it('opens and closes the folders drawer from the top-bar toggle', async () => {
    mountChrome();
    expect(drawer()).toBeNull();
    await openDrawer();
    expect(host.querySelector('.tablet-drawer .stub-sidebar')!.getAttribute('data-tablet-frame')).toBe('drawer');
    expect(column()).toBeNull();
    click(scrim());
    await expectDrawerGone();
  });

  it('tablet drawer open pushes an overlay entry; popstate closes it', async () => {
    mountChrome();
    await openDrawer();
    // One entry above the fresh page (history.length cannot say so in jsdom:
    // a push over earlier tests' forward entries replaces them).
    expect(history.state).toMatchObject({ overlay: 'drawer', depth: 1 });
    // Android Back: the WebView goes back one entry.
    history.back();
    await vi.waitFor(() => expect(drawer()).toBeNull());
    expect(scrim()).toBeNull();
    expect(overlay()).toBeNull();
    expect(history.state).toBeNull();
  });

  it('a pick in the drawer closes it and removes its entry', async () => {
    mountChrome();
    await openDrawer();
    click(host.querySelector('.tablet-drawer .stub-pick'));
    await expectDrawerGone();
  });

  it('opening a note closes it; a rekey of the open note does not', async () => {
    selectedNote.set(note('tmp:abc'));
    mountChrome();
    await openDrawer();
    // The open note's identity is forwarded (gotcha #16): still the same note.
    forwardNoteIdentity(ACCT, 'tmp:abc', 'real-1');
    selectedNote.set(note('real-1'));
    flushSync();
    expect(drawer()).not.toBeNull();
    expect(overlay()).toBe('drawer');
    // A different note opens (an Ask citation, a New note): the drawer goes.
    selectedNote.set(note('other'));
    flushSync();
    await expectDrawerGone();
  });

  it('the Sidebar\'s collapse ‹ closes the drawer', async () => {
    mountChrome();
    await openDrawer();
    click(host.querySelector('.tablet-drawer .stub-collapse'));
    await expectDrawerGone();
  });

  it('unmounting with the drawer open (rotation to phone) removes its entry', async () => {
    mountChrome();
    await openDrawer();
    unmountChrome();
    await vi.waitFor(() => expect(overlay()).toBeNull());
  });

  it('never persists the drawer: a fresh mount starts closed', async () => {
    mountChrome();
    await openDrawer();
    unmountChrome();
    await vi.waitFor(() => expect(overlay()).toBeNull());
    mountChrome();
    expect(drawer()).toBeNull();
  });
});

describe('wide tablet: persistent column', () => {
  beforeEach(() => layout.set('tabletWide'));

  it('tabletWide renders sidebar in flow without backdrop', () => {
    const before = history.length;
    mountChrome();
    expect(column()).not.toBeNull();
    const sb = column()!.querySelector<HTMLElement>('.stub-sidebar')!;
    expect(sb.style.width).toBe('260px');
    expect(scrim()).toBeNull();
    expect(drawer()).toBeNull();
    // A sibling of the bar and the panes, in the layout's own flow.
    expect(column()!.parentElement).toBe(host.querySelector('.tablet-layout'));
    expect(history.length).toBe(before);
    expect(overlay()).toBeNull();
  });

  it('☰ state persists', () => {
    mountChrome();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    click(toggle());
    expect(column()).toBeNull();
    expect(localStorage.getItem(OPEN_KEY)).toBe('false');
    expect(overlay()).toBeNull();
    unmountChrome();
    host.remove();
    mountChrome();
    expect(column()).toBeNull();
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    click(toggle());
    expect(column()).not.toBeNull();
    expect(localStorage.getItem(OPEN_KEY)).toBe('true');
  });

  it('defaults open when storage throws, and still toggles', () => {
    vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked'); });
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked'); });
    mountChrome();
    expect(column()).not.toBeNull();
    click(toggle());
    expect(column()).toBeNull();
  });

  it('a pick does not collapse the column', () => {
    mountChrome();
    click(host.querySelector('.tablet-column .stub-pick'));
    expect(column()).not.toBeNull();
    selectedNote.set(note('n1'));
    flushSync();
    expect(column()).not.toBeNull();
  });

  // Tab S7 (2026-10-21): ☰ and the column's own ‹ both toggled it, and the
  // column's header sat higher than the bar. The column is told it sits
  // beside the bar (Sidebar aligns its header and drops ‹); ☰ is the toggle.
  it('the column is a tablet column: header aligned with the bar, ☰ the one toggle', () => {
    mountChrome();
    expect(column()!.querySelector('.stub-sidebar')!.getAttribute('data-tablet-frame')).toBe('column');
  });
});

describe('rotation between tablet and tabletWide', () => {
  it('portrait → landscape with the drawer open: no dead entry, no scrim', async () => {
    mountChrome();
    await openDrawer();
    layout.set('tabletWide');
    flushSync();
    expect(scrim()).toBeNull();
    expect(drawer()).toBeNull();
    expect(column()).not.toBeNull();
    await vi.waitFor(() => expect(overlay()).toBeNull());
    // And back to portrait: still closed, still no entry.
    layout.set('tablet');
    flushSync();
    expect(drawer()).toBeNull();
    expect(scrim()).toBeNull();
    expect(column()).toBeNull();
    expect(overlay()).toBeNull();
  });

  it('landscape → portrait with the column open: no scrim, no entry; ☰ then opens the drawer', async () => {
    layout.set('tabletWide');
    mountChrome();
    expect(column()).not.toBeNull();
    layout.set('tablet');
    flushSync();
    expect(column()).toBeNull();
    expect(scrim()).toBeNull();
    expect(overlay()).toBeNull();
    await openDrawer();
    history.back();
    await vi.waitFor(() => expect(drawer()).toBeNull());
  });
});
