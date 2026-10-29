// @vitest-environment jsdom
//
// Task 9 of the hypermedia-links plan (spec 2026-10-22-hypermedia-links-design.md),
// mounted: every link in the body — `<a>`, plain-text URL, `[[wikilink]]`,
// `#meeting-…` anchor — opens through one router. A plain click opens the
// chip; Cmd/Ctrl+click follows the link at once; nothing ever changes the note.
//
// jsdom has no layout: `Range.prototype.getClientRects` is stubbed so the span
// under test reports one rect around the click point, and requestAnimationFrame
// runs on `vi.advanceTimersByTime(16)`.
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import * as stores from '../stores/notes';
import { persistenceByNote, pendingLocalEdits, savingNoteKeys } from '../notePersistence';
import { clearNoteAliases } from '../noteIdentity';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const openUrl = vi.fn();
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: (...a: unknown[]) => openUrl(...a) }));
const scanSpy = vi.hoisted(() => ({ calls: 0 }));
vi.mock('../links/scan', async (importOriginal) => {
  const m = await importOriginal<typeof import('../links/scan')>();
  return {
    ...m,
    scanLinks: (...a: Parameters<typeof m.scanLinks>) => {
      scanSpy.calls++;
      return m.scanLinks(...a);
    },
  };
});

// The keyboard inset, settable: on a device it tracks visualViewport.
vi.mock('../stores/viewport', async (importOriginal) => {
  const m = await importOriginal<typeof import('../stores/viewport')>();
  const { writable } = await import('svelte/store');
  return { ...m, keyboardInset: writable(0) };
});
import { keyboardInset } from '../stores/viewport';
import type { Writable } from 'svelte/store';

import NoteEditor from './NoteEditor.svelte';

const URL_TEXT = 'https://example.com/x';
const WIKI_TEXT = '[[Beta-1234abcd]]';
const BODY_INNER =
  `<div>See ${URL_TEXT} here</div>` +
  `<div>Link to ${WIKI_TEXT} too</div>` +
  `<div><a href="#meeting-1">evidence</a></div>` +
  `<div id="meeting-1">The passage</div>` +
  `<div><a href="https://a.example/y">anchor link</a></div>` +
  `<div><a href="javascript:alert(1)">bad link</a></div>`;
// Whole-document body (gotcha #37).
const BODY = `<html><head></head><body>${BODY_INNER}</body></html>`;

const a: Note = { account_id: 'acct', uuid: 'note-a', id: 'a-id', title: 'Alpha', body_html: BODY, label: 'Notes', date: '2026-10-22', local_version: 2 };
const b: Note = { ...a, uuid: 'note-b', id: 'b-id', title: 'Beta', body_html: '<html><head></head><body><div>Beta body</div></body></html>' };
const target: Note = { ...a, uuid: '1234abcd-0000-0000-0000-000000000000', id: 't-id', title: 'Beta plan', body_html: '<body>Beta plan</body>' };

/** The rects each piece of text reports, keyed by the Range's text. Reset per test. */
const RECTS_AT_START: Record<string, { x: number; y: number }> = {
  [URL_TEXT]: { x: 100, y: 20 },
  [WIKI_TEXT]: { x: 100, y: 60 },
};
let RECTS: Record<string, { x: number; y: number }> = { ...RECTS_AT_START };
const rectAt = (x: number, y: number) =>
  ({ left: x - 10, top: y - 5, right: x + 10, bottom: y + 5, width: 20, height: 10, x: x - 10, y: y - 5 }) as DOMRect;

let resolveReply: (key: string) => Promise<unknown>;
function route(cmd: string, args: Record<string, unknown>): Promise<unknown> {
  if (cmd === 'platform_name') return Promise.resolve('macos');
  if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
  if (cmd === 'resolve_note_link') return resolveReply(String(args.key));
  if (cmd === 'save_note') return Promise.resolve({ id: 'a-id-2', uuid: args.existingUuid, local_version: 3 });
  if (cmd === 'note_persistence') {
    return Promise.resolve({ account_id: args.accountId, uuid: args.uuid, local_version: 3, sync_state: 'dirty', push_blocked_reason: null });
  }
  return Promise.resolve([]);
}

let host: HTMLElement;
let app: ReturnType<typeof mount> | undefined;
let originalRects: unknown;

beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
  vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => setTimeout(() => cb(performance.now()), 16) as unknown as number);
  vi.stubGlobal('cancelAnimationFrame', (id: number) => clearTimeout(id));
  originalRects = (Range.prototype as unknown as { getClientRects?: unknown }).getClientRects;
  (Range.prototype as unknown as { getClientRects: () => DOMRect[] }).getClientRects = function (this: Range) {
    const at = RECTS[this.toString()];
    return at ? [rectAt(at.x, at.y)] : [];
  };
  clearNoteAliases();
  persistenceByNote.set({});
  pendingLocalEdits.set({});
  savingNoteKeys.set(new Set());
  Object.defineProperty(HTMLElement.prototype, 'innerText', { configurable: true, get() { return this.textContent ?? ''; } });
  resolveReply = () => Promise.resolve({ note: target, firstLine: 'First line' });
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => route(cmd, args ?? {}));
  openUrl.mockReset();
  openUrl.mockResolvedValue(undefined);
  scanSpy.calls = 0;
  RECTS = { ...RECTS_AT_START };
  (keyboardInset as Writable<number>).set(0);
  stores.accounts.set([]);
  stores.currentAccount.set('acct');
  stores.notes.set([a, b]);
  stores.selectedNote.set(a);
  stores.selectedFolder.set('Notes');
  stores.isSaving.set(false);
  host = document.createElement('div');
  document.body.append(host);
});
afterEach(async () => {
  if (app) await unmount(app);
  app = undefined;
  host.remove();
  stores.selectedNote.set(null);
  (Range.prototype as unknown as { getClientRects?: unknown }).getClientRects = originalRects;
  window.getSelection()?.removeAllRanges();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function settle() {
  await tick();
  await vi.advanceTimersByTimeAsync(0);
  flushSync();
  await tick();
}
/** Mount, render the body, and let the first link scan run. */
async function open() {
  app = mount(NoteEditor, { target: host });
  await settle();
  await settle();
  await vi.advanceTimersByTimeAsync(16);
  flushSync();
}

const editor = () => host.querySelector('.editor-body') as HTMLElement;
const chip = () => host.querySelector('.link-chip') as HTMLElement | null;
const chipButton = (id: string) => chip()?.querySelector(`button[data-action="${id}"]`) as HTMLButtonElement | null;
const anchor = (href: string) => editor().querySelector(`a[href="${href}"]`) as HTMLAnchorElement;
/** The text node's parent element that contains `text`. */
function holderOf(text: string): HTMLElement {
  const el = Array.from(editor().querySelectorAll('div')).find((d) => d.textContent?.includes(text));
  if (!el) throw new Error(`no element holds ${text}`);
  return el;
}

function click(el: Element, at: { x: number; y: number } = { x: 0, y: 0 }, mods: MouseEventInit = {}): MouseEvent {
  const e = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0, clientX: at.x, clientY: at.y, ...mods });
  el.dispatchEvent(e);
  flushSync();
  return e;
}
const clickUrl = (mods: MouseEventInit = {}) => click(holderOf(URL_TEXT), RECTS[URL_TEXT], mods);
const clickWiki = (mods: MouseEventInit = {}) => click(holderOf(WIKI_TEXT), RECTS[WIKI_TEXT], mods);

it('click on a bare URL opens the chip; Open calls openUrl', async () => {
  await open();
  clickUrl();
  expect(chip()).not.toBeNull();
  expect(chip()!.textContent).toContain(URL_TEXT);
  chipButton('open')!.click();
  flushSync();
  expect(openUrl).toHaveBeenCalledWith(URL_TEXT);
  expect(chip()).toBeNull();
});

it('Cmd+click a wikilink resolves and selects the target', async () => {
  await open();
  clickWiki({ metaKey: true });
  await settle();
  expect(invoke).toHaveBeenCalledWith('resolve_note_link', { accountId: 'acct', key: 'Beta-1234abcd' });
  expect(get(stores.selectedNote)?.uuid).toBe(target.uuid);
  expect(chip()).toBeNull();
});

it('click on #meeting- anchor opens the chip; Go to scrolls', async () => {
  await open();
  const scroll = vi.fn();
  (editor().querySelector('#meeting-1') as HTMLElement).scrollIntoView = scroll;
  const e = click(anchor('#meeting-1'));
  expect(e.defaultPrevented).toBe(true);
  expect(scroll).not.toHaveBeenCalled();
  expect(chipButton('open')?.textContent).toBe('Go to');
  chipButton('open')!.click();
  flushSync();
  expect(scroll).toHaveBeenCalledOnce();
  expect(chip()).toBeNull();
});

