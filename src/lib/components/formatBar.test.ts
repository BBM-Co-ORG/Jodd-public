// @vitest-environment jsdom
//
// Task 6.1 (spec 2026-10-20 §5.6): on an Android phone the desktop toolbar row
// does not render — it wrapped onto 2–3 rows of ~22px buttons — and a single
// scrollable 48px format bar sits above the soft keyboard while the title or
// the body has focus. The bar's buttons call NoteEditor's existing format
// handlers (passed in as `commands`); nothing is reimplemented.
//
// jsdom has no execCommand and no CSS engine. execCommand is stubbed to record
// each call (insertText also inserts, so `[[` reaches the real picker); size
// rules are asserted on the stylesheets, as the other touch-size tests do.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeAll, afterAll, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';
import { readFileSync } from 'node:fs';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(true) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/viewport')>();
  return { ...orig, androidLayoutMode: writable('phone'), keyboardInset: writable(0) };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import FormatBar, { FORMAT_COMMANDS, type FormatCommand } from './FormatBar.svelte';
import NoteEditor from './NoteEditor.svelte';
import { isAndroid as isAndroidRO } from '../stores/platform';
import { androidLayoutMode as layoutRO, keyboardInset as keyboardInsetRO } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import { accounts, currentAccount, notes, selectedNote, setAccountCapabilities, capabilitiesByAccount } from '../stores/notes';
import { editorExternalChange } from '../stores/editorSaveState';
import type { Note } from '../types';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const layout = layoutRO as unknown as Writable<string>;
const keyboardInset = keyboardInsetRO as unknown as Writable<number>;

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const NOTE: Note = {
  uuid: 'uuid-format-bar',
  id: 'msg-1',
  account_id: ACCOUNT.id,
  title: 'A long note',
  body_html: '<div>first line</div><div>last line</div>',
  date: '2026-10-20T00:00:00Z',
  label: 'Notes',
  local_version: 1,
} as Note;

// ─── execCommand stub ─────────────────────────────────────────────────────
const ZERO_RECT = { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0, x: 0, y: 0, toJSON() {} } as DOMRect;
let caretRect: DOMRect = ZERO_RECT;
type Call = { cmd: string; arg?: string };
let calls: Call[] = [];
beforeAll(() => {
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string, _ui?: boolean, arg?: string) => {
      calls.push({ cmd, arg });
      if (cmd === 'insertText' && arg) {
        const sel = window.getSelection();
        if (!sel || !sel.rangeCount) return false;
        const r = sel.getRangeAt(0);
        const t = document.createTextNode(arg);
        r.insertNode(t);
        const after = document.createRange();
        after.setStart(t, t.data.length);
        after.collapse(true);
        sel.removeAllRanges();
        sel.addRange(after);
        (t.parentElement?.closest('[contenteditable]') ?? t.parentElement)?.dispatchEvent(
          new Event('input', { bubbles: true }),
        );
      }
      return true;
    },
  });
  // jsdom has no layout: the [[ picker positions itself from the caret rect.
  // jsdom has no Selection.modify (insertChecklist moves to the line start):
  // a caret already at a line's end is enough for these tests.
  (Selection.prototype as unknown as { modify: () => void }).modify = () => {};
  // Tests set `caretRect` to put the caret somewhere on screen.
  Range.prototype.getBoundingClientRect = () => caretRect;
  Object.defineProperty(document, 'queryCommandValue', {
    configurable: true,
    writable: true,
    value: () => 'div',
  });
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

async function mountEditor() {
  accounts.set([ACCOUNT as never]);
  currentAccount.set(ACCOUNT.id);
  notes.set([NOTE]);
  selectedNote.set(NOTE);
  app = mount(NoteEditor, { target });
  await settle();
}

const bar = () => target.querySelector<HTMLElement>('[data-testid="format-bar"]');
const body = () => target.querySelector<HTMLElement>('.editor-body')!;
const titleInput = () => target.querySelector<HTMLInputElement>('.title-input')!;
const btn = (c: FormatCommand) => bar()?.querySelector<HTMLButtonElement>(`button[data-command="${c}"]`) ?? null;

/** Focus the body with the caret at the end of its last text. */
function focusBody() {
  const el = body();
  el.focus();
  const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let last: Text | null = null;
  for (let n = w.nextNode() as Text | null; n; n = w.nextNode() as Text | null) last = n;
  const r = document.createRange();
  if (last) r.setStart(last, last.data.length);
  else r.setStart(el, 0);
  r.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(r);
  flushSync();
}

