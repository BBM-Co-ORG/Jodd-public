// @vitest-environment jsdom
//
// The desktop and tablet format toolbar (editor design pass, step 5):
//   - every button has an accessible name, not only the icon ones: "B", "Aa"
//     and "1." are glyphs, not names;
//   - the marks the caret already has read as pressed (aria-pressed), from
//     document.queryCommandState on every selection change in the body;
//   - three groups, inline | block | insert, each new group's first button
//     drawing the divider;
//   - Aa opens the same Title / Heading / Subheading / Body sheet as the phone;
//   - no formatting row on a note typing cannot change.
// Mounted: the pressed state and the row's presence change across reactive
// turns, and only the component shows that.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readFileSync } from 'node:fs';
import { selectedNote, notes, accounts } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const NOTE: Note = {
  uuid: 'uuid-format-toolbar',
  id: 'msg-1',
  account_id: ACCOUNT.id,
  title: 'Toolbar',
  body_html: '<div>plain <b>bold</b> and <code>code</code></div>',
  date: '2026-11-07T00:00:00Z',
  label: 'Notes',
  local_version: 1,
} as Note;

let host: HTMLElement;
let app: ReturnType<typeof mount> | null = null;
let commandState: Record<string, boolean> = {};

async function render(note: Note = NOTE) {
  notes.set([note]);
  selectedNote.set(note);
  host = document.createElement('div');
  document.body.appendChild(host);
  app = mount(NoteEditor, { target: host });
  flushSync();
  await tick();
  await tick();
  flushSync();
}

const body = () => host.querySelector<HTMLElement>('.editor-body')!;
const rowButtons = () => Array.from(host.querySelectorAll<HTMLButtonElement>('.toolbar-actions > .fmt-btn[data-fmt]'));
const rowButton = (id: string) => host.querySelector<HTMLButtonElement>(`.toolbar-actions > .fmt-btn[data-fmt="${id}"]`)!;

/** Put the caret inside `node` (in the body) and fire selectionchange. */
function caretIn(node: Node) {
  const r = document.createRange();
  r.setStart(node, 0);
  r.collapse(true);
  const sel = window.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(r);
  document.dispatchEvent(new Event('selectionchange'));
  flushSync();
}

beforeEach(() => {
  commandState = {};
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
  accounts.set([ACCOUNT as never]);
  (document as unknown as Record<string, unknown>).execCommand = () => true;
  (document as unknown as Record<string, unknown>).queryCommandValue = () => '';
  (document as unknown as Record<string, unknown>).queryCommandState = (cmd: string) => !!commandState[cmd];
});

afterEach(() => {
  if (app) unmount(app);
  app = null;
  host?.remove();
  selectedNote.set(null);
  notes.set([]);
  delete (document as unknown as Record<string, unknown>).execCommand;
  delete (document as unknown as Record<string, unknown>).queryCommandValue;
  delete (document as unknown as Record<string, unknown>).queryCommandState;
});