it('javascript: href never opens', async () => {
  await open();
  const plain = click(anchor('javascript:alert(1)'));
  expect(plain.defaultPrevented).toBe(true);
  expect(chip()).toBeNull();
  const mod = click(anchor('javascript:alert(1)'), { x: 0, y: 0 }, { metaKey: true });
  expect(mod.defaultPrevented).toBe(true);
  await settle();
  expect(chip()).toBeNull();
  expect(openUrl).not.toHaveBeenCalled();
});

it('nothing saves', async () => {
  await open();
  const before = editor().innerHTML;
  expect(before).toBe(BODY_INNER);
  const hover = (el: Element, at = { x: 0, y: 0 }) => {
    el.dispatchEvent(new MouseEvent('mousemove', { bubbles: true, clientX: at.x, clientY: at.y }));
  };
  hover(holderOf(URL_TEXT), RECTS[URL_TEXT]);
  expect(editor().style.cursor).toBe('pointer');
  expect(editor().title).toBe(`Cmd+click to open ${URL_TEXT}`);
  hover(holderOf(WIKI_TEXT), RECTS[WIKI_TEXT]);
  await settle();
  hover(holderOf(WIKI_TEXT), RECTS[WIKI_TEXT]);
  expect(editor().title).toBe('Cmd+click to open Beta plan');
  hover(anchor('#meeting-1'));
  hover(anchor('https://a.example/y'));
  expect(editor().title).toBe('Cmd+click to open https://a.example/y');
  hover(anchor('javascript:alert(1)'));
  expect(editor().style.cursor).toBe('');
  expect(editor().title).toBe('');

  for (const go of [clickUrl, clickWiki, () => click(anchor('#meeting-1')), () => click(anchor('https://a.example/y'))]) {
    go();
    await settle();
    expect(chip()).not.toBeNull();
    // Esc closes it.
    editor().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(chip()).toBeNull();
  }
  click(anchor('javascript:alert(1)'));
  await vi.advanceTimersByTimeAsync(10_000);
  await settle();
  expect(invoke.mock.calls.some(([cmd]) => cmd === 'save_note')).toBe(false);
  expect(editor().innerHTML).toBe(before);
});

it('drag selection across a link opens no chip', async () => {
  await open();
  const holder = holderOf(URL_TEXT);
  const text = holder.firstChild as Text;
  const range = document.createRange();
  range.setStart(text, 0);
  range.setEnd(text, text.data.length);
  window.getSelection()!.removeAllRanges();
  window.getSelection()!.addRange(range);
  clickUrl();
  expect(chip()).toBeNull();
  expect(openUrl).not.toHaveBeenCalled();
});

it('note switch drops a late resolve and closes the chip', async () => {
  await open();
  let release: (v: unknown) => void = () => {};
  resolveReply = () => new Promise((r) => { release = r; });
  clickWiki({ metaKey: true }); // the open is waiting on resolve_note_link
  await settle();
  clickUrl(); // and a chip is up
  expect(chip()).not.toBeNull();
  stores.selectedNote.set(b);
  await settle();
  expect(chip()).toBeNull();
  release({ note: target, firstLine: 'First line' });
  await settle();
  expect(get(stores.selectedNote)?.uuid).toBe('note-b');
  expect(chip()).toBeNull();
});

it('a fresh editor element is rescanned', async () => {
  await open();
  const first = editor();
  // The same note's body changes elsewhere: a full render mounts a fresh element.
  const moved = BODY.replace('See ', 'Now see ');
  stores.selectedNote.set({ ...a, body_html: moved });
  await settle();
  await settle();
  expect(editor()).not.toBe(first);
  expect(editor().textContent).toContain('Now see');
  await vi.advanceTimersByTimeAsync(16);
  flushSync();
  clickUrl();
  expect(chip()).not.toBeNull();
  chipButton('open')!.click();
  expect(openUrl).toHaveBeenCalledWith(URL_TEXT);
});

it('ten inputs in one frame scan once', async () => {
  await open();
  const settled = scanSpy.calls;
  expect(settled).toBeGreaterThan(0);
  for (let i = 0; i < 10; i++) editor().dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
  expect(scanSpy.calls).toBe(settled);
  await vi.advanceTimersByTimeAsync(16);
  expect(scanSpy.calls).toBe(settled + 1);
});