beforeEach(() => {
  calls = [];
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'note_persistence') return Promise.resolve(null);
    return Promise.resolve([]);
  });
  isAndroid.set(true);
  layout.set('phone');
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(async () => {
  capabilitiesByAccount.set({});
  caretRect = ZERO_RECT;
  if (app) unmount(app);
  app = null;
  target.remove();
  selectedNote.set(null);
  notes.set([]);
  // A phone sheet pops its history entry asynchronously on unmount (R8).
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

// ─── FormatBar on its own ─────────────────────────────────────────────────

describe('FormatBar', () => {
  function spies() {
    return Object.fromEntries(FORMAT_COMMANDS.map((c) => [c, vi.fn<() => void>()])) as Record<FormatCommand, ReturnType<typeof vi.fn<() => void>>>;
  }

  it('renders one button per command, in the spec order, each calling its command', () => {
    const commands = spies();
    app = mount(FormatBar, { target, props: { commands } });
    flushSync();
    expect(FORMAT_COMMANDS).toEqual([
      'heading', 'bold', 'italic', 'underline', 'strike', 'bullet', 'numbered', 'task',
      'quote', 'code', 'link', 'hashtag', 'wikilink', 'dismiss',
    ]);
    const buttons = Array.from(target.querySelectorAll<HTMLButtonElement>('[data-testid="format-bar"] button'));
    expect(buttons.map((b) => b.dataset.command)).toEqual(FORMAT_COMMANDS);
    for (const b of buttons) {
      expect(b.getAttribute('aria-label')).toBeTruthy();
      expect(b.type).toBe('button');
    }
    for (const c of FORMAT_COMMANDS) {
      target.querySelector<HTMLButtonElement>(`button[data-command="${c}"]`)!.click();
      expect(commands[c]).toHaveBeenCalledTimes(1);
    }
    // No IPC of its own (platform_name is the platform store's, not ours).
    expect(invoke.mock.calls.filter(([cmd]) => cmd !== 'platform_name')).toEqual([]);
  });

  it('buttons are 48px, the bar is pinned above the keyboard and scrolls sideways', () => {
    const src = readFileSync('src/lib/components/FormatBar.svelte', 'utf8');
    const style = src.slice(src.indexOf('<style>'));
    const rule = (sel: string) => {
      const i = style.indexOf(`${sel} {`);
      expect(i, `${sel} rule`).toBeGreaterThanOrEqual(0);
      return style.slice(i, style.indexOf('}', i));
    };
    const b = rule('.format-bar button');
    expect(b).toMatch(/width:\s*var\(--touch-target\)/);
    expect(b).toMatch(/height:\s*var\(--touch-target\)/);
    expect(b).toMatch(/flex:\s*0 0 auto/);
    const root = rule('.format-bar');
    expect(root).toMatch(/position:\s*fixed/);
    // Above the keyboard; with no keyboard, above the navigation bar (live
    // pass 2026-10-21: its buttons overlapped the system buttons).
    expect(root).toMatch(/bottom:\s*max\(var\(--kb-inset, 0px\), env\(safe-area-inset-bottom\)\)/);
    expect(root).toMatch(/height:\s*var\(--touch-target\)/);
    expect(root).toMatch(/overflow-x:\s*auto/);
    expect(root).toMatch(/z-index:\s*var\(--z-sticky\)/);
    // New files use tokens only: no raw px in padding, margin or gap.
    expect(style).not.toMatch(/(padding|margin|gap)[^;]*\d+px/);
  });

  it('pointerdown and mousedown are cancelled, so the caret stays in the editor', () => {
    app = mount(FormatBar, { target, props: { commands: spies() } });
    flushSync();
    const b = target.querySelector<HTMLButtonElement>('button[data-command="bold"]')!;
    const down = new Event('pointerdown', { bubbles: true, cancelable: true });
    b.dispatchEvent(down);
    expect(down.defaultPrevented).toBe(true);
    const mouse = new MouseEvent('mousedown', { bubbles: true, cancelable: true });
    b.dispatchEvent(mouse);
    expect(mouse.defaultPrevented).toBe(true);
  });
});

// ─── NoteEditor on phone ──────────────────────────────────────────────────

describe('NoteEditor format bar — phone', () => {
  it('phone: toolbar row absent, bar shows on body focus, hides on blur', async () => {
    await mountEditor();
    expect(target.querySelector('.editor-toolbar')).toBeNull();
    expect(bar()).toBeNull();

    focusBody();
    expect(document.activeElement).toBe(body());
    expect(bar()).not.toBeNull();

    body().blur();
    flushSync();
    expect(bar()).toBeNull();
  });

  it('the title shows it too', async () => {
    await mountEditor();
    titleInput().focus();
    flushSync();
    expect(bar()).not.toBeNull();
    titleInput().blur();
    flushSync();
    expect(bar()).toBeNull();
  });

  it('buttons are 48px', async () => {
    await mountEditor();
    focusBody();
    expect(bar()!.querySelectorAll('button').length).toBe(FORMAT_COMMANDS.length);
    const src = readFileSync('src/lib/components/FormatBar.svelte', 'utf8');
    const style = src.slice(src.indexOf('<style>'));
    expect(style).toMatch(/\.format-bar button \{[^}]*width:\s*var\(--touch-target\)[^}]*height:\s*var\(--touch-target\)/);
  });

  it('pointerdown does not blur the editor', async () => {
    await mountEditor();
    focusBody();
    const down = new Event('pointerdown', { bubbles: true, cancelable: true });
    btn('bold')!.dispatchEvent(down);
    flushSync();
    expect(down.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(body());
    btn('bold')!.click();
    flushSync();
    expect(calls).toContainEqual({ cmd: 'bold', arg: undefined });
    expect(document.activeElement).toBe(body());
    expect(bar()).not.toBeNull();
  });

  it('formatting buttons call the toolbar\'s own commands', async () => {
    await mountEditor();
    focusBody();
    const expected: [FormatCommand, string, string?][] = [
      ['bold', 'bold'],
      ['italic', 'italic'],
      ['underline', 'underline'],
      ['strike', 'strikeThrough'],
      ['bullet', 'insertUnorderedList'],
      ['numbered', 'insertOrderedList'],
      ['quote', 'formatBlock', '<blockquote>'],
      ['task', 'insertHTML', '<input type="checkbox">&nbsp;'],
      ['code', 'insertHTML', '<code>\u200b</code>'],
    ];
    for (const [c, cmd, arg] of expected) {
      calls = [];
      btn(c)!.click();
      flushSync();
      expect(calls[0], c).toEqual({ cmd, arg });
    }
    // Link is the toolbar's own link prompt (formatLink), not a command.
    calls = [];
    expect(target.querySelector('.link-prompt')).toBeNull();
    btn('link')!.click();
    flushSync();
    expect(target.querySelector('.link-prompt .link-prompt-url')).not.toBeNull();
    expect(calls).toEqual([]);
  });

  it('with the title focused, body-only buttons are disabled and do nothing; Hide keyboard works', async () => {
    await mountEditor();
    titleInput().focus();
    flushSync();
    const before = titleInput().value;
    for (const c of FORMAT_COMMANDS.filter((c) => c !== 'dismiss')) {
      expect(btn(c)!.getAttribute('aria-disabled'), c).toBe('true');
      const down = new Event('pointerdown', { bubbles: true, cancelable: true });
      btn(c)!.dispatchEvent(down);
      expect(down.defaultPrevented, c).toBe(true); // still keeps the caret where it is
      btn(c)!.click();
      flushSync();
    }
    expect(calls).toEqual([]);
    expect(titleInput().value).toBe(before);
    expect(document.querySelector('dialog[data-presentation="sheet"]')).toBeNull();
    expect(target.querySelector('.link-prompt')).toBeNull();
    expect(document.activeElement).toBe(titleInput());
    expect(btn('dismiss')!.getAttribute('aria-disabled')).toBeNull();
    btn('dismiss')!.click();
    flushSync();
    expect(document.activeElement).not.toBe(titleInput());
    expect(bar()).toBeNull();
  });

  // Gotcha #38: on iCloud a tag is an object Jodd cannot create, so `#`
  // would only type text Apple never treats as a tag.
  it('iCloud: # is disabled in the body and types nothing; the rest still work', async () => {
    const ICLOUD = { id: 'icloud:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'icloud', status: 'active' };
    accounts.set([ICLOUD as never]);
    currentAccount.set(ICLOUD.id);
    const note = { ...NOTE, uuid: 'uuid-format-bar-icloud', account_id: ICLOUD.id } as Note;
    notes.set([note]);
    selectedNote.set(note);
    app = mount(NoteEditor, { target });
    await settle();
    focusBody();
    flushSync();
    expect(btn('hashtag')!.getAttribute('aria-disabled')).toBe('true');
    calls = [];
    btn('hashtag')!.click();
    flushSync();
    expect(calls).toEqual([]);
    expect(body().textContent).not.toContain('#');
    expect(btn('bold')!.getAttribute('aria-disabled')).toBeNull();
    expect(btn('wikilink')!.getAttribute('aria-disabled')).toBeNull();
  });

  it('Gmail: # is enabled in the body', async () => {
    await mountEditor();
    focusBody();
    flushSync();
    expect(btn('hashtag')!.getAttribute('aria-disabled')).toBeNull();
  });

  it('moving from the title to the body enables them again', async () => {
    await mountEditor();
    titleInput().focus();
    flushSync();
    expect(btn('bold')!.getAttribute('aria-disabled')).toBe('true');
    focusBody();
    expect(btn('bold')!.getAttribute('aria-disabled')).toBeNull();
    btn('bold')!.click();
    expect(calls).toEqual([{ cmd: 'bold', arg: undefined }]);
  });

  // Live pass 2026-10-21 (S23 FE): Android Back hides the keyboard but
  // leaves the caret in the body — no blur — and the bar stayed, at the very
  // bottom under the navigation bar.
  it('the keyboard closing under a focused body hides the bar; it returns with the keyboard', async () => {
    keyboardInset.set(0);
    await mountEditor();
    focusBody();
    keyboardInset.set(300); // the keyboard is up
    flushSync();
    expect(bar()).not.toBeNull();
    keyboardInset.set(0); // Android Back: keyboard down, caret still in the body
    flushSync();
    expect(document.activeElement).toBe(body());
    expect(bar()).toBeNull();
    // The pane's own bottom inset is back, so its last row clears the nav bar.
    expect(target.querySelector('.editor-pane')!.classList.contains('format-bar-shown')).toBe(false);
    keyboardInset.set(300); // a tap in the body brings the keyboard back
    flushSync();
    expect(bar()).not.toBeNull();
    keyboardInset.set(0);
  });

  it('focusing again after the keyboard closed shows the bar', async () => {
    keyboardInset.set(0);
    await mountEditor();
    focusBody();
    keyboardInset.set(300);
    flushSync();
    keyboardInset.set(0);
    flushSync();
    expect(bar()).toBeNull();
    titleInput().focus();
    flushSync();
    expect(bar()).not.toBeNull();
    titleInput().blur();
  });

  it('dismiss blurs the editor and the bar goes', async () => {
    await mountEditor();
    focusBody();
    btn('dismiss')!.click();
    flushSync();
    expect(document.activeElement).not.toBe(body());
    expect(bar()).toBeNull();
  });

  // Task 6.3: the # it types opens the tag list (hashtagPicker.test.ts), so
  // right after a word it types " #" — `line#` would not start a tag.
  it('# inserts "#" at the caret, with a space first right after a word', async () => {
    await mountEditor();
    focusBody();
    btn('hashtag')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'insertText', arg: ' #' }]);
    expect(body().textContent).toContain('last line #');
  });

  it('[[ opens the existing wikilink picker', async () => {
    await mountEditor();
    focusBody();
    btn('wikilink')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'insertText', arg: '[[' }]);
    await vi.waitFor(() =>
      expect(invoke.mock.calls.find(([cmd]) => cmd === 'search_note_links')).toBeTruthy(),
    );
    const [, args] = invoke.mock.calls.find(([cmd]) => cmd === 'search_note_links')!;
    expect(args).toEqual({ accountId: ACCOUNT.id, query: '' });
  });

  it('Aa opens a sheet with Title / Heading / Subheading / Body; a pick applies after it closes', async () => {
    await mountEditor();
    focusBody();
    btn('heading')!.click();
    flushSync();
    const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    const options = Array.from(sheet()!.querySelectorAll<HTMLButtonElement>('button.text-style'));
    expect(options.map((b) => b.textContent?.trim())).toEqual(['Title', 'Heading', 'Subheading', 'Body']);
    // Nothing applied while the sheet is up: the editor behind it is inert.
    expect(calls).toEqual([]);
    options[2].click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(calls).toContainEqual({ cmd: 'formatBlock', arg: '<h3>' }));
    expect(document.activeElement).toBe(body());
    // The caret is back in the body, where it was.
    expect(body().contains(window.getSelection()!.getRangeAt(0).startContainer)).toBe(true);
  });

  it('closing the sheet without a pick changes nothing', async () => {
    await mountEditor();
    focusBody();
    btn('heading')!.click();
    flushSync();
    const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    sheet()!.querySelector<HTMLButtonElement>('button.close')!.click();
    flushSync();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await settle();
    expect(calls.filter((c) => c.cmd === 'formatBlock')).toEqual([]);
  });

  it('the editor scroller keeps the caret line above the bar', () => {
    const src = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
    const style = src.slice(src.lastIndexOf('<style>'));
    expect(style).toMatch(/scroll-padding-bottom:\s*calc\(var\(--touch-target\) \+ var\(--kb-inset, 0px\)\)/);
  });

  it('a read-only note shows no bar', async () => {
    setAccountCapabilities(ACCOUNT.id, {
      has_trash: true,
      writes: { notes: false, relocate: false, folders: false, sidecars: false },
    } as never);
    await mountEditor();
    titleInput().focus();
    flushSync();
    expect(bar()).toBeNull();
  });

  // A role=textbox that cannot be typed into must say so (final review
  // 2026-10-20): a screen reader otherwise announces an editable field.
  it('the body is aria-readonly exactly when the note is read-only', async () => {
    await mountEditor();
    expect(body().getAttribute('role')).toBe('textbox');
    expect(body().getAttribute('aria-readonly')).toBeNull();
    unmount(app!);
    app = null;
    setAccountCapabilities(ACCOUNT.id, {
      has_trash: true,
      writes: { notes: false, relocate: false, folders: false, sidecars: false },
    } as never);
    await mountEditor();
    expect(body().getAttribute('contenteditable')).toBe('false');
    expect(body().getAttribute('aria-readonly')).toBe('true');
  });

  it('[[ near the bottom opens the picker above the caret and above the bar', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'search_note_links') {
        return Promise.resolve([{ uuid: 'other', title: 'Other note', label: 'Notes', slug: 'Other note--other' }]);
      }
      return Promise.resolve([]);
    });
    await mountEditor();
    focusBody();
    // The caret line sits just above the bar (scroll-padding put it there).
    const h = window.innerHeight;
    const caretTop = h - 48 - 30;
    caretRect = { left: 40, top: caretTop, right: 40, bottom: caretTop + 20, width: 0, height: 20, x: 40, y: caretTop, toJSON() {} } as DOMRect;
    btn('wikilink')!.click();
    flushSync();
    await vi.waitFor(() => expect(target.querySelector('.link-picker')).not.toBeNull());
    const style = target.querySelector<HTMLElement>('.link-picker')!.style;
    expect(style.top).toBe('');
    const bottomEdge = h - parseFloat(style.bottom);
    expect(bottomEdge).toBeLessThanOrEqual(caretTop);
    expect(bottomEdge).toBeLessThanOrEqual(h - 48);
    expect(parseFloat(style.maxHeight)).toBeGreaterThan(0);
    expect(parseFloat(style.maxHeight)).toBeLessThanOrEqual(caretTop);
  });

  it('[[ high up on the phone opens the picker below the caret, inside the visible area', async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'search_note_links') {
        return Promise.resolve([{ uuid: 'other', title: 'Other note', label: 'Notes', slug: 'Other note--other' }]);
      }
      return Promise.resolve([]);
    });
    await mountEditor();
    focusBody();
    caretRect = { left: 40, top: 100, right: 40, bottom: 120, width: 0, height: 20, x: 40, y: 100, toJSON() {} } as DOMRect;
    btn('wikilink')!.click();
    flushSync();
    await vi.waitFor(() => expect(target.querySelector('.link-picker')).not.toBeNull());
    const style = target.querySelector<HTMLElement>('.link-picker')!.style;
    expect(parseFloat(style.top)).toBeGreaterThanOrEqual(120);
    expect(parseFloat(style.top) + parseFloat(style.maxHeight)).toBeLessThanOrEqual(window.innerHeight - 48);
  });
});

