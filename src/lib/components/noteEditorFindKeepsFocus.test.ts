// @vitest-environment jsdom
//
// The find bar marked its current match by moving the DOCUMENT selection onto
// it — 350 ms after the user paused typing, and on every Enter / ‹ / ›. Inside
// a contenteditable the selection IS the caret, so a real webview moved focus
// into the note with the match selected, and the next keystroke the user meant
// for the query overwrote the match in the note instead ("hello" + type "lo"
// → "lolo", measured in Chromium 2026-10-06). A title match called
// input.focus() outright.
//
// jsdom does not move focus on addRange, so the body cases pin the cause
// rather than the symptom: while the bar is open, no selection may sit inside
// the editor. The match is painted with the CSS Custom Highlight API instead,
// and the caret moves to it only when the user leaves the bar with Escape.
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

class FakeHighlight {
  ranges: Range[];
  constructor(...ranges: Range[]) { this.ranges = ranges; }
}

function note(title: string, body_html: string): Note {
  return {
    uuid: 'uuid-find',
    id: 'msg-1',
    account_id: 'gmail:test@example.com',
    title,
    body_html,
    date: '2026-10-06T00:00:00Z',
    label: 'Notes',
  } as Note;
}

let highlights: Map<string, FakeHighlight>;
let cleanup: (() => void) | null = null;

beforeEach(() => {
  highlights = new Map();
  vi.stubGlobal('CSS', { highlights, supports: () => false, escape: (s: string) => s });
  vi.stubGlobal('Highlight', FakeHighlight);
  // jsdom has no layout: give it the two geometry calls a scroll check makes.
  if (!Element.prototype.scrollIntoView) Element.prototype.scrollIntoView = () => {};
  if (!Range.prototype.getBoundingClientRect) {
    Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
  }
});

afterEach(() => {
  cleanup?.();
  cleanup = null;
  selectedNote.set(null);
  notes.set([]);
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
  window.getSelection()?.removeAllRanges();
});

async function openFindWith(n: Note, query: string) {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'platform_name') return Promise.resolve('macos');
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
  notes.set([n]);
  selectedNote.set(n);
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  cleanup = () => { unmount(app); host.remove(); };
  await tick();
  await tick(); // platform_name resolves
  flushSync();

  vi.useFakeTimers();
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', metaKey: true }));
  flushSync();
  vi.advanceTimersByTime(1); // openFind focuses the input on the next task
  const findInput = host.querySelector('.find-input') as HTMLInputElement;
  expect(document.activeElement).toBe(findInput);

  findInput.value = query;
  findInput.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
  vi.advanceTimersByTime(400); // past the 350 ms pause
  flushSync();

  const editor = host.querySelector('.editor-body') as HTMLElement;
  return { host, findInput, editor };
}

function selectionInside(el: HTMLElement): boolean {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return false;
  return el.contains(sel.getRangeAt(0).startContainer);
}

function enter(input: HTMLInputElement, shiftKey = false) {
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', shiftKey, bubbles: true }));
  flushSync();
}

describe('find in note keeps focus in the find bar', () => {
  it('a typing pause paints the match without putting the caret in the note', async () => {
    const { host, findInput, editor } = await openFindWith(note('Note', '<p>hello world hello</p>'), 'hel');

    expect(document.activeElement).toBe(findInput);
    expect(selectionInside(editor)).toBe(false);
    expect(host.querySelector('.find-count')?.textContent?.trim()).toBe('1 / 2');
    const current = highlights.get('jodd-find-current');
    expect(current?.ranges.map((r) => r.toString())).toEqual(['hel']);
    expect(current?.ranges[0].startOffset).toBe(0);
  });

  it('Enter steps through matches and focus never leaves the find bar', async () => {
    const { host, findInput, editor } = await openFindWith(note('Note', '<p>hello world hello</p>'), 'hel');

    enter(findInput);
    expect(document.activeElement).toBe(findInput);
    expect(selectionInside(editor)).toBe(false);
    expect(host.querySelector('.find-count')?.textContent?.trim()).toBe('2 / 2');
    expect(highlights.get('jodd-find-current')?.ranges[0].startOffset).toBe(12);

    // The second Enter is the one that used to land in the note as a newline.
    enter(findInput);
    expect(document.activeElement).toBe(findInput);
    expect(host.querySelector('.find-count')?.textContent?.trim()).toBe('1 / 2');
    expect(editor.textContent).toBe('hello world hello');
  });

  it('a title match does not focus the title input', async () => {
    const { host, findInput } = await openFindWith(note('Help me', '<p>nothing</p>'), 'hel');

    expect(host.querySelector('.find-count')?.textContent?.trim()).toBe('1 / 1');
    expect(document.activeElement).toBe(findInput);
  });

  it('Escape closes the bar and selects the current match in the note', async () => {
    const { host, findInput, editor } = await openFindWith(note('Note', '<p>hello world hello</p>'), 'hel');
    enter(findInput);

    findInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    flushSync();

    expect(host.querySelector('.find-bar')).toBeNull();
    expect(selectionInside(editor)).toBe(true);
    const r = window.getSelection()!.getRangeAt(0);
    expect(r.toString()).toBe('hel');
    expect(r.startOffset).toBe(12);
    // Emptied, not deleted: what was painted passes through the transparent
    // highlight so WebKit repaints it (see 'the paint follows the query').
    expect(highlights.get('jodd-find')?.ranges ?? []).toEqual([]);
    expect(highlights.get('jodd-find-current')?.ranges ?? []).toEqual([]);
  });
});