// The cases linkOpen.test.ts held, on a real <a> in the mounted editor.
it('an <a>: Cmd+click opens it; Shift+click and Ctrl+click on macOS do not; every click is prevented', async () => {
  await open();
  const link = () => anchor('https://a.example/y');
  const shift = click(link(), { x: 0, y: 0 }, { shiftKey: true });
  expect(shift.defaultPrevented).toBe(true);
  const ctrl = click(link(), { x: 0, y: 0 }, { ctrlKey: true });
  expect(ctrl.defaultPrevented).toBe(true);
  await settle();
  expect(openUrl).not.toHaveBeenCalled();
  expect(chip()).toBeNull();
  const cmd = click(link(), { x: 0, y: 0 }, { metaKey: true });
  expect(cmd.defaultPrevented).toBe(true);
  await settle();
  expect(openUrl).toHaveBeenCalledExactlyOnceWith('https://a.example/y');
  expect(chip()).toBeNull();
});

// ─── Fix round 1 ─────────────────────────────────────────────────────────

/** The editor's visible box (jsdom has no layout). */
function stubEditorBox(top = 0, bottom = 400) {
  editor().getBoundingClientRect = () => ({ left: 0, top, right: 800, bottom, width: 800, height: bottom - top, x: 0, y: top }) as DOMRect;
}
const scroll = () => {
  editor().dispatchEvent(new Event('scroll'));
  flushSync();
};

it('a scroll that leaves the link visible keeps the chip and moves it with the link', async () => {
  await open();
  stubEditorBox();
  clickUrl();
  expect(chip()!.style.top).toBe('27px'); // under the link's rect (bottom 25) + 2
  RECTS[URL_TEXT] = { x: 100, y: 120 }; // the body scrolled; the link moved down
  scroll();
  expect(chip()).not.toBeNull();
  expect(chip()!.style.top).toBe('127px');
});

it('a scroll that takes the link out of the visible box closes the chip', async () => {
  await open();
  stubEditorBox();
  clickUrl();
  expect(chip()).not.toBeNull();
  RECTS[URL_TEXT] = { x: 100, y: -40 }; // scrolled up past the top
  scroll();
  expect(chip()).toBeNull();
});

it('a keyboard inset change re-places the chip', async () => {
  await open();
  stubEditorBox();
  clickUrl();
  expect(chip()!.style.top).toBe('27px');
  RECTS[URL_TEXT] = { x: 100, y: 200 }; // the keyboard came up and the layout moved
  (keyboardInset as Writable<number>).set(300);
  await settle();
  expect(chip()).not.toBeNull();
  expect(chip()!.style.top).toBe('207px');
});

it('Cmd+click a missing wikilink opens the chip instead, with only Copy', async () => {
  await open();
  resolveReply = () => Promise.resolve(null);
  clickWiki({ metaKey: true });
  await settle();
  expect(chip()).not.toBeNull();
  expect(chip()!.textContent).toContain('No note called “Beta-1234abcd”');
  const actions = Array.from(chip()!.querySelectorAll('button')).map((b) => b.dataset.action);
  expect(actions).toEqual(['copy']);
  expect(get(stores.selectedNote)?.uuid).toBe('note-a');
  expect(openUrl).not.toHaveBeenCalled();
});

// The positive control for 'nothing saves': the same harness does save a real edit.
it('a real input in the same harness does call save_note', async () => {
  await open();
  editor().innerHTML = BODY_INNER + '<div>typed</div>';
  editor().dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
  await vi.advanceTimersByTimeAsync(1500); // the autosave debounce
  await settle();
  await settle();
  expect(invoke.mock.calls.some(([cmd]) => cmd === 'save_note')).toBe(true);
});

// ─── Task 10: the keyboard, Edit link and Remove link ───────────────────