// ─── not phone ─────────────────────────────────────────────────────────────

describe('NoteEditor format bar — not phone', () => {
  it('desktop: no FormatBar', async () => {
    isAndroid.set(false);
    layout.set('phone'); // a narrow desktop window never gets the phone layout
    await mountEditor();
    expect(target.querySelector('.editor-toolbar')).not.toBeNull();
    focusBody();
    expect(bar()).toBeNull();
  });

  it('tablet: the toolbar row, no FormatBar', async () => {
    layout.set('tablet');
    await mountEditor();
    expect(target.querySelector('.editor-toolbar')).not.toBeNull();
    focusBody();
    expect(bar()).toBeNull();
  });
});

// ─── A long title wraps on phone ──────────────────────────────────────────
// Live pass 2026-10-21 (S23 FE): "6 Things You Should Never A" — an <input>
// clips a long title on one line with no ellipsis. On phone the title is a
// one-row textarea that grows with its text; it still holds one line.

// Tab S7 live pass of v0.35.2 (2026-10-21): the tablet title was cut off on
// one line ("…for basically fre") in both orientations. Every layout wraps.
describe('NoteEditor title — wraps on every layout', () => {
  it('phone: a textarea that grows with its text', async () => {
    await mountEditor();
    expect(titleInput().tagName).toBe('TEXTAREA');
    expect(titleInput().getAttribute('rows')).toBe('1');
    expect(titleInput().value).toBe(NOTE.title);
    const src = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
    const css = src.slice(src.indexOf('<style>'));
    const i = css.indexOf('textarea.title-input {');
    expect(i, 'textarea.title-input rule').toBeGreaterThanOrEqual(0);
    const rule = css.slice(i, css.indexOf('}', i));
    expect(rule).toMatch(/field-sizing:\s*content/);
    expect(rule).toMatch(/resize:\s*none/);
  });

  it('tablet, wide tablet and desktop: the same wrapping textarea', async () => {
    for (const [android, mode] of [[true, 'tablet'], [true, 'tabletWide'], [false, 'phone']] as const) {
      isAndroid.set(android);
      layout.set(mode);
      await mountEditor();
      expect(titleInput().tagName, `${android ? 'android' : 'desktop'} ${mode}`).toBe('TEXTAREA');
      expect(titleInput().value).toBe(NOTE.title);
      unmount(app!);
      app = null;
    }
  });

  it('desktop: Enter moves to the body, and a pasted line break becomes a space', async () => {
    isAndroid.set(false);
    await mountEditor();
    const t = titleInput();
    t.focus();
    const enter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    t.dispatchEvent(enter);
    flushSync();
    expect(enter.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(body());
    t.value = 'Two\nlines';
    t.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    expect(t.value).toBe('Two lines');
  });

  // A webview without field-sizing (macOS before Safari 26's WebKit) would
  // leave a rows=1 textarea one line tall with the rest hidden: size it from
  // its scrollHeight instead.
  it('without field-sizing support, the height follows the text', async () => {
    const supports = globalThis.CSS?.supports;
    const desc = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'scrollHeight');
    Object.defineProperty(HTMLTextAreaElement.prototype, 'scrollHeight', { configurable: true, get() { return this.value.length > 20 ? 120 : 60; } });
    try {
      if (globalThis.CSS) globalThis.CSS.supports = (() => false) as typeof CSS.supports;
      isAndroid.set(false);
      await mountEditor();
      const t = titleInput();
      const short = parseFloat(t.style.height);
      expect(short).toBeGreaterThan(0);
      t.value = 'A title long enough to wrap onto a second line';
      t.dispatchEvent(new Event('input', { bubbles: true }));
      flushSync();
      expect(parseFloat(t.style.height)).toBeGreaterThan(short);
    } finally {
      if (globalThis.CSS && supports) globalThis.CSS.supports = supports;
      if (desc) Object.defineProperty(HTMLTextAreaElement.prototype, 'scrollHeight', desc);
      else delete (HTMLTextAreaElement.prototype as { scrollHeight?: number }).scrollHeight;
    }
  });

  it('Enter moves to the body, and a pasted line break becomes a space', async () => {
    await mountEditor();
    const t = titleInput();
    t.focus();
    const enter = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true });
    t.dispatchEvent(enter);
    flushSync();
    expect(enter.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(body());
    t.focus();
    t.value = 'Two\nlines\r\nhere';
    t.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    expect(t.value).toBe('Two lines here');
    t.blur();
  });
});

// ─── "Edited on another device" reaches the phone header ──────────────────

describe('external change, published for the phone header', () => {
  it('mirrors the banner, and its reload clears it', async () => {
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    await mountEditor();
    expect(get(editorExternalChange)).toBeNull();
    // Unsaved typing here, then a newer body from elsewhere.
    body().innerHTML = '<div>typed here</div>';
    body().dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    selectedNote.set({ ...get(selectedNote)!, body_html: '<div>edited in Apple Notes</div>' });
    flushSync();
    await settle();
    expect(get(editorExternalChange)).not.toBeNull();
    get(editorExternalChange)!.reload();
    flushSync();
    await settle();
    expect(get(editorExternalChange)).toBeNull();
  });
});