// Replace must CONSUME a match. Find is case-insensitive, so text Replace put
// in can still match the query: "line" → "LINE" re-replaced LINE1 on every
// press and the count sat at 5 / 5 forever (dev build, 2026-10-06); "note" →
// "notes" grew "notess". Agreed fix: what Replace inserted is not a match —
// not counted, not painted — until the query, the replacement, an option or
// the note itself changes. jsdom has no execCommand; the stub performs
// insertText on the selection the way the browser does.
describe('Replace consumes what it replaced', () => {
  let execCommand: typeof document.execCommand | undefined;
  beforeEach(() => {
    execCommand = document.execCommand;
    document.execCommand = ((cmd: string, _ui?: boolean, text?: string) => {
      const sel = window.getSelection();
      if (cmd !== 'insertText' || !sel?.rangeCount) return false;
      const r = sel.getRangeAt(0);
      r.deleteContents();
      const t = document.createTextNode(text ?? '');
      r.insertNode(t);
      sel.collapse(t, t.length);
      return true;
    }) as typeof document.execCommand;
  });
  afterEach(() => { document.execCommand = execCommand as typeof document.execCommand; });

  const count = (host: HTMLElement) => host.querySelector('.find-count')?.textContent?.trim() ?? '';
  const button = (host: HTMLElement, label: string) =>
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === label || b.getAttribute('aria-label') === label) as HTMLButtonElement;

  function click(host: HTMLElement, label: string) {
    button(host, label).click();
    flushSync();
    vi.advanceTimersByTime(1); // replaceCurrent re-selects on the next task
    flushSync();
  }

  function type(input: HTMLInputElement, value: string) {
    input.value = value;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    vi.advanceTimersByTime(400);
    flushSync();
  }

  async function openReplace(n: Note, query: string, replacement: string, options: string[] = []) {
    const opened = await openFindWith(n, query);
    for (const o of options) click(opened.host, o);
    (opened.host.querySelector('.find-toggle input') as HTMLInputElement).click();
    flushSync();
    type(opened.host.querySelector('input[placeholder="Replace with"]') as HTMLInputElement, replacement);
    return opened;
  }

  async function replaceTimes(n: Note, query: string, replacement: string, presses: number, options: string[] = []) {
    const opened = await openReplace(n, query, replacement, options);
    const counts: string[] = [];
    for (let i = 0; i < presses; i++) {
      click(opened.host, 'Replace');
      counts.push(count(opened.host));
    }
    return { ...opened, counts };
  }

  const painted = (name: string) => highlights.get(name)?.ranges.map((r) => r.toString()) ?? [];

  it('a case-only replacement counts down to no match and stops', async () => {
    const { host, editor, counts } = await replaceTimes(
      note('Note', '<p>line1</p><p>line2</p><p>line3</p>'), 'line', 'LINE', 3);

    expect(Array.from(editor.querySelectorAll('p')).map((p) => p.textContent)).toEqual(['LINE1', 'LINE2', 'LINE3']);
    expect(counts).toEqual(['1 / 2', '1 / 1', 'no match']);
    expect(button(host, 'Replace').disabled).toBe(true);
    expect(button(host, 'All').disabled).toBe(true);
    expect(painted('jodd-find')).toEqual([]);
  });

  it('what was replaced is no longer painted', async () => {
    await replaceTimes(note('Note', '<p>line1</p><p>line2</p><p>line3</p>'), 'line', 'LINE', 1);

    expect(painted('jodd-find')).toEqual(['line', 'line']);
    expect(painted('jodd-find-current')).toEqual(['line']);
  });

  it('a replacement containing the query does not grow the same match', async () => {
    const { editor, counts } = await replaceTimes(note('Plural', '<p>note and note</p>'), 'note', 'notes', 3);

    expect(editor.textContent).toBe('notes and notes');
    expect(counts).toEqual(['1 / 1', 'no match', 'no match']);
  });

  it('steps from a title match into the body', async () => {
    const { host, editor, counts } = await replaceTimes(note('line title', '<p>line</p>'), 'line', 'LINE', 2);

    expect((host.querySelector('.title-input') as HTMLInputElement).value).toBe('LINE title');
    expect(editor.textContent).toBe('LINE');
    expect(counts).toEqual(['1 / 1', 'no match']);
  });

  it('a replacement without the query takes the next match in place', async () => {
    const { editor, counts } = await replaceTimes(note('Note', '<p>cat and cat and cat</p>'), 'cat', 'dog', 2);

    expect(editor.textContent).toBe('dog and dog and cat');
    expect(counts).toEqual(['1 / 2', '1 / 1']);
  });

  it('Replace All leaves nothing to find', async () => {
    const { host, editor } = await openReplace(note('Note', '<p>line1</p><p>line2</p>'), 'line', 'LINE');
    click(host, 'All');

    expect(editor.textContent).toBe('LINE1LINE2');
    expect(count(host)).toBe('no match');
    click(host, '›');
    expect(count(host)).toBe('no match');
  });

  it('changing the query forgets what was replaced', async () => {
    const { host, findInput } = await replaceTimes(note('Note', '<p>line1</p><p>line2</p>'), 'line', 'LINE', 1);
    expect(count(host)).toBe('1 / 1');

    type(findInput, 'lin');
    expect(count(host)).toBe('1 / 2');
  });

  it('what was replaced stays consumed when the same text is re-rendered', async () => {
    // A Gmail save re-renders the body from fresh nodes; a Range-based memory
    // would die there and the count would jump back up.
    const { host, editor } = await replaceTimes(note('Note', '<p>line1</p><p>line2</p>'), 'line', 'LINE', 1);
    editor.innerHTML = editor.innerHTML;
    click(host, '›');

    expect(count(host)).toBe('1 / 1');
    expect(highlights.get('jodd-find')?.ranges.map((r) => r.toString())).toEqual(['line']);
  });

  it('typing in the note forgets what was replaced', async () => {
    const { host, editor } = await replaceTimes(note('Note', '<p>line1</p><p>line2</p>'), 'line', 'LINE', 1);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    click(host, '›');

    expect(count(host)).toBe('2 / 2');
  });
});

