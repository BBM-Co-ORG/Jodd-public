// @vitest-environment jsdom
//
// Task 12 (spec 2026-11-19 §7): help typing the property block and due
// dates, mounted. The key picker on a block line, the value picker after
// `Key: `, the date picker after `📅 ` and from the toolbar, Add property,
// and the fix-it on a wavy underline — each writes only on a pick, a click
// or a toolbar press, and opening a note writes nothing at all.
//
// jsdom has no execCommand. The stub edits the selection the way WebKit does
// for insertText (replace the selection, caret after) and insertParagraph
// (split the caret's line block), as hashtagPicker.test.ts's does.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeAll, afterAll, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';
import { isCommittableDate } from './PropertyPicker.svelte';
import { accounts, currentAccount, notes, selectedNote } from '../stores/notes';
import { dateChoices } from '../editor/propertyTrigger';
import type { Note } from '../types';

const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const VOCAB = [
  ['Status', 3, ['active', 'draft', 'Active-ish']],
  ['Owner', 2, ['Kai']],
  ['Notes', 1, []],
];

const RECT = { left: 40, top: 100, right: 60, bottom: 118, width: 20, height: 18, x: 40, y: 100, toJSON() {} } as DOMRect;
let calls: { cmd: string; arg?: string }[] = [];

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
      const el = document.querySelector<HTMLElement>('.editor-body');
      if (!el || !sel.rangeCount) return false;
      if (cmd !== 'insertText' && cmd !== 'insertParagraph') return true;
      calls.push({ cmd, arg });
      const r = sel.getRangeAt(0);
      if (cmd === 'insertParagraph') {
        let block: Node = r.startContainer;
        while (block.parentNode && block.parentNode !== el) block = block.parentNode;
        const tail = document.createRange();
        tail.setStart(r.startContainer, r.startOffset);
        tail.setEnd(block, block.childNodes.length);
        const next = document.createElement('div');
        next.appendChild(tail.extractContents());
        (block as Element).after(next);
        placeCaret(next, 0);
      } else {
        if (!r.collapsed) r.deleteContents();
        const node = r.startContainer;
        if (node.nodeType === 3) {
          (node as Text).insertData(r.startOffset, arg!);
          placeCaret(node, r.startOffset + arg!.length);
        } else {
          const t = document.createTextNode(arg!);
          r.insertNode(t);
          placeCaret(t, t.data.length);
        }
      }
      el.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    },
  });
  Object.defineProperty(document, 'queryCommandValue', { configurable: true, writable: true, value: () => 'div' });
  Range.prototype.getBoundingClientRect = () => RECT;
});
afterAll(() => {
  delete (Range.prototype as unknown as Record<string, unknown>).getBoundingClientRect;
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

async function mountEditor(body: string) {
  accounts.set([GMAIL as never]);
  currentAccount.set(GMAIL.id);
  const n = {
    uuid: 'uuid-props',
    id: 'msg-1',
    account_id: GMAIL.id,
    title: 'A note',
    body_html: body,
    date: '2026-11-19T00:00:00Z',
    label: 'Notes',
    local_version: 1,
  } as Note;
  notes.set([n]);
  selectedNote.set(n);
  app = mount(NoteEditor, { target });
  await settle();
}

const editor = () => target.querySelector<HTMLElement>('.editor-body')!;
const picker = () => target.querySelector<HTMLElement>('[data-testid="property-picker"]');
const options = () => Array.from(picker()?.querySelectorAll<HTMLButtonElement>('[role="option"]') ?? []).map((b) => b.dataset.value);
const vocabCalls = () => invoke.mock.calls.filter(([c]) => c === 'property_vocabulary');
const lines = () => Array.from(editor().children).map((c) => c.textContent);

/** Focus the body with the caret at the end of `node` (a text node) or inside an element. */
function caretIn(node: Node, offset = node.nodeType === 3 ? (node as Text).data.length : 0) {
  editor().focus();
  placeCaret(node, offset);
  document.dispatchEvent(new Event('selectionchange'));
  flushSync();
}

async function type(text: string) {
  for (const ch of Array.from(text)) document.execCommand('insertText', false, ch);
  flushSync();
  await settle();
}

function key(k: string) {
  const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true });
  editor().dispatchEvent(e);
  flushSync();
  return e;
}

