// @vitest-environment jsdom
//
// Task 6.3 (spec 2026-10-20 §6.4): typing `#` in the body lists the account's
// existing tags. ↑/↓ move, Enter/Tab replace the typed `#prefix` with
// `#tag ` as an edit command (so Cmd+Z restores the prefix), Esc closes, and
// a space or punctuation closes. Never on iCloud (gotcha #38), never in the
// title, and on a phone the list sits clear of the format bar.
//
// jsdom has no execCommand. The stub below edits the selection the way the
// browser does — insertText replaces what is selected, then the caret sits
// after the text — and keeps a snapshot per command so 'undo' takes the last
// one back, which is the property the real undo stack gives an edit command.
// (Real Chrome: the probe in task-6.3-report.md.)
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeAll, afterAll, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/viewport')>();
  return { ...orig, androidLayoutMode: writable('phone') };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';
import HashtagPicker from './HashtagPicker.svelte';
// Resolved from this file, wherever the tests run from.
import pickerSource from './HashtagPicker.svelte?raw';
import { isAndroid as isAndroidRO } from '../stores/platform';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import { accounts, currentAccount, notes, selectedNote, capabilitiesByAccount, noteTagsByAccount } from '../stores/notes';
import type { Note } from '../types';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const layout = layoutRO as unknown as Writable<string>;

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const ICLOUD = { id: 'icloud:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'icloud', status: 'active' };

function noteFor(accountId: string, body = '<div>first line</div><div>last line</div>'): Note {
  return {
    uuid: 'uuid-hashtag',
    id: 'msg-1',
    account_id: accountId,
    title: 'A note',
    body_html: body,
    date: '2026-10-20T00:00:00Z',
    label: 'Notes',
    local_version: 1,
  } as Note;
}

// ─── execCommand stub with an undo stack ─────────────────────────────────
const RECT = { left: 40, top: 100, right: 60, bottom: 118, width: 20, height: 18, x: 40, y: 100, toJSON() {} } as DOMRect;
let caretRect: DOMRect = RECT;
type Call = { cmd: string; arg?: string; selected: string };
let calls: Call[] = [];
let undo: string[] = [];

function placeCaret(node: Node, offset: number) {
  const r = document.createRange();
  r.setStart(node, offset);
  r.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(r);
}

beforeAll(() => {
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string, _ui?: boolean, arg?: string) => {
      const sel = window.getSelection()!;
      calls.push({ cmd, arg, selected: sel.rangeCount ? sel.toString() : '' });
      const el = document.querySelector<HTMLElement>('.editor-body');
      if (!el) return false;
      if (cmd === 'undo') {
        const html = undo.pop();
        if (html === undefined) return false;
        el.innerHTML = html;
        el.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      }
      if (cmd !== 'insertText' && cmd !== 'delete') return true;
      if (!sel.rangeCount) return false;
      undo.push(el.innerHTML);
      const r = sel.getRangeAt(0);
      if (!r.collapsed) r.deleteContents();
      if (cmd === 'insertText' && arg) {
        const node = r.startContainer;
        if (node.nodeType === 3) {
          (node as Text).insertData(r.startOffset, arg);
          placeCaret(node, r.startOffset + arg.length);
        } else {
          const t = document.createTextNode(arg);
          r.insertNode(t);
          placeCaret(t, t.data.length);
        }
      }
      el.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    },
  });
  (Selection.prototype as unknown as { modify: () => void }).modify = () => {};
  Range.prototype.getBoundingClientRect = () => caretRect;
  Object.defineProperty(document, 'queryCommandValue', { configurable: true, writable: true, value: () => 'div' });
});
afterAll(() => {
  delete (Range.prototype as unknown as Record<string, unknown>).getBoundingClientRect;
  delete (Selection.prototype as unknown as Record<string, unknown>).modify;
  delete (document as unknown as Record<string, unknown>).execCommand;
  delete (document as unknown as Record<string, unknown>).queryCommandValue;
});

let target: HTMLElement;
let app: ReturnType<typeof mount> | null = null;

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

/** Other notes in the account carry these tags; the vocabulary is alphabetical (tagsByAccount). */
function vocabulary(accountId: string, byNote: Record<string, string[]>) {
  noteTagsByAccount.set(new Map([[accountId, new Map(Object.entries(byNote))]]));
}

async function mountEditor(account = GMAIL, body?: string) {
  accounts.set([account as never]);
  currentAccount.set(account.id);
  const n = noteFor(account.id, body);
  notes.set([n]);
  selectedNote.set(n);
  app = mount(NoteEditor, { target });
  await settle();
}