describe('the format toolbar', () => {
  it('names every button, in three groups: inline | block | insert', async () => {
    await render();
    expect(rowButtons().map((b) => [b.dataset.fmt, b.getAttribute('aria-label')])).toEqual([
      ['bold', 'Bold'], ['italic', 'Italic'], ['underline', 'Underline'], ['strike', 'Strikethrough'], ['code', 'Inline code'],
      ['heading', 'Text style'], ['quote', 'Blockquote'], ['bullets', 'Bullet list'], ['numbered', 'Numbered list'], ['task', 'Checklist'],
      ['link', 'Link'],
    ]);
    // Each group after the first opens with the divider.
    expect(rowButtons().filter((b) => b.classList.contains('fmt-group-start')).map((b) => b.dataset.fmt)).toEqual(['heading', 'link']);
    // Aa, not a bare "H" that only ever made a Heading 2.
    expect(rowButton('heading').textContent!.trim()).toBe('Aa');
  });

  // Re-critique after #296: eleven separate Tab stops before the title. Now
  // one toolbar: a single Tab stop, arrow keys inside, Home and End.
  it('is one toolbar: a single Tab stop, arrow keys move along it', async () => {
    await render();
    const bar = host.querySelector<HTMLElement>('.toolbar-actions')!;
    expect(bar.getAttribute('role')).toBe('toolbar');
    expect(bar.getAttribute('aria-label')).toBe('Formatting');
    const stops = () => rowButtons().filter((b) => b.tabIndex === 0).map((b) => b.dataset.fmt);
    expect(stops()).toEqual(['bold']);
    rowButton('bold').focus();
    rowButton('bold').dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true }));
    expect(document.activeElement).toBe(rowButton('italic'));
    expect(stops()).toEqual(['italic']);
    rowButton('italic').dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true, cancelable: true }));
    expect(document.activeElement).toBe(rowButton('link'));
    rowButton('link').dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true, cancelable: true }));
    expect(document.activeElement).toBe(rowButton('bold'));
  });

  it('marks what the caret already has as pressed, and follows the caret', async () => {
    await render();
    const bold = rowButton('bold');
    const code = rowButton('code');
    // Toggles say false until there is a caret in the body; the rest say nothing.
    expect(bold.getAttribute('aria-pressed')).toBe('false');
    for (const id of ['heading', 'task', 'link']) expect(rowButton(id).hasAttribute('aria-pressed'), id).toBe(false);

    commandState = { bold: true };
    caretIn(body().querySelector('b')!.firstChild!);
    expect(bold.getAttribute('aria-pressed')).toBe('true');
    expect(bold.classList.contains('fmt-pressed')).toBe(true);
    expect(rowButton('italic').getAttribute('aria-pressed')).toBe('false');
    expect(code.getAttribute('aria-pressed')).toBe('false');

    commandState = {};
    caretIn(body().querySelector('code')!.firstChild!);
    expect(bold.getAttribute('aria-pressed')).toBe('false');
    expect(code.getAttribute('aria-pressed')).toBe('true');
  });

  it('a caret outside the body presses nothing', async () => {
    await render();
    commandState = { bold: true };
    const outside = document.createElement('p');
    outside.textContent = 'elsewhere';
    document.body.appendChild(outside);
    caretIn(outside.firstChild!);
    expect(rowButton('bold').getAttribute('aria-pressed')).toBe('false');
    outside.remove();
  });

  it('Aa opens the Text style sheet with Apple Notes\' names', async () => {
    await render();
    caretIn(body().querySelector('div')!.firstChild!);
    rowButton('heading').click();
    flushSync();
    await tick();
    flushSync();
    const dialog = host.querySelector('dialog')!;
    expect(dialog).not.toBeNull();
    expect(Array.from(dialog.querySelectorAll('.text-style')).map((b) => b.textContent!.trim()))
      .toEqual(['Title', 'Heading', 'Subheading', 'Body']);
  });

  it('a note typing cannot change has no formatting row, but keeps the toolbar and ⋯', async () => {
    await render({ ...NOTE, push_blocked_reason: 'permanent: open it in Apple Notes to edit', push_blocked_by_remote: true } as Note);
    expect(host.querySelector('.editor-toolbar')).not.toBeNull();
    expect(rowButtons()).toHaveLength(0);
    expect(host.querySelector('button[aria-label="More formatting"]')).toBeNull();
    // A remote-locked note's state is the metadata chip ("Read-only"); the
    // toolbar's right side keeps ⋯.
    expect(host.querySelector('.toolbar-right')).not.toBeNull();
    expect(host.querySelector('.editor-toolbar button[aria-label="Note actions"]')).not.toBeNull();
  });

  it('the save word has a fixed width, so the row does not reflow while typing', () => {
    const css = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
    const style = css.slice(css.indexOf('<style>'));
    const i = style.indexOf('.toolbar-right .save-status:not(.external) {');
    expect(i).toBeGreaterThanOrEqual(0);
    expect(style.slice(i, style.indexOf('}', i))).toMatch(/min-width:\s*7em/);
  });

  // Step 7 polish: the find bar's ‹ › were glyph buttons with only a title,
  // and the match count changed silently for a screen reader.
  it('the find bar names ‹ › and announces the match count', async () => {
    await render();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', ctrlKey: true }));
    flushSync();
    await tick();
    flushSync();
    const bar = host.querySelector('.find-bar')!;
    expect(bar).not.toBeNull();
    const names = Array.from(bar.querySelectorAll('button.find-btn')).map((b) => b.getAttribute('aria-label'));
    expect(names).toEqual(['Previous match', 'Next match']);
    const count = bar.querySelector('.find-count')!;
    expect(count.getAttribute('role')).toBe('status');
    expect(count.getAttribute('aria-live')).toBe('polite');
    // The search glyph is the shared Icon, not an inline one-off SVG.
    expect(bar.querySelector('.find-icon svg')).not.toBeNull();
    // Esc closes it from any control in the bar, not only from the inputs.
    const aa = bar.querySelector<HTMLButtonElement>('button.find-opt')!;
    aa.focus();
    aa.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(host.querySelector('.find-bar')).toBeNull();
  });

  // Final re-critique: Tab always indented, so keyboard focus could not
  // leave the body (WCAG 2.1.2). Esc, then Tab, now leaves.
  it('Tab indents; Esc then Tab leaves the note, and the body says so', async () => {
    await render();
    const tab = () => {
      const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
      body().dispatchEvent(ev);
      return ev.defaultPrevented;
    };
    caretIn(body().querySelector('div')!.firstChild!);
    expect(tab()).toBe(true); // indents
    body().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    expect(tab()).toBe(false); // the browser moves focus
    expect(tab()).toBe(true); // released once only
    body().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    body().dispatchEvent(new KeyboardEvent('keydown', { key: 'a', bubbles: true, cancelable: true }));
    expect(tab()).toBe(true); // typing re-arms Tab as indent
    const hintId = body().getAttribute('aria-describedby')!;
    expect(host.querySelector(`#${hintId}`)!.textContent).toBe('Tab indents. To leave the note, press Escape, then Tab.');
  });

  it('a find match in the title marks the title', async () => {
    await render();
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', ctrlKey: true }));
    flushSync();
    await tick();
    const input = host.querySelector<HTMLInputElement>('.find-input')!;
    const title = host.querySelector<HTMLElement>('.title-input')!;
    expect(title.classList.contains('find-current')).toBe(false);
    input.value = 'Toolbar'; // the note's title; not in its body
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    flushSync();
    await tick();
    flushSync();
    expect(title.classList.contains('find-current')).toBe(true);
  });
});