function click(el: Element) {
  el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 }));
  flushSync();
}

beforeEach(() => {
  calls = [];
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'note_persistence') return Promise.resolve(null);
    if (cmd === 'property_vocabulary') return Promise.resolve(VOCAB);
    return Promise.resolve([]);
  });
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(() => {
  if (app) unmount(app);
  app = null;
  target.remove();
  selectedNote.set(null);
  notes.set([]);
});

describe('opening a note writes nothing', () => {
  it('a near miss and a slashed date are painted, never rewritten', async () => {
    const body = '<div>Due: 25/11</div><div>Status:active</div><div>text</div>';
    await mountEditor(body);
    expect(editor().innerHTML).toBe(body);
    expect(calls).toEqual([]);
    expect(vocabCalls()).toEqual([]);
  });
});

describe('key and value pickers', () => {
  it('a key typed on the line that extends the block lists keys; a pick writes `Key: ` and lists its values', async () => {
    await mountEditor('<div>Owner: Kai</div><div><br></div><div>rest</div>');
    caretIn(editor().children[1], 0);
    await type('St');
    expect(options()).toEqual(['Status']);
    expect(vocabCalls()).toHaveLength(1);
    key('Enter');
    await settle();
    expect(lines()[1]).toBe('Status: ');
    // Straight on to the values, with the vocabulary already read.
    expect(options()).toEqual(['active', 'draft', 'Active-ish']);
    expect(vocabCalls()).toHaveLength(1);
    // Nothing typed: Enter is the editor's until the highlight is moved.
    expect(key('Enter').defaultPrevented).toBe(false);
    key('ArrowDown');
    key('ArrowUp');
    key('Enter');
    await settle();
    expect(lines()[1]).toBe('Status: active');
    // The finished value closes the list — `Active-ish` still starts with it,
    // but the next Enter is a new line.
    expect(picker()).toBeNull();
    expect(key('Enter').defaultPrevented).toBe(false);
  });

  it('with no block yet, Enter after a word that prefixes a key is a new line (ruling R13)', async () => {
    await mountEditor('<div><br></div><div>rest</div>');
    caretIn(editor().children[0], 0);
    await type('Note');
    expect(options()).toEqual(['Notes']);
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(lines()).toEqual(['Note', 'rest']);
    expect(calls.filter((c) => c.arg?.includes(':'))).toEqual([]);
    // Moving the highlight is the explicit choice: then Enter picks.
    key('ArrowDown');
    expect(key('Enter').defaultPrevented).toBe(true);
    await settle();
    expect(lines()).toEqual(['Notes: ', 'rest']);
  });

  it('lists nothing on a line below the block', async () => {
    await mountEditor('<div>Owner: Kai</div><div>notes</div><div><br></div>');
    caretIn(editor().children[2], 0);
    await type('St');
    expect(picker()).toBeNull();
    expect(vocabCalls()).toEqual([]);
  });

  it('Esc closes the list for that text', async () => {
    await mountEditor('<div><br></div>');
    caretIn(editor().children[0], 0);
    await type('St');
    expect(options()).toEqual(['Status']);
    key('Escape');
    expect(picker()).toBeNull();
  });
});