describe('Match case and Regex options', () => {
  const count = (host: HTMLElement) => host.querySelector('.find-count')?.textContent?.trim() ?? '';
  function toggle(host: HTMLElement, label: string) {
    const b = host.querySelector(`button[aria-label="${label}"]`) as HTMLButtonElement;
    b.click();
    flushSync();
    return b;
  }

  it('Aa makes the search case-sensitive', async () => {
    const { host } = await openFindWith(note('Note', '<p>Line line LINE</p>'), 'line');
    expect(count(host)).toBe('1 / 3');

    const b = toggle(host, 'Match case');
    expect(b.getAttribute('aria-pressed')).toBe('true');
    expect(count(host)).toBe('1 / 1');
    expect(highlights.get('jodd-find')?.ranges.map((r) => r.toString())).toEqual(['line']);
  });

  it('.* reads the query as a regex with ^ and $ per line', async () => {
    const { host } = await openFindWith(note('Note', '<div>line1</div><div>xline</div><div>line</div>'), '^line$');
    expect(count(host)).toBe('no match');

    const b = toggle(host, 'Regular expression');
    expect(b.getAttribute('aria-pressed')).toBe('true');
    expect(count(host)).toBe('1 / 1');
  });

  it('Aa and .* combine: a case-sensitive regex', async () => {
    const { host } = await openFindWith(note('Note', '<div>Line1</div><div>line2</div><div>LINE3</div>'), '^l\\w+\\d$');
    toggle(host, 'Regular expression');
    expect(count(host)).toBe('1 / 3');

    toggle(host, 'Match case');
    expect(count(host)).toBe('1 / 1');
    expect(highlights.get('jodd-find-current')?.ranges.map((r) => r.toString())).toEqual(['line2']);
  });

  it('an invalid regex says so', async () => {
    const { host } = await openFindWith(note('Note', '<p>a(b</p>'), 'a(');
    expect(count(host)).toBe('1 / 1');

    toggle(host, 'Regular expression');
    expect(count(host)).toBe('invalid regex');
  });
});