const body = () => target.querySelector<HTMLElement>('.editor-body')!;
const picker = () => target.querySelector<HTMLElement>('[data-testid="hashtag-picker"]');
const options = () => Array.from(picker()?.querySelectorAll<HTMLButtonElement>('[role="option"]') ?? []).map((b) => b.dataset.tag);
const active = () => picker()?.querySelector<HTMLButtonElement>('[aria-selected="true"]')?.dataset.tag;

/** Focus the body with the caret at the end of its last text. */
function focusBodyEnd() {
  const el = body();
  el.focus();
  const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let last: Text | null = null;
  for (let n = w.nextNode() as Text | null; n; n = w.nextNode() as Text | null) last = n;
  if (last) placeCaret(last, last.data.length);
  else placeCaret(el, 0);
  flushSync();
}

/** Type one character at a time, as edit commands. */
function type(text: string) {
  for (const ch of text) document.execCommand('insertText', false, ch);
  flushSync();
}

function key(k: string, init: KeyboardEventInit = {}) {
  const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init });
  body().dispatchEvent(e);
  flushSync();
  return e;
}

beforeEach(() => {
  calls = [];
  undo = [];
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'note_persistence') return Promise.resolve(null);
    return Promise.resolve([]);
  });
  isAndroid.set(false);
  layout.set('phone');
  caretRect = RECT;
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.appendChild(target);
  vocabulary(GMAIL.id, { n1: ['study', 'stack'], n2: ['test', 'work'], n3: ['ภาษาไทย'] });
});