describe('the date picker', () => {
  it('opens after `📅 ` on a task row and writes the picked date', async () => {
    await mountEditor('<div><input type="checkbox">&nbsp;ship</div>');
    const row = editor().children[0];
    caretIn(row.lastChild!);
    await type(' 📅 ');
    const choices = dateChoices(new Date());
    expect(options()).toEqual(choices.map((c) => c.value));
    expect(target.querySelector('[data-testid="property-date-input"]')).not.toBeNull();
    click(picker()!.querySelector(`[data-value="${choices[1].value}"]`)!);
    await settle();
    expect(row.textContent).toBe(` ship 📅 ${choices[1].value}`);
    expect(picker()).toBeNull();
  });

  it('an untouched date list never takes Enter', async () => {
    await mountEditor('<div>ship</div>');
    caretIn(editor().children[0].firstChild!);
    await type(' 📅 ');
    expect(picker()).not.toBeNull();
    expect(key('Enter').defaultPrevented).toBe(false);
    expect(editor().textContent).toBe('ship 📅 ');
  });

  it('the native date field writes its date where the caret was', async () => {
    await mountEditor('<div><input type="checkbox">&nbsp;ship</div>');
    const row = editor().children[0];
    caretIn(row.lastChild!);
    await type(' due: ');
    const input = target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!;
    // Focus moves into the field: the list stays open for it.
    input.focus();
    flushSync();
    expect(picker()).not.toBeNull();
    input.value = '2026-12-01';
    input.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(row.textContent).toBe(' ship due: 2026-12-01');
  });

  // Task 12 M1: Chromium/WebView and WebKit fire `change` per segment edit,
  // so typing a year digit by digit passes through `0002-12-01`, and a day
  // through `…-02`. Only a commit may reach the body: Enter after typing, or a
  // change with no typing behind it (the calendar popup, the phone's dialog).
  it('a typed date is written only on Enter, never a half-typed value', async () => {
    await mountEditor('<div><input type="checkbox">&nbsp;ship</div>');
    const row = editor().children[0];
    caretIn(row.lastChild!);
    await type(' due: ');
    const input = target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!;
    input.focus();
    flushSync();
    for (const [k, v] of [['2', '0002-12-01'], ['0', '0020-12-01'], ['2', '0202-12-01'], ['6', '2026-12-01'], ['0', '2026-12-01']]) {
      input.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true }));
      input.value = v;
      input.dispatchEvent(new Event('change', { bubbles: true }));
      await settle();
      expect(row.textContent, `nothing written at ${v}`).toBe(' ship due: ');
      expect(picker(), 'the picker stays open while typing').not.toBeNull();
    }
    input.value = '2026-12-03';
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await settle();
    expect(row.textContent).toBe(' ship due: 2026-12-03');
  });

  it('an untyped change writes only a whole, real date', async () => {
    await mountEditor('<div><input type="checkbox">&nbsp;ship</div>');
    const row = editor().children[0];
    caretIn(row.lastChild!);
    await type(' due: ');
    const input = target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!;
    input.focus();
    flushSync();
    input.value = '0002-12-01';
    input.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(row.textContent).toBe(' ship due: ');
    input.value = '2026-12-02';
    input.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(row.textContent).toBe(' ship due: 2026-12-02');
  });
});

describe('isCommittableDate', () => {
  it('takes a whole real date from 1900 on, nothing partial', () => {
    expect(isCommittableDate('2026-11-25')).toBe(true);
    expect(isCommittableDate('2028-02-29')).toBe(true);
    for (const v of ['', '2026-11', '0002-11-25', '1899-12-31', '2026-02-30', '2026-13-01', '20261-11-25']) {
      expect(isCommittableDate(v), v).toBe(false);
    }
  });
});

