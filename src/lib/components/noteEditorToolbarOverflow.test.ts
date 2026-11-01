// @vitest-environment jsdom
//
// Task 6.4 (spec 2026-10-20 §5.6, last bullet): on desktop and tablet the
// editor toolbar is ONE row. The formatting buttons that do not fit move, from
// the right, into a "More formatting" popover instead of wrapping onto a second
// line. The save state and ⋯ on the right are reserved and never overflow.
//
// jsdom has no layout and no ResizeObserver: the policy is unit-tested as the
// pure `overflowCount`, and the component is driven through a stubbed
// ResizeObserver and stubbed widths (offsetWidth / clientWidth). "Never wraps"
// is asserted on the stylesheet, as the other size tests do; the real-Chrome
// check is tests/browser/toolbar-overflow.mjs.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readFileSync } from 'node:fs';
import { overflowCount } from '../editor/toolbarOverflow';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const NOTE: Note = {
  uuid: 'uuid-toolbar-overflow',
  id: 'msg-1',
  account_id: 'gmail:test@example.com',
  title: 'Overflow',
  body_html: '<div>body</div>',
  date: '2026-10-20T00:00:00Z',
  label: 'Notes',
} as Note;

describe('overflowCount', () => {
  it('counts the buttons that do not fit, from the right', () => {
    expect(overflowCount(300, Array(11).fill(30), 120)).toBe(5);
  });
  it('is 0 when everything fits', () => {
    expect(overflowCount(1000, Array(11).fill(30), 120)).toBe(0);
  });
});

describe('the rendered toolbar never wraps', () => {
  const css = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
  const rule = (sel: string) => {
    const m = css.match(new RegExp(`\\n  ${sel.replace('.', '\\.')} \\{([^}]*)\\}`));
    if (!m) throw new Error(`no ${sel} rule`);
    return m[1];
  };
  it('has flex-wrap: nowrap on the row and on the actions block', () => {
    expect(rule('.editor-toolbar')).toMatch(/flex-wrap:\s*nowrap/);
    expect(rule('.toolbar-actions')).toMatch(/flex-wrap:\s*nowrap/);
    expect(rule('.editor-toolbar')).not.toMatch(/flex-wrap:\s*wrap\b/);
  });
  // The overflow count is computed from each button's width: a button that
  // shrank or wrapped its label would measure narrower than it draws.
  it('a format button neither shrinks nor wraps its label', () => {
    expect(rule('.fmt-btn')).toMatch(/white-space:\s*nowrap/);
    expect(rule('.fmt-btn')).toMatch(/flex-shrink:\s*0/);
  });
});

// ─── stubs ────────────────────────────────────────────────────────────────
let observers: Array<{ cb: () => void; targets: Element[] }> = [];
class FakeResizeObserver {
  rec: { cb: () => void; targets: Element[] };
  constructor(cb: () => void) {
    this.rec = { cb, targets: [] };
    observers.push(this.rec);
  }
  observe(el: Element) { this.rec.targets.push(el); }
  unobserve() {}
  disconnect() { observers = observers.filter((o) => o !== this.rec); }
}

let toolbarWidth = 2000;
let rightWidth = 120;
const BTN = 30;
const proto = HTMLElement.prototype;
const origOffset = Object.getOwnPropertyDescriptor(proto, 'offsetWidth');
const origClient = Object.getOwnPropertyDescriptor(proto, 'clientWidth');

type Call = { cmd: string; arg?: string };
let calls: Call[] = [];

function resize(width: number) {
  toolbarWidth = width;
  for (const o of [...observers]) o.cb();
  flushSync();
}

function mountEditor() {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = mount(NoteEditor, { target: host });
  flushSync();
  return { host, app };
}

const rowButtons = (host: HTMLElement) =>
  Array.from(host.querySelectorAll<HTMLButtonElement>('.toolbar-actions > .fmt-btn[data-fmt]'));
const moreBtn = (host: HTMLElement) =>
  host.querySelector<HTMLButtonElement>('button[aria-label="More formatting"]');