afterEach(async () => {
  capabilitiesByAccount.set({});
  noteTagsByAccount.set(new Map());
  if (app) unmount(app);
  app = null;
  target.remove();
  selectedNote.set(null);
  notes.set([]);
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('HashtagPicker', () => {
  it('renders nothing for an empty list, and a row per tag otherwise', () => {
    const onPick = vi.fn();
    app = mount(HashtagPicker, { target, props: { items: [], activeIndex: 0, position: { left: 1, top: 2 }, onPick } });
    flushSync();
    expect(picker()).toBeNull();
    unmount(app);
    app = mount(HashtagPicker, { target, props: { items: ['study', 'test'], activeIndex: 1, position: { left: 10, top: 20 }, onPick } });
    flushSync();
    expect(options()).toEqual(['study', 'test']);
    expect(active()).toBe('test');
    expect(picker()!.style.left).toBe('10px');
    expect(picker()!.style.top).toBe('20px');
  });

  it('a tap keeps focus where it was (pointerdown and mousedown cancelled) and picks on click', () => {
    const onPick = vi.fn();
    app = mount(HashtagPicker, { target, props: { items: ['study'], activeIndex: 0, position: { left: 0, top: 0 }, onPick } });
    flushSync();
    const b = picker()!.querySelector<HTMLButtonElement>('button')!;
    const pd = new Event('pointerdown', { bubbles: true, cancelable: true });
    b.dispatchEvent(pd);
    const md = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
    b.dispatchEvent(md);
    expect(pd.defaultPrevented).toBe(true);
    expect(md.defaultPrevented).toBe(true);
    b.click();
    expect(onPick).toHaveBeenCalledWith('study');
  });
});

describe('#-autocomplete in the body', () => {
  it('typing #st lists the account\'s tags, prefix matches first, at the caret', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    expect(options()).toEqual(['stack', 'study', 'test']);
    expect(active()).toBe('stack');
    expect(picker()!.style.left).toBe(`${RECT.left}px`);
    expect(picker()!.style.top).toBe(`${RECT.bottom + 2}px`);
    // No IPC of its own (platform_name is the platform store's).
    expect(invoke.mock.calls.filter(([c]) => c === 'search_note_links' || c === 'list_note_tags')).toEqual([]);
  });

  it('Enter replaces #stu with "#study " and Cmd+Z restores #stu', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #stu');
    expect(options()).toEqual(['study']);
    calls = [];
    const e = key('Enter');
    expect(e.defaultPrevented).toBe(true);
    // One edit command, over a selection spanning exactly the typed prefix.
    expect(calls).toEqual([{ cmd: 'insertText', arg: '#study ', selected: '#stu' }]);
    expect(body().textContent).toBe('first linelast line #study ');
    expect(picker()).toBeNull();
    expect(document.activeElement).toBe(body());
    // Undo (Ctrl+Z off macOS) is the editor's own handler → execCommand('undo').
    key('z', { code: 'KeyZ', ctrlKey: true });
    expect(calls.at(-1)?.cmd).toBe('undo');
    expect(body().textContent).toBe('first linelast line #stu');
  });

  it('↑/↓ move the highlight, Tab picks the highlighted tag', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    key('ArrowDown');
    expect(active()).toBe('study');
    key('ArrowDown');
    key('ArrowDown');
    expect(active()).toBe('stack'); // wraps
    key('ArrowUp');
    expect(active()).toBe('test');
    // A caret key's keyup re-reads the caret; the highlight stays.
    body().dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowUp', bubbles: true }));
    flushSync();
    expect(active()).toBe('test');
    key('Tab');
    expect(body().textContent).toBe('first linelast line #test ');
  });

  it('clicking a row picks it', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #wo');
    picker()!.querySelector<HTMLButtonElement>('[data-tag="work"]')!.click();
    flushSync();
    expect(body().textContent).toBe('first linelast line #work ');
    expect(picker()).toBeNull();
  });

  it('Esc closes it and the typed text stays; more typing on that # keeps it closed', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    expect(picker()).not.toBeNull();
    const e = key('Escape');
    expect(e.defaultPrevented).toBe(true);
    expect(picker()).toBeNull();
    expect(body().textContent).toBe('first linelast line #st');
    type('u');
    expect(picker()).toBeNull();
    // A new # is a new chance.
    type(' #wo');
    expect(options()).toEqual(['work']);
  });

  it('a space or punctuation closes it; Enter then is an ordinary Enter', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    expect(picker()).not.toBeNull();
    type(' ');
    expect(picker()).toBeNull();
    type('#st');
    expect(picker()).not.toBeNull();
    type('.');
    expect(picker()).toBeNull();
    calls = [];
    const e = key('Enter');
    expect(calls.find((c) => c.cmd === 'insertText')).toBeUndefined();
    expect(e.defaultPrevented).toBe(false);
  });

  it('no match shows nothing and leaves the typed text as typed', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #zzz');
    expect(picker()).toBeNull();
    const e = key('Enter');
    expect(e.defaultPrevented).toBe(false);
    expect(body().textContent).toBe('first linelast line #zzz');
  });

  it('does not suggest the half-typed tag itself (it reaches the tag store as it is typed)', async () => {
    vocabulary(GMAIL.id, { n1: ['work'] });
    await mountEditor();
    focusBodyEnd();
    type(' #wor');
    expect(options()).toEqual(['work']);
    type('kx');
    // `workx` is now this note's tag in the store, and nobody else's.
    expect(picker()).toBeNull();
  });

  it('Thai: #ภาษ lists ภาษาไทย and Enter inserts it (Review Focus 2)', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #ภาษ');
    expect(options()).toEqual(['ภาษาไทย']);
    key('Enter');
    expect(body().textContent).toBe('first linelast line #ภาษาไทย ');
  });

  it('not after a letter, not in code, not in a link, not in a URL', async () => {
    await mountEditor(GMAIL, '<div>word</div><div><code>x </code></div><div><a href="https://x.com">x </a></div><div>see https://x.com/</div>');
    const codeText = body().querySelector('code')!.firstChild as Text;
    body().focus();
    placeCaret(body().firstElementChild!.firstChild!, 4);
    type('#st');
    expect(picker()).toBeNull();
    placeCaret(codeText, codeText.data.length);
    type('#st');
    expect(picker()).toBeNull();
    const linkText = body().querySelector('a')!.firstChild as Text;
    placeCaret(linkText, linkText.data.length);
    type('#st');
    expect(picker()).toBeNull();
    const urlText = body().lastElementChild!.firstChild as Text;
    placeCaret(urlText, urlText.data.length);
    type('#st');
    expect(picker()).toBeNull();
  });

  it('iCloud: typing # opens no picker (gotcha #38)', async () => {
    vocabulary(ICLOUD.id, { n1: ['study', 'stack'] });
    await mountEditor(ICLOUD);
    focusBodyEnd();
    type(' #');
    expect(picker()).toBeNull();
    type('st');
    expect(picker()).toBeNull();
    const e = key('Enter');
    expect(calls.filter((c) => c.arg === '#study ')).toEqual([]);
    expect(e.defaultPrevented).toBe(false);
  });

  it('IME: Enter or ↓ while composing is the keyboard\'s, not the list\'s', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    calls = [];
    const e1 = key('Enter', { isComposing: true });
    const e2 = key('ArrowDown', { keyCode: 229 } as KeyboardEventInit);
    expect(e1.defaultPrevented).toBe(false);
    expect(e2.defaultPrevented).toBe(false);
    expect(calls).toEqual([]);
    expect(active()).toBe('stack');
    expect(body().textContent).toBe('first linelast line #st');
  });

  it('is not shown while the title has focus', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    expect(picker()).not.toBeNull();
    target.querySelector<HTMLInputElement>('.title-input')!.focus();
    flushSync();
    expect(picker()).toBeNull();
  });
});