const doc = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
/** Mount with this body instead of BODY. */
async function openWith(inner: string) {
  stores.selectedNote.set({ ...a, body_html: doc(inner) });
  await open();
}
/** The text node in the body whose data is exactly `data`. */
function textNode(data: string): Text {
  const walker = document.createTreeWalker(editor(), NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) if ((n as Text).data === data) return n as Text;
  throw new Error(`no text node "${data}"`);
}
function caret(node: Node, offset: number) {
  const r = document.createRange();
  r.setStart(node, offset);
  r.collapse(true);
  window.getSelection()!.removeAllRanges();
  window.getSelection()!.addRange(r);
}
function cmdEnter(): KeyboardEvent {
  const e = new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', metaKey: true, bubbles: true, cancelable: true });
  editor().dispatchEvent(e);
  flushSync();
  return e;
}
const saves = () => invoke.mock.calls.filter(([cmd]) => cmd === 'save_note').length;
async function afterAutosave() {
  await vi.advanceTimersByTimeAsync(1500);
  await settle();
  await settle();
}

// jsdom has no execCommand. This one does what WebKit does for the commands
// used here, records each call, and fires the native `input` event a real
// webview fires on the editor after an editing command — unless `fireInput`
// is false, the webview that fires none (the editor must then call onInput).
let execCalls: string[] = [];
function stubExecCommand({ fireInput = true } = {}) {
  execCalls = [];
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string, _ui?: boolean, arg?: string) => {
      execCalls.push(cmd);
      const sel = window.getSelection();
      if (!sel || sel.rangeCount === 0) return false;
      const r = sel.getRangeAt(0);
      if (cmd === 'unlink') {
        const c = r.commonAncestorContainer;
        const link = (c instanceof Element ? c : c.parentElement)?.closest('a');
        if (!link) return false;
        link.replaceWith(...Array.from(link.childNodes));
      } else if (cmd === 'insertHTML') {
        const t = document.createElement('template');
        t.innerHTML = arg ?? '';
        r.deleteContents();
        r.insertNode(t.content);
      } else if (cmd !== 'insertParagraph') {
        return false;
      }
      if (fireInput) editor().dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    },
  });
}
afterEach(() => {
  delete (document as unknown as Record<string, unknown>).execCommand;
});

/**
 * How many times onInput ran during `act`. Every call re-arms the autosave
 * debounce with `setTimeout(autoSave, 1500)` — its one per-call effect a test
 * can see (the optimistic store update skips a body it already has, so a
 * second call is invisible there, and the save itself is debounced to one).
 */
function onInputCalls(act: () => void): number {
  const spy = vi.spyOn(globalThis, 'setTimeout');
  try {
    act();
    return spy.mock.calls.filter(([cb, ms]) => ms === 1500 && typeof cb === 'function' && cb.name === 'autoSave').length;
  } finally {
    spy.mockRestore();
  }
}

const CHECKLIST =
  `<div><input type="checkbox" contenteditable="false">Read <a href="https://a.example/y">anchor link</a></div>` +
  `<div><input type="checkbox" contenteditable="false">See ${URL_TEXT} now</div>`;

it('Cmd+Enter inside a link opens it — before the checklist branch', async () => {
  await openWith(CHECKLIST);
  stubExecCommand();
  caret(textNode('anchor link'), 3);
  const onAnchor = cmdEnter();
  expect(onAnchor.defaultPrevented).toBe(true);
  expect(openUrl).toHaveBeenCalledExactlyOnceWith('https://a.example/y');
  caret(textNode(`See ${URL_TEXT} now`), 'See https://exa'.length);
  const onUrl = cmdEnter();
  expect(onUrl.defaultPrevented).toBe(true);
  expect(openUrl).toHaveBeenLastCalledWith(URL_TEXT);
  // No new checklist row, no edit, no chip.
  expect(execCalls).toEqual([]);
  expect(editor().querySelectorAll('input[type=checkbox]').length).toBe(2);
  expect(chip()).toBeNull();
  await afterAutosave();
  expect(saves()).toBe(0);
});

it('Cmd+Enter on a checklist row outside a link still continues the checklist', async () => {
  await openWith(CHECKLIST);
  stubExecCommand();
  caret(textNode('Read '), 2);
  const e = cmdEnter();
  expect(e.defaultPrevented).toBe(true);
  expect(execCalls[0]).toBe('insertParagraph');
  expect(openUrl).not.toHaveBeenCalled();
  expect(chip()).toBeNull();
});