describe('Regex replace', () => {
  let execCommand: typeof document.execCommand | undefined;
  beforeEach(() => {
    execCommand = document.execCommand;
    document.execCommand = ((cmd: string, _ui?: boolean, text?: string) => {
      const sel = window.getSelection();
      if (cmd !== 'insertText' || !sel?.rangeCount) return false;
      const r = sel.getRangeAt(0);
      r.deleteContents();
      const t = document.createTextNode(text ?? '');
      r.insertNode(t);
      sel.collapse(t, t.length);
      return true;
    }) as typeof document.execCommand;
  });
  afterEach(() => { document.execCommand = execCommand as typeof document.execCommand; });

  async function open(n: Note, query: string, replacement: string) {
    const opened = await openFindWith(n, query);
    (opened.host.querySelector('button[aria-label="Regular expression"]') as HTMLButtonElement).click();
    flushSync();
    (opened.host.querySelector('.find-toggle input') as HTMLInputElement).click();
    flushSync();
    const input = opened.host.querySelector('input[placeholder="Replace with"]') as HTMLInputElement;
    input.value = replacement;
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    return opened;
  }
  const press = (host: HTMLElement, label: string) => {
    (Array.from(host.querySelectorAll('.find-btn-text')).find((b) => b.textContent?.trim() === label) as HTMLButtonElement).click();
    flushSync();
    vi.advanceTimersByTime(1);
    flushSync();
  };

  it('Replace expands $1, $2 and $&', async () => {
    const { host, editor } = await open(note('Names', '<div>John Smith</div>'), '^(\\w+) (\\w+)$', '$2, $1 [$&]');
    press(host, 'Replace');

    expect(editor.textContent).toBe('Smith, John [John Smith]');
    expect(host.querySelector('.find-count')?.textContent?.trim()).toBe('no match');
  });

  it('Replace All expands each match on its own', async () => {
    const { host, editor } = await open(note('Pairs', '<div>a1</div><div>b2</div>'), '^(\\w)(\\d)$', '$2$1$$');
    press(host, 'All');

    expect(Array.from(editor.querySelectorAll('div')).map((d) => d.textContent)).toEqual(['1a$', '2b$']);
  });
});

// Reported 2026-10-06 with Aa on: typing "lin" → "line" showed linE1's "lin"
// still dark next to the right match, and fixed itself "after a while". Two
// causes. The paint waited for the 350 ms pause while the count did not —
// now the paint is live and only the scroll waits. And WebKit does not
// repaint a range that leaves every highlight, so linE1 kept its old colour
// until something else repainted (measured in Safari: stuck every time; the
// same page with the old ranges moved into a transparent highlight cleared).
describe('the paint follows the query at once', () => {
  const current = () => highlights.get('jodd-find-current')?.ranges.map((r) => r.toString()) ?? [];

  it('repaints on every keystroke, before the pause', async () => {
    const { host, findInput } = await openFindWith(note('Note', '<p>linE1</p><p>line2</p>'), 'lin');
    (host.querySelector('button[aria-label="Match case"]') as HTMLButtonElement).click();
    flushSync();
    expect(current()).toEqual(['lin']);

    findInput.value = 'line';
    findInput.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync(); // no timer advanced

    expect(current()).toEqual(['line']);
    expect(highlights.get('jodd-find')?.ranges.map((r) => r.toString())).toEqual(['line']);
  });

  it('moves what is no longer a match into the transparent highlight', async () => {
    const { findInput } = await openFindWith(note('Note', '<p>linE1</p><p>line2</p>'), 'lin');

    findInput.value = 'line';
    findInput.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    // linE1's "lin" left every visible highlight; it must sit in the
    // transparent one so WebKit repaints it.
    const gone = highlights.get('jodd-find-gone')?.ranges ?? [];
    expect(gone.map((r) => [r.startContainer.textContent, r.toString()])).toContainEqual(['linE1', 'lin']);
  });

  it('a query with no match clears the paint through the transparent highlight', async () => {
    const { findInput } = await openFindWith(note('Note', '<p>line1</p>'), 'line');

    findInput.value = 'zzz';
    findInput.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    expect(highlights.get('jodd-find')?.ranges ?? []).toEqual([]);
    expect(highlights.get('jodd-find-current')?.ranges ?? []).toEqual([]);
    expect((highlights.get('jodd-find-gone')?.ranges ?? []).map((r) => r.toString())).toEqual(['line']);
  });
});