describe('toolbar', () => {
  const button = (id: string) => target.querySelector<HTMLButtonElement>(`.toolbar-actions > [data-fmt="${id}"]`)!;

  it('Add property puts `Key: ` on body line 0, `Key` selected, and a pick replaces it', async () => {
    await mountEditor('<div>Hello</div>');
    caretIn(editor().children[0].firstChild!);
    click(button('property'));
    await settle();
    expect(lines()).toEqual(['Key: ', 'Hello']);
    expect(window.getSelection()!.toString()).toBe('Key');
    expect(options()).toEqual(['Status', 'Owner', 'Notes']);
    // One read for the list it opened, none for the moment `Key: ` was typed.
    expect(vocabCalls()).toHaveLength(1);
    // The list was asked for: Enter takes the top row.
    key('Enter');
    await settle();
    expect(lines()).toEqual(['Status: ', 'Hello']);
    expect(options()).toEqual(['active', 'draft', 'Active-ish']);
  });

  it('Due date replaces the date on the caret\'s task row: never a second marker', async () => {
    await mountEditor('<div><input type="checkbox">&nbsp;ship 📅 2026-11-20</div>');
    const row = editor().children[0];
    caretIn(row.lastChild!, 3);
    expect(button('due').getAttribute('aria-disabled')).toBeNull();
    click(button('due'));
    await settle();
    const today = dateChoices(new Date())[0].value;
    expect(target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!.value).toBe('2026-11-20');
    key('Enter'); // the list was asked for
    await settle();
    expect(row.textContent).toBe(` ship 📅 ${today}`);
  });

  it('Due date is off a task row, and does nothing there', async () => {
    await mountEditor('<div>plain</div>');
    caretIn(editor().children[0].firstChild!);
    expect(button('due').getAttribute('aria-disabled')).toBe('true');
    click(button('due'));
    await settle();
    expect(picker()).toBeNull();
    expect(editor().innerHTML).toBe('<div>plain</div>');
  });
});

describe('fix-it', () => {
  const fixIt = () => target.querySelector<HTMLElement>('[data-testid="property-fixit"]');

  it('a click on the line offers the fix; only clicking the offer rewrites it', async () => {
    await mountEditor('<div>Status:active</div><div>text</div>');
    caretIn(editor().children[0].firstChild!, 3);
    click(editor());
    expect(fixIt()!.textContent).toContain('add a space after “Status:”');
    expect(editor().innerHTML).toBe('<div>Status:active</div><div>text</div>');
    click(fixIt()!.querySelector('button')!);
    await settle();
    expect(lines()).toEqual(['Status: active', 'text']);
    expect(fixIt()).toBeNull();
  });

  it('a space fix keeps the link on the line', async () => {
    await mountEditor('<div>Source:<a href="https://x.com/a">Article</a></div><div>text</div>');
    caretIn(editor().children[0].firstChild!, 3);
    click(editor());
    click(fixIt()!.querySelector('button')!);
    await settle();
    expect(editor().children[0].outerHTML).toBe('<div>Source: <a href="https://x.com/a">Article</a></div>');
  });

  it('offers only the space for a ratio, and nothing for a time', async () => {
    await mountEditor('<div>Ratio:1/2</div>');
    caretIn(editor().children[0].firstChild!, 2);
    click(editor());
    expect(fixIt()!.querySelector('.prop-fixit-text')!.textContent).toBe('add a space after “Ratio:”');
    unmount(app!);
    app = null;
    await mountEditor('<div>Meeting at 10:30</div>');
    caretIn(editor().children[0].firstChild!, 2);
    click(editor());
    expect(fixIt()).toBeNull();
  });
});

describe('the date field', () => {
  it('Esc closes the list and puts the caret back in the text', async () => {
    await mountEditor('<div>ship</div>');
    caretIn(editor().children[0].firstChild!);
    await type(' 📅 ');
    const input = target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!;
    input.focus();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(picker()).toBeNull();
    expect(document.activeElement).toBe(editor());
    expect(editor().textContent).toBe('ship 📅 ');
  });

  it('focus leaving the field for elsewhere closes the list', async () => {
    await mountEditor('<div>ship</div>');
    caretIn(editor().children[0].firstChild!);
    await type(' 📅 ');
    const input = target.querySelector<HTMLInputElement>('[data-testid="property-date-input"]')!;
    input.focus();
    const elsewhere = document.createElement('button');
    document.body.appendChild(elsewhere);
    elsewhere.focus();
    flushSync();
    expect(picker()).toBeNull();
    elsewhere.remove();
  });
});