// The boundary: the caret is in a link when the character just before it is
// the link's — strictly inside it, or right at its end. At its start it is not.
it('Cmd+Enter at a link’s end opens it; at its start it does not', async () => {
  await openWith(`<div>See ${URL_TEXT}</div><div>Go <a href="https://a.example/y">anchor link</a> now</div>`);
  const atStart = [
    () => caret(textNode(`See ${URL_TEXT}`), 'See '.length),
    () => caret(textNode('anchor link'), 0),
    () => caret(textNode('Go '), 3),
  ];
  for (const place of atStart) {
    place();
    expect(cmdEnter().defaultPrevented).toBe(false); // today's Cmd+Enter, untouched
  }
  expect(openUrl).not.toHaveBeenCalled();

  caret(textNode(`See ${URL_TEXT}`), `See ${URL_TEXT}`.length);
  expect(cmdEnter().defaultPrevented).toBe(true);
  expect(openUrl).toHaveBeenLastCalledWith(URL_TEXT);
  caret(textNode('anchor link'), 'anchor link'.length);
  expect(cmdEnter().defaultPrevented).toBe(true);
  expect(openUrl).toHaveBeenLastCalledWith('https://a.example/y');
  // Just past </a>, in the next text node: still the link's end.
  caret(textNode(' now'), 0);
  expect(cmdEnter().defaultPrevented).toBe(true);
  // …and the same point given as (the line, the child after </a>).
  caret(anchor('https://a.example/y').parentElement!, 2);
  expect(cmdEnter().defaultPrevented).toBe(true);
  expect(openUrl).toHaveBeenCalledTimes(4);
  expect(openUrl).toHaveBeenLastCalledWith('https://a.example/y');
});

// Ruling 17: a link the modifier may not open shows its chip instead.
const INERT = `<div>Open <a href="applenotes:note/1">apple note</a> later</div>`;

it('Cmd+click a link the modifier may not open shows its chip', async () => {
  await openWith(INERT);
  const e = click(anchor('applenotes:note/1'), { x: 0, y: 0 }, { metaKey: true });
  expect(e.defaultPrevented).toBe(true);
  expect(chip()).not.toBeNull();
  expect(chip()!.textContent).toContain('applenotes:note/1');
  expect(Array.from(chip()!.querySelectorAll('button')).map((b) => b.dataset.action)).toEqual(['copy']);
  expect(openUrl).not.toHaveBeenCalled();
});

it('Cmd+Enter on a link the modifier may not open shows its chip', async () => {
  await openWith(INERT);
  caret(textNode('apple note'), 4);
  const e = cmdEnter();
  expect(e.defaultPrevented).toBe(true);
  expect(chip()).not.toBeNull();
  expect(chip()!.textContent).toContain('applenotes:note/1');
  expect(openUrl).not.toHaveBeenCalled();
});