describe('NoteEditor toolbar overflow', () => {
  let cleanup: (() => void) | null = null;

  beforeEach(() => {
    calls = [];
    observers = [];
    toolbarWidth = 2000;
    rightWidth = 120;
    vi.stubGlobal('ResizeObserver', FakeResizeObserver);
    Object.defineProperty(proto, 'offsetWidth', {
      configurable: true,
      get(this: HTMLElement) {
        if (this.classList.contains('toolbar-right')) return rightWidth;
        if (this.classList.contains('fmt-btn') && this.parentElement?.classList.contains('toolbar-actions')) return BTN;
        return 0;
      },
    });
    Object.defineProperty(proto, 'clientWidth', {
      configurable: true,
      get(this: HTMLElement) {
        return this.classList.contains('editor-toolbar') ? toolbarWidth : 0;
      },
    });
    (document as unknown as Record<string, unknown>).execCommand = (cmd: string, _ui?: boolean, arg?: string) => {
      calls.push({ cmd, arg });
      return true;
    };
    (document as unknown as Record<string, unknown>).queryCommandValue = () => '';
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      return Promise.resolve([]);
    });
    notes.set([NOTE]);
    selectedNote.set(NOTE);
  });

  afterEach(() => {
    cleanup?.();
    cleanup = null;
    document.body.innerHTML = '';
    selectedNote.set(null);
    notes.set([]);
    vi.unstubAllGlobals();
    if (origOffset) Object.defineProperty(proto, 'offsetWidth', origOffset);
    if (origClient) Object.defineProperty(proto, 'clientWidth', origClient);
    delete (document as unknown as Record<string, unknown>).execCommand;
    delete (document as unknown as Record<string, unknown>).queryCommandValue;
    vi.clearAllMocks();
  });

  async function setup() {
    const m = mountEditor();
    cleanup = () => unmount(m.app);
    await tick();
    flushSync();
    return m.host;
  }

  it('shows every formatting button and no More button when the row is wide', async () => {
    const host = await setup();
    expect(rowButtons(host)).toHaveLength(11);
    expect(host.querySelectorAll('.fmt-overflowed')).toHaveLength(0);
    expect(moreBtn(host)).toBeNull();
  });

  it('moves the buttons that do not fit, from the right, behind a More formatting button', async () => {
    const host = await setup();
    // 300 wide, 120 reserved, a 30px More button: 150px of buttons -> 5 fit.
    resize(300);
    const buttons = rowButtons(host);
    const hidden = buttons.filter((b) => b.classList.contains('fmt-overflowed'));
    expect(hidden).toHaveLength(6);
    // A suffix: the first five stay, the last six moved.
    expect(buttons.slice(0, 5).every((b) => !b.classList.contains('fmt-overflowed'))).toBe(true);
    expect(buttons.slice(5).every((b) => b.classList.contains('fmt-overflowed'))).toBe(true);
    // Moved buttons are out of the tab order and the accessibility tree.
    expect(hidden.every((b) => (b as HTMLElement & { inert: boolean }).inert === true)).toBe(true);
    const more = moreBtn(host);
    expect(more).not.toBeNull();
    expect(more!.getAttribute('aria-haspopup')).toBe('menu');
    expect(more!.getAttribute('aria-expanded')).toBe('false');
    // Not the Note actions ⋯ beside it: two identical ⋯ side by side would
    // not say which one holds the formatting.
    const actions = host.querySelector('button[aria-label="Note actions"]')!;
    expect(more!.querySelector('svg')!.innerHTML).not.toBe(actions.querySelector('svg')!.innerHTML);
  });

  it('puts the moved buttons in the menu, same handlers, and keeps the selection in the editor', async () => {
    const host = await setup();
    resize(300);
    moreBtn(host)!.click();
    flushSync();
    const menu = document.querySelector('[data-testid="more-formatting-menu"]')!;
    expect(menu).not.toBeNull();
    const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('button'));
    expect(items.map((b) => b.getAttribute('aria-label'))).toEqual([
      expect.stringMatching(/^Heading 2/),
      expect.stringMatching(/^Blockquote/),
      expect.stringMatching(/^Inline code/),
      expect.stringMatching(/^Bullet List/),
      expect.stringMatching(/^Numbered List/),
      expect.stringMatching(/^Task \/ Checklist/),
    ]);
    // Like the row's buttons, a menu item must not take focus from the
    // editor on mousedown, or the format would apply to nothing.
    for (const b of [items[0], rowButtons(host)[0]]) {
      const ev = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
      b.dispatchEvent(ev);
      expect(ev.defaultPrevented).toBe(true);
    }
    const bullets = items.find((b) => b.getAttribute('aria-label')!.startsWith('Bullet List'))!;
    bullets.click();
    await tick();
    flushSync();
    expect(calls).toContainEqual({ cmd: 'insertUnorderedList', arg: undefined });
    // The pick closes the menu.
    expect(document.querySelector('[data-testid="more-formatting-menu"]')).toBeNull();
    expect(moreBtn(host)!.getAttribute('aria-expanded')).toBe('false');
  });

  it('grows back: widening the row retires the More button and shows every button', async () => {
    const host = await setup();
    resize(300);
    expect(moreBtn(host)).not.toBeNull();
    resize(2000);
    expect(moreBtn(host)).toBeNull();
    expect(host.querySelectorAll('.fmt-overflowed')).toHaveLength(0);
  });

  it('never hides the reserved right side, however narrow', async () => {
    const host = await setup();
    resize(100); // narrower than the reserved 120
    expect(rowButtons(host).every((b) => b.classList.contains('fmt-overflowed'))).toBe(true);
    expect(moreBtn(host)).not.toBeNull();
    expect(host.querySelector('.toolbar-right')).not.toBeNull();
    expect(host.querySelector('button[aria-label="Note actions"]')).not.toBeNull();
  });

  it('ignores a toolbar that is not laid out instead of hiding everything', async () => {
    const host = await setup();
    resize(300);
    resize(0); // display:none ancestor: clientWidth 0 — keep the last reading
    expect(rowButtons(host).filter((b) => b.classList.contains('fmt-overflowed'))).toHaveLength(6);
  });

  it('reflows when the save text widens the reserved side', async () => {
    const host = await setup();
    resize(400); // 280px for buttons: some overflow already
    const before = rowButtons(host).filter((b) => b.classList.contains('fmt-overflowed')).length;
    rightWidth = 220;
    resize(400);
    const after = rowButtons(host).filter((b) => b.classList.contains('fmt-overflowed')).length;
    expect(after).toBeGreaterThan(before);
  });
});