describe('an existing tag is not typing a tag (fix round 1)', () => {
  it('a click or End at the end of an existing #work opens nothing; ↓ and Enter stay the editor\'s', async () => {
    await mountEditor(GMAIL, '<div>note #work</div>');
    const t = body().firstElementChild!.firstChild as Text;
    body().focus();
    placeCaret(t, t.data.length);
    body().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
    body().dispatchEvent(new KeyboardEvent('keyup', { key: 'End', bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
    calls = [];
    expect(key('ArrowDown').defaultPrevented).toBe(false);
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(calls.filter((c) => c.cmd === 'insertText')).toEqual([]);
  });

  it('a caret inside #wo|rk opens nothing, by caret key or by typing', async () => {
    await mountEditor(GMAIL, '<div>note #work</div>');
    const t = body().firstElementChild!.firstChild as Text;
    body().focus();
    placeCaret(t, 'note #wo'.length);
    body().dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
    type('r');
    expect(body().textContent).toBe('note #worrk');
    expect(picker()).toBeNull();
    expect(key('Enter').defaultPrevented).toBe(false);
  });

  it('a caret key while the list is open follows it, and closes it once inside a word', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    expect(picker()).not.toBeNull();
    const t = window.getSelection()!.getRangeAt(0).startContainer as Text;
    placeCaret(t, t.data.length - 1); // #s|t
    body().dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
  });

  // Final review 2026-10-20: an open list followed a click to ANOTHER tag,
  // and then owned Enter there (a prefix match picks) — the newline the user
  // typed was taken. A caret move keeps the list only on its own `#`.
  it('an open list does not follow a click or caret key to a different #tag', async () => {
    await mountEditor(GMAIL, '<div>note #wo</div>');
    focusBodyEnd();
    type(' #st');
    expect(options()).toContain('study');
    const t = window.getSelection()!.getRangeAt(0).startContainer as Text;
    expect(t.data).toBe('note #wo #st');
    placeCaret(t, 'note #wo'.length); // the end of the other tag, #wo|
    body().dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
    calls = [];
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(calls.filter((c) => c.cmd === 'insertText')).toEqual([]);

    // The same by caret key, from a list reopened on the new #st.
    placeCaret(t, t.data.length);
    type('a');
    expect(options()).toContain('stack');
    placeCaret(t, 'note #wo'.length);
    body().dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }));
    flushSync();
    expect(picker()).toBeNull();
  });

  it('reads the character before the # across inline elements: <span>hello</span>#st is no tag', async () => {
    await mountEditor(GMAIL, '<div><span>hello</span></div>');
    body().focus();
    placeCaret(body().firstElementChild!, 1);
    type('#st');
    expect(body().innerHTML).toContain('<span>hello</span>#st');
    expect(picker()).toBeNull();
    type(' #st');
    expect(options()).toEqual(['stack', 'study', 'test']);
  });

  it('a list whose #prefix was changed under it inserts nothing', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    const t = window.getSelection()!.getRangeAt(0).startContainer as Text;
    t.data = 'replaced by something else'; // no input event: the list did not see it
    calls = [];
    picker()!.querySelector<HTMLButtonElement>('[data-tag="study"]')!.click();
    flushSync();
    expect(calls.filter((c) => c.cmd === 'insertText')).toEqual([]);
    expect(body().textContent).toBe('first linereplaced by something else');
  });
});

describe('Enter and Tab pick only a likely tag (ruling R31)', () => {
  it('a bare # + Enter is a new line, even with the list showing', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #');
    expect(picker()).not.toBeNull();
    calls = [];
    expect(key('Enter').defaultPrevented).toBe(false);
    key('ArrowDown');
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(calls.filter((c) => c.cmd === 'insertText')).toEqual([]);
  });

  it('a substring-only top row picks on Enter only after ↑/↓', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #ud');
    expect(options()).toEqual(['study']);
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(calls.filter((c) => c.arg === '#study ')).toEqual([]);
    key('ArrowDown');
    expect(key('Enter').defaultPrevented).toBe(true);
    expect(body().textContent).toBe('first linelast line #study ');
  });

  it('Shift+Enter and Shift+Tab never pick', async () => {
    await mountEditor();
    focusBodyEnd();
    type(' #st');
    calls = [];
    key('Enter', { shiftKey: true });
    key('Tab', { shiftKey: true });
    expect(calls.filter((c) => c.cmd === 'insertText')).toEqual([]);
    expect(body().textContent).toBe('first linelast line #st');
  });
});