const ONE_LINK = `<div>Read <a href="https://a.example/y">anchor link</a> now</div>`;
const prompt = () => host.querySelector('.link-prompt') as HTMLElement | null;
const okButton = () => host.querySelector('.link-prompt-btn:not(.link-prompt-cancel)') as HTMLButtonElement;
const promptText = () => host.querySelector('.link-prompt-text') as HTMLInputElement | null;
const promptUrl = () => host.querySelector('.link-prompt-url') as HTMLInputElement;
function type(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

it('Edit link prefills href and text and replaces the anchor', async () => {
  await openWith(ONE_LINK);
  stubExecCommand();
  click(anchor('https://a.example/y'));
  chipButton('edit')!.click();
  flushSync();
  await vi.advanceTimersByTimeAsync(0);
  expect(prompt()).not.toBeNull();
  expect(promptText()!.value).toBe('anchor link');
  expect(promptUrl().value).toBe('https://a.example/y');
  type(promptUrl(), 'https://b.example/z?q="1"');
  type(promptText()!, 'new <text>');
  expect(onInputCalls(() => { okButton().click(); flushSync(); })).toBe(1);
  // One command (one undo step) over the selected anchor.
  expect(execCalls).toEqual(['insertHTML']);
  const links = editor().querySelectorAll('a');
  expect(links.length).toBe(1);
  expect(links[0].getAttribute('href')).toBe('https://b.example/z?q="1"');
  expect(links[0].textContent).toBe('new <text>');
  expect(editor().textContent).toBe('Read new <text> now');
  expect(prompt()).toBeNull();
  await afterAutosave();
  expect(saves()).toBe(1);
});

it('cancelling Edit link changes nothing, and a new link still starts at https://', async () => {
  await openWith(ONE_LINK);
  stubExecCommand();
  const before = editor().innerHTML;
  click(anchor('https://a.example/y'));
  chipButton('edit')!.click();
  flushSync();
  await vi.advanceTimersByTimeAsync(0);
  type(promptUrl(), 'https://changed.example/');
  (host.querySelector('.link-prompt-cancel') as HTMLButtonElement).click();
  flushSync();
  expect(prompt()).toBeNull();
  expect(execCalls).toEqual([]);
  expect(editor().innerHTML).toBe(before);
  await afterAutosave();
  expect(saves()).toBe(0);

  // Cmd+K with no anchor involved: today's blank prompt.
  caret(textNode(' now'), 4);
  editor().dispatchEvent(new KeyboardEvent('keydown', { key: 'k', code: 'KeyK', metaKey: true, bubbles: true, cancelable: true }));
  flushSync();
  expect(promptUrl().value).toBe('https://');
  expect(promptText()!.value).toBe('');
});

it('Remove link keeps the text and saves once', async () => {
  await openWith(ONE_LINK);
  stubExecCommand();
  click(anchor('https://a.example/y'));
  expect(onInputCalls(() => { chipButton('remove')!.click(); flushSync(); })).toBe(1);
  expect(execCalls).toEqual(['unlink']);
  expect(editor().querySelector('a')).toBeNull();
  expect(editor().textContent).toBe('Read anchor link now');
  expect(chip()).toBeNull();
  await afterAutosave();
  expect(saves()).toBe(1);
});

// ─── Fix round 1 ─────────────────────────────────────────────────────────

// A webview whose execCommand fires no `input` event: the editor's own
// fallback call is then the only onInput, and the edit still saves.
it('Remove link with no native input event still saves, once', async () => {
  await openWith(ONE_LINK);
  stubExecCommand({ fireInput: false });
  click(anchor('https://a.example/y'));
  expect(onInputCalls(() => { chipButton('remove')!.click(); flushSync(); })).toBe(1);
  expect(editor().querySelector('a')).toBeNull();
  await afterAutosave();
  expect(saves()).toBe(1);
});

/** Open Edit link on the body's only <a>, set the URL, press OK; returns the onInput count. */
async function editUrlOnly(newUrl: string): Promise<number> {
  click(editor().querySelector('a')!);
  chipButton('edit')!.click();
  flushSync();
  await vi.advanceTimersByTimeAsync(0);
  type(promptUrl(), newUrl);
  return onInputCalls(() => { okButton().click(); flushSync(); });
}

it('Edit link with no native input event still saves, once', async () => {
  await openWith(ONE_LINK);
  stubExecCommand({ fireInput: false });
  expect(await editUrlOnly('https://b.example/z')).toBe(1);
  expect(editor().querySelector('a')!.getAttribute('href')).toBe('https://b.example/z');
  await afterAutosave();
  expect(saves()).toBe(1);
});

// Ruling 18: a URL-only edit changes the href and nothing else.
it('a URL-only edit keeps the markup inside the link', async () => {
  await openWith(`<div>See <a href="https://a.co" class="x"><b>bold</b> word</a> here</div>`);
  stubExecCommand();
  await editUrlOnly('https://b.co');
  expect(execCalls).toEqual(['insertHTML']);
  const links = editor().querySelectorAll('a');
  expect(links.length).toBe(1);
  expect(links[0].outerHTML).toBe('<a href="https://b.co" class="x"><b>bold</b> word</a>');
});

it('a URL-only edit of a linked image keeps the image', async () => {
  await openWith(`<div><a href="https://a.co"><img src="cid:pic" alt="pic"></a></div>`);
  stubExecCommand();
  await editUrlOnly('https://b.co');
  const link = editor().querySelector('a')!;
  expect(link.getAttribute('href')).toBe('https://b.co');
  expect(link.querySelector('img')?.getAttribute('src')).toBe('cid:pic');
  expect(link.textContent).toBe('');
});

it('a URL-only edit keeps the space at the end of the link text', async () => {
  await openWith(`<div><a href="https://a.co">link </a>now</div>`);
  stubExecCommand();
  await editUrlOnly('https://b.co');
  expect(editor().textContent).toBe('link now');
  expect(editor().querySelector('a')!.textContent).toBe('link ');
});

it('a changed label rebuilds the link from the typed text, untrimmed and escaped', async () => {
  await openWith(`<div><a href="https://a.co" class="x"><b>bold</b> word</a>now</div>`);
  stubExecCommand();
  click(editor().querySelector('a')!);
  chipButton('edit')!.click();
  flushSync();
  await vi.advanceTimersByTimeAsync(0);
  type(promptText()!, 'a <new> label ');
  okButton().click();
  flushSync();
  const link = editor().querySelector('a')!;
  expect(link.outerHTML).toBe('<a href="https://a.co">a &lt;new&gt; label </a>');
  expect(editor().textContent).toBe('a <new> label now');
});

// ─── Final fix pass: Copy that never goes silent; Edit/Remove on read-only ──

// `navigator.clipboard` rejects in the Tauri window (live run, AccountPage),
// and Copy is the chip's only action for an inert link or a missing note.
function stubClipboard(writeText: (t: string) => Promise<void>) {
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: vi.fn(writeText) } });
}
/** execCommand('copy') that records what the hidden textarea held, or fails. */
function stubExecCopy(works = true): string[] {
  const copied: string[] = [];
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string) => {
      if (cmd !== 'copy' || !works) return false;
      const ta = document.body.querySelector(':scope > textarea') as HTMLTextAreaElement | null;
      if (!ta) return false;
      copied.push(ta.value);
      return true;
    },
  });
  return copied;
}
/** What a real webview does on textarea.select(): the caret leaves the editor. */
function textareaSelectTakesTheSelection() {
  vi.spyOn(HTMLTextAreaElement.prototype, 'select').mockImplementation(function (this: HTMLTextAreaElement) {
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    const r = document.createRange();
    r.selectNodeContents(this);
    sel.addRange(r);
  });
}
afterEach(() => {
  delete (navigator as unknown as Record<string, unknown>).clipboard;
  vi.restoreAllMocks();
  stores.capabilitiesByAccount.set({});
  stores.error.set(null);
});

it('chip Copy with navigator.clipboard rejecting still copies, and the caret stays put', async () => {
  await open();
  stubClipboard(() => Promise.reject(new DOMException('denied', 'NotAllowedError')));
  const copied = stubExecCopy();
  textareaSelectTakesTheSelection();
  const passage = textNode('The passage');
  caret(passage, 3);
  clickUrl();
  chipButton('copy')!.click();
  flushSync();
  await settle();
  expect(copied).toEqual([URL_TEXT]);
  const sel = window.getSelection()!;
  expect(sel.rangeCount).toBe(1);
  const r = sel.getRangeAt(0);
  expect([r.startContainer, r.startOffset, r.collapsed]).toEqual([passage, 3, true]);
  expect(document.body.querySelectorAll(':scope > textarea').length).toBe(0);
  expect(get(stores.error)).toBeNull();
  expect(chip()).toBeNull();
});

it('chip Copy that cannot copy at all says so', async () => {
  await open();
  stubClipboard(() => Promise.reject(new Error('denied')));
  stubExecCopy(false);
  clickUrl();
  chipButton('copy')!.click();
  flushSync();
  await settle();
  expect(get(stores.error)).toBe("Couldn't copy the link.");
});

it('a read-only note offers no Edit link or Remove link', async () => {
  stores.capabilitiesByAccount.set({ acct: { writes: { notes: false, relocate: false, folders: false, sidecars: false } } } as never);
  await openWith(ONE_LINK);
  click(anchor('https://a.example/y'));
  expect(Array.from(chip()!.querySelectorAll('button')).map((b) => b.dataset.action)).toEqual(['open', 'copy']);
});

it('a note locked by the remote offers no Edit link or Remove link', async () => {
  const locked = { ...a, body_html: doc(ONE_LINK), push_blocked_reason: 'refused', push_blocked_by_remote: true };
  invoke.mockImplementation((cmd: string, args: Record<string, unknown>) =>
    cmd === 'note_persistence' ? Promise.resolve(null) : route(cmd, args ?? {}));
  stores.notes.set([locked, b]);
  stores.selectedNote.set(locked);
  await open();
  expect(editor().getAttribute('contenteditable')).toBe('false');
  click(anchor('https://a.example/y'));
  expect(Array.from(chip()!.querySelectorAll('button')).map((b) => b.dataset.action)).toEqual(['open', 'copy']);
});

it('a writable note still offers Edit link and Remove link', async () => {
  await openWith(ONE_LINK);
  click(anchor('https://a.example/y'));
  expect(Array.from(chip()!.querySelectorAll('button')).map((b) => b.dataset.action)).toEqual(['open', 'copy', 'edit', 'remove']);
});