describe('caret lists name their active row to assistive tech', () => {
  it('the body points aria-activedescendant at the active tag row', async () => {
    await mountEditor();
    focusBodyEnd();
    expect(body().hasAttribute('aria-activedescendant')).toBe(false);
    type(' #st');
    const rows = picker()!.querySelectorAll<HTMLElement>('[role="option"]');
    expect(body().getAttribute('aria-activedescendant')).toBe(rows[0].id);
    key('ArrowDown');
    expect(body().getAttribute('aria-activedescendant')).toBe(rows[1].id);
    expect(rows[1].id).not.toBe(rows[0].id);
    key('Escape');
    expect(body().hasAttribute('aria-activedescendant')).toBe(false);
  });

  it('… and at the active [[ row', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'note_persistence') return Promise.resolve(null);
      if (cmd === 'search_note_links')
        return Promise.resolve([
          { uuid: 'u1', title: 'One', label: 'Notes', slug: 'One|u1' },
          { uuid: 'u2', title: 'Two', label: 'Notes', slug: 'Two|u2' },
        ]);
      return Promise.resolve([]);
    });
    await mountEditor();
    focusBodyEnd();
    type(' [[');
    await vi.waitFor(() => expect(target.querySelectorAll('.link-picker [role="option"]').length).toBe(2));
    flushSync();
    const rows = target.querySelectorAll<HTMLElement>('.link-picker [role="option"]');
    expect(target.querySelector('.link-picker')!.getAttribute('role')).toBe('listbox');
    expect(body().getAttribute('aria-activedescendant')).toBe(rows[0].id);
    key('ArrowDown');
    expect(body().getAttribute('aria-activedescendant')).toBe(rows[1].id);
    expect(rows[1].getAttribute('aria-selected')).toBe('true');
  });
});

describe('#-autocomplete on a phone', () => {
  const viewport = { w: window.innerWidth, h: window.innerHeight };
  beforeEach(() => {
    isAndroid.set(true);
    layout.set('phone');
  });
  afterEach(() => {
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: viewport.h });
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: viewport.w });
  });

  it('the format bar\'s # inserts # and opens the list above the bar, with 48px rows', async () => {
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 740 });
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 360 });
    await mountEditor(GMAIL, '<div>first line</div><div>last line </div>');
    focusBodyEnd();
    // The caret sits just above the bar: no room below for the list.
    caretRect = { left: 40, top: 660, right: 41, bottom: 680, width: 1, height: 20, x: 40, y: 660, toJSON() {} } as DOMRect;
    target.querySelector<HTMLButtonElement>('[data-testid="format-bar"] button[data-command="hashtag"]')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'insertText', arg: '#', selected: '' }]);
    expect(options()).toEqual(['stack', 'study', 'test', 'work', 'ภาษาไทย']);
    expect(picker()!.classList.contains('touch')).toBe(true);
    expect(pickerSource).toMatch(
      /\.touch \.opt \{\s*min-height: var\(--touch-target\);/,
    );
    // Anchored by its bottom edge just above the caret (caretPanelPosition),
    // clamped inside a 360px screen.
    expect(picker()!.style.bottom).toBe(`${740 - (660 - 4)}px`);
    expect(picker()!.style.top).toBe('');
    expect(picker()!.style.left).toBe('12px');
    expect(picker()!.style.maxHeight).toBe('240px');
    expect(document.activeElement).toBe(body());
  });

  it('the format bar\'s # right after a word puts a space first, so the list opens', async () => {
    await mountEditor();
    focusBodyEnd();
    target.querySelector<HTMLButtonElement>('[data-testid="format-bar"] button[data-command="hashtag"]')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'insertText', arg: ' #', selected: '' }]);
    expect(body().textContent).toBe('first linelast line #');
    expect(picker()).not.toBeNull();
  });

  it('… also when the caret sits on an element boundary after the word', async () => {
    await mountEditor();
    body().focus();
    placeCaret(body(), body().childNodes.length);
    flushSync();
    target.querySelector<HTMLButtonElement>('[data-testid="format-bar"] button[data-command="hashtag"]')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'insertText', arg: ' #', selected: '' }]);
    expect(picker()).not.toBeNull();
  });
});
