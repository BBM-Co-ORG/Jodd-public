// @vitest-environment jsdom
//
// Task 5.3 (spec 2026-10-20 §6.2): tags move from the always-visible row under
// the title into a popover opened from the metadata line's #N. The add and
// remove paths are NoteEditor's own (`addNormalizedTag` → `appendTagToBody`,
// `removeTag` → `replaceRanges(findTextRanges(hashMarkMatches(tag)), '')`),
// so the editor is MOUNTED here: what a test sees is the body a save is built
// from (gotchas #28, #37). On iCloud a tag is an object Jodd cannot create
// (gotcha #38): the popover only lists them.
//
// jsdom has no execCommand, no Selection.modify and no layout (innerText).
// The stubs below do the least those commands do, and record every call: an
// edit that went through execCommand is one WebKit's undo history holds
// (scripts/webkit-editor-harness, tag-add*.txt / tag-remove.txt).
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeAll, afterAll, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/viewport')>();
  return { ...orig, androidLayoutMode: writable('tablet') };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import TagsPopover from './TagsPopover.svelte';
import NoteActionsButton from './NoteActionsButton.svelte';
import NoteEditor from './NoteEditor.svelte';
import LayoutSwapEditor from './__fixtures__/LayoutSwapEditor.svelte';
import { isAndroid as isAndroidRO } from '../stores/platform';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import { accounts, currentAccount, notes, selectedNote, capabilitiesByAccount, setAccountCapabilities, setAccountNoteTags } from '../stores/notes';
import type { Note } from '../types';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const layout = layoutRO as unknown as Writable<string>;

// ─── editing stubs ─────────────────────────────────────────────────────────
type Call = { cmd: string; arg?: string; selected: string };
let calls: Call[] = [];

function lastText(root: Node): Text | null {
  let last: Text | null = null;
  const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  for (let n = w.nextNode() as Text | null; n; n = w.nextNode() as Text | null) if (n.data) last = n;
  return last;
}

const BLOCK = /^(DIV|P|LI|BR|H[1-6]|BLOCKQUOTE|PRE)$/;
beforeAll(() => {
  Object.defineProperty(document, 'execCommand', {
    configurable: true,
    writable: true,
    value: (cmd: string, _ui?: boolean, arg?: string) => {
      const sel = window.getSelection()!;
      const r = sel.getRangeAt(0);
      calls.push({ cmd, arg, selected: r.toString() });
      if (cmd === 'insertText') {
        r.deleteContents();
        const t = document.createTextNode(arg ?? '');
        r.insertNode(t);
        const c = document.createRange();
        c.setStartAfter(t);
        c.collapse(true);
        sel.removeAllRanges();
        sel.addRange(c);
        return true;
      }
      if (cmd === 'delete') {
        r.deleteContents();
        return true;
      }
      return false;
    },
  });
  // What appendTagToBody asks of it: the end of the document, then the last
  // paragraph (here: the last text node, which is the last line).
  Object.defineProperty(Selection.prototype, 'modify', {
    configurable: true,
    writable: true,
    value(this: Selection, alter: string, _dir: string, unit: string) {
      const host = document.querySelector('.editor-body')!;
      const last = lastText(host);
      if (!last) return;
      if (alter === 'move' && unit === 'documentboundary') this.collapse(last, last.length);
      else if (alter === 'extend' && unit === 'paragraphboundary') this.setBaseAndExtent(last, last.length, last, 0);
    },
  });
  // Block boundaries as line breaks, so `#tag` opening a line is a tag.
  Object.defineProperty(HTMLElement.prototype, 'innerText', {
    configurable: true,
    get(this: HTMLElement) {
      const parts: string[] = [];
      const walk = (n: Node) => {
        for (const c of Array.from(n.childNodes)) {
          if (c.nodeType === 3) parts.push((c as Text).data);
          else if (c.nodeType === 1) {
            if (BLOCK.test((c as Element).tagName)) parts.push('\n');
            walk(c);
          }
        }
      };
      walk(this);
      return parts.join('');
    },
  });
});
afterAll(() => {
  delete (document as unknown as Record<string, unknown>).execCommand;
  delete (Selection.prototype as unknown as Record<string, unknown>).modify;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>).innerText;
});

// ─── fixtures ──────────────────────────────────────────────────────────────
const GMAIL = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' };
const ICLOUD = { id: 'icloud:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'icloud', status: 'active' };

// A WHOLE-DOCUMENT body, as Apple and Jodd store it (gotcha #37): an append
// must land inside <body>, never after </html>.
const doc = (inner: string) =>
  `<html><head></head><body style="overflow-wrap: break-word; -webkit-nbsp-mode: space; line-break: after-white-space;">${inner}</body></html>`;

const TAGGED: Note = {
  uuid: 'uuid-tagged', id: 'msg-tagged', account_id: GMAIL.id, title: 'Groceries',
  body_html: doc('<div>Groceries</div><div>milk and eggs</div><div>#home</div>'),
  date: '2026-10-20T00:00:00Z', label: 'Notes',
} as Note;
const OTHER: Note = {
  uuid: 'uuid-other', id: 'msg-other', account_id: GMAIL.id, title: 'Other',
  body_html: doc('<div>Other</div><div>see #errand</div>'), date: '2026-10-19T00:00:00Z', label: 'Notes',
} as Note;
const APPLE: Note = {
  uuid: '4de5ef24-d5ad-4b7a-9ca0-db923e55634e', id: '4de5ef24-d5ad-4b7a-9ca0-db923e55634e', account_id: ICLOUD.id,
  title: 'Trip', body_html: doc('<div>Trip</div><div>pack <span data-jodd-inline="hashtag">#travel</span></div>'),
  date: '2026-10-20T00:00:00Z', label: 'Notes',
} as Note;
const APPLE_UNTAGGED: Note = { ...APPLE, uuid: 'b1e5ef24-d5ad-4b7a-9ca0-db923e55634e', id: 'b1e5ef24-d5ad-4b7a-9ca0-db923e55634e', body_html: doc('<div>Trip</div><div>pack</div>') } as Note;

const notIpc = () => invoke.mock.calls.filter((c) => c[0] !== 'platform_name');

let target: HTMLElement;
let app: ReturnType<typeof mount> | null = null;
let headerButton: ReturnType<typeof mount> | null = null;

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

async function mountEditor(note: Note, account: typeof GMAIL = GMAIL) {
  accounts.set([account as never]);
  currentAccount.set(account.id);
  notes.set([note, OTHER]);
  selectedNote.set(note);
  app = mount(NoteEditor, { target });
  await settle();
}

const tagsBtn = () => target.querySelector<HTMLButtonElement>('.meta-line .meta-tags');
const pop = () => document.querySelector<HTMLElement>('[data-testid="tags-popover"]');
const tagInput = () => pop()?.querySelector<HTMLInputElement>('.tag-input') ?? null;
const rows = () => Array.from(pop()?.querySelectorAll('.tags-name') ?? []).map((e) => e.textContent?.trim());
const body = () => target.querySelector('.editor-body') as HTMLElement;

function openTags() {
  tagsBtn()!.click();
  flushSync();
}

function type(input: HTMLInputElement, text: string) {
  input.value = text;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}
function key(el: HTMLElement, k: string, init: KeyboardEventInit = {}) {
  el.dispatchEvent(new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init }));
  flushSync();
}

beforeEach(() => {
  calls = [];
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'note_persistence') return Promise.resolve(null);
    if (cmd === 'platform_name') return Promise.resolve('macos');
    return Promise.resolve([]);
  });
  capabilitiesByAccount.set({});
  setAccountNoteTags(GMAIL.id, [{ uuid: OTHER.uuid, tag: 'errand' }, { uuid: OTHER.uuid, tag: 'work' }]);
  isAndroid.set(false);
  layout.set('tablet');
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(async () => {
  if (headerButton) unmount(headerButton);
  headerButton = null;
  if (app) unmount(app);
  app = null;
  target.remove();
  selectedNote.set(null);
  notes.set([]);
  // A phone sheet pops its history entry asynchronously on unmount (R8).
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

// ─── NoteEditor, mounted ───────────────────────────────────────────────────
describe('Tags popover in the editor', () => {
  it('the tag row under the title is gone', async () => {
    await mountEditor(TAGGED);
    expect(target.querySelector('.tag-row')).toBeNull();
    expect(target.querySelector('.tag-input')).toBeNull();
    expect(target.querySelector('.tag-hint')).toBeNull();
    expect(pop()).toBeNull();
  });

  it('add via Enter calls appendTagToBody path (body gains #tag, undoable)', async () => {
    await mountEditor(TAGGED);
    openTags();
    const input = tagInput()!;
    expect(input).not.toBeNull();
    // Desktop: opening puts the caret in the field, as clicking the old row's did.
    expect(document.activeElement).toBe(input);
    invoke.mockClear();

    type(input, 'Work');
    key(input, 'Enter');

    // One edit command, on the trailing line of tags — the undoable path.
    expect(calls).toEqual([{ cmd: 'insertText', arg: ' #work', selected: '' }]);
    expect(body().textContent).toContain('#home #work');
    // The editor gave focus back to the field (bodyEdits takeFocus).
    expect(document.activeElement).toBe(input);
    expect(input.value).toBe('');
    // refreshBodyTags → the popover and #N agree with the body at once.
    expect(rows()).toEqual(['#home', '#work']);
    expect(tagsBtn()!.textContent).toContain('#2');
    // onInput → the store holds the whole document, the tag inside <body> (#37).
    const stored = get(selectedNote)!.body_html;
    expect(stored.startsWith('<html><head></head><body')).toBe(true);
    expect(stored.endsWith('#home #work</div></body></html>')).toBe(true);
    expect(stored.match(/<\/html>/g)).toHaveLength(1);
    // Local-first: no IPC for the add itself; the content save is debounced.
    expect(notIpc()).toEqual([]);
  });

  it('suggestions come from the account\'s tags, minus the note\'s own', async () => {
    await mountEditor(TAGGED);
    openTags();
    const input = tagInput()!;
    type(input, 'er');
    const options = () => Array.from(pop()!.querySelectorAll('[role="option"]')).map((o) => o.textContent?.trim());
    expect(options()).toEqual(['#errand']);
    key(input, 'ArrowDown');
    key(input, 'Enter');
    expect(calls.map((c) => [c.cmd, c.arg])).toEqual([['insertText', ' #errand']]);
    expect(rows()).toEqual(['#home', '#errand']);
  });

  it('remove deletes only the #', async () => {
    const note = { ...TAGGED, body_html: doc('<div>Groceries</div><div>call about #work today</div>') } as Note;
    await mountEditor(note);
    openTags();
    expect(rows()).toEqual(['#work']);
    pop()!.querySelector<HTMLButtonElement>('button[aria-label="Remove tag work"]')!.click();
    flushSync();
    expect(calls).toEqual([{ cmd: 'delete', arg: undefined, selected: '#' }]);
    expect(body().textContent).toContain('call about work today');
    expect(rows()).toEqual([]);
    // Focus stays in the popover rather than on a removed row.
    expect(document.activeElement).toBe(tagInput());
  });

  it('iCloud: no input, no ×, hint shown', async () => {
    await mountEditor(APPLE, ICLOUD);
    expect(tagsBtn()?.textContent).toContain('#1');
    openTags();
    expect(rows()).toEqual(['#travel']);
    expect(tagInput()).toBeNull();
    expect(pop()!.querySelector('button[aria-label^="Remove tag"]')).toBeNull();
    expect(pop()!.textContent).toContain('Add or remove tags in Apple Notes.');
    const more = pop()!.querySelector('details')!;
    expect(more.querySelector('summary')?.textContent?.trim()).toBe('Learn more');
    expect(more.textContent).toContain("Apple Notes stores tags as special objects Jodd can't create yet.");
    // The permanent hint under the title is gone.
    expect(target.textContent).not.toContain('Tags on this account are added and removed in Apple Notes.');
    expect(calls).toEqual([]);
  });

  it('#N hidden when 0 tags and not editable', async () => {
    await mountEditor(APPLE_UNTAGGED, ICLOUD);
    expect(tagsBtn()).toBeNull();
  });

  it('#N shows #0 when there are no tags but tags are editable', async () => {
    await mountEditor({ ...TAGGED, body_html: doc('<div>Groceries</div><div>milk</div>') } as Note);
    expect(tagsBtn()?.textContent).toContain('#0');
    openTags();
    expect(rows()).toEqual([]);
    expect(tagInput()).not.toBeNull();
  });

  it('closing returns focus to #N', async () => {
    await mountEditor(TAGGED);
    openTags();
    expect(document.activeElement).toBe(tagInput());
    key(tagInput()!, 'Escape');
    expect(pop()).toBeNull();
    expect(document.activeElement).toBe(tagsBtn());
  });

  it('#N toggles the popover', async () => {
    await mountEditor(TAGGED);
    openTags();
    expect(pop()).not.toBeNull();
    openTags();
    expect(pop()).toBeNull();
  });

  it('switching notes closes it in the same pass (gotcha #28), and it stays closed', async () => {
    await mountEditor(TAGGED);
    openTags();
    expect(pop()).not.toBeNull();
    selectedNote.set(OTHER);
    flushSync();
    expect(pop()).toBeNull();
    selectedNote.set(TAGGED);
    flushSync();
    expect(pop()).toBeNull();
  });

  it('⋯ Tags… opens the popover, anchored to #N', async () => {
    await mountEditor(TAGGED);
    target.querySelector<HTMLButtonElement>('.editor-toolbar button[aria-label="Note actions"]')!.click();
    flushSync();
    const item = Array.from(document.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')).find(
      (b) => b.textContent?.trim() === 'Tags…',
    )!;
    item.click();
    await vi.waitFor(() => expect(pop()).not.toBeNull());
    key(tagInput()!, 'Escape');
    expect(document.activeElement).toBe(tagsBtn());
  });

  // Ported from noteEditorPushBlocked.test.ts: a note locked by the remote's
  // own document takes no tag edits either — the field is absent, inside the
  // popover as it was in the row.
  it('a note blocked by the remote document lists its tags read-only', async () => {
    const blocked = {
      ...TAGGED,
      push_blocked_reason: 'permanent: the remote document changed shape',
      push_blocked_by_remote: true,
    } as Note;
    await mountEditor(blocked);
    expect(body().getAttribute('contenteditable')).toBe('false');
    openTags();
    expect(rows()).toEqual(['#home']);
    expect(tagInput()).toBeNull();
    expect(pop()!.querySelector('button[aria-label^="Remove tag"]')).toBeNull();
    // The Apple-objects reason is iCloud's; it is not offered here.
    expect(pop()!.querySelector('details')).toBeNull();
  });

  it('an ordinary block leaves the tag field in place', async () => {
    const blocked = {
      ...TAGGED,
      push_blocked_reason: "permanent: no Exchange folder id for 'Notes'",
      push_blocked_by_remote: false,
    } as Note;
    await mountEditor(blocked);
    openTags();
    expect(tagInput()).not.toBeNull();
  });

  it('a note not saved yet lists its tags with no line about Apple Notes', async () => {
    await mountEditor({ ...TAGGED, uuid: 'tmp:new-note', id: '' } as Note);
    expect(tagsBtn()?.textContent).toContain('#1');
    openTags();
    expect(rows()).toEqual(['#home']);
    expect(tagInput()).toBeNull();
    expect(pop()!.querySelector('.tags-hint')).toBeNull();
    expect(pop()!.textContent).not.toContain('Apple Notes');
    expect(pop()!.querySelector('details')).toBeNull();
  });

  it('the highlight starts over when the list changes for any reason', async () => {
    await mountEditor(TAGGED);
    openTags();
    const input = tagInput()!;
    const selected = () =>
      Array.from(pop()!.querySelectorAll('[role="option"]'))
        .filter((o) => o.getAttribute('aria-selected') === 'true')
        .map((o) => o.textContent?.trim());
    key(input, 'ArrowDown');
    key(input, 'ArrowDown');
    expect(selected()).toEqual(['#work']);
    // The account's tags change underneath (a sync, another note's edit).
    setAccountNoteTags(GMAIL.id, [{ uuid: OTHER.uuid, tag: 'alpha' }, { uuid: OTHER.uuid, tag: 'errand' }]);
    flushSync();
    await tick();
    flushSync();
    expect(selected()).toEqual([]);
    // Enter now commits what was typed — nothing — rather than a stale row.
    key(input, 'Enter');
    expect(calls).toEqual([]);
  });

  it('aria-controls names the list only while it exists', async () => {
    await mountEditor(TAGGED);
    openTags();
    const input = tagInput()!;
    const list = () => pop()!.querySelector('[role="listbox"]');
    expect(list()).not.toBeNull();
    expect(document.getElementById(input.getAttribute('aria-controls')!)).toBe(list());
    type(input, 'zzz'); // matches nothing: no list
    expect(list()).toBeNull();
    expect(input.hasAttribute('aria-controls')).toBe(false);
  });

  it('on an Android tablet the field does not take focus on open (soft keyboard)', async () => {
    isAndroid.set(true);
    layout.set('tablet');
    await mountEditor(TAGGED);
    openTags();
    expect(document.querySelector('[data-presentation="sheet"]')).toBeNull();
    expect(tagInput()).not.toBeNull();
    expect(document.activeElement).not.toBe(tagInput());
  });
});

// ─── phone: a sheet ────────────────────────────────────────────────────────
// The sheet is a modal <dialog>: the page behind it — the editor — is inert,
// so an edit command run while it is open is refused (measured in Chrome:
// focus() is ignored and execCommand('insertText') returns false). The
// sheet's adds and removes run through the same functions once it closes.
describe('Tags popover on phone', () => {
  beforeEach(() => {
    isAndroid.set(true);
    layout.set('phone');
  });

  const sheet = () => document.querySelector('[data-presentation="sheet"]');

  it('is a sheet whose edits reach the body when it closes', async () => {
    await mountEditor(TAGGED);
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheet()!.querySelector('h2')?.textContent).toBe('Tags');
    const input = tagInput()!;
    type(input, 'work');
    key(input, 'Enter');
    pop()!.querySelector<HTMLButtonElement>('button[aria-label="Remove tag home"]')!.click();
    flushSync();
    // Shown at once, applied later.
    expect(rows()).toEqual(['#work']);
    expect(calls).toEqual([]);

    sheet()!.querySelector<HTMLButtonElement>('button.close')!.click();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(calls.length).toBe(2));
    expect(calls).toEqual([
      { cmd: 'insertText', arg: ' #work', selected: '' },
      { cmd: 'delete', arg: undefined, selected: '#' },
    ]);
    expect(body().textContent).toContain('home #work');
    expect(tagsBtn()!.textContent).toContain('#1');
  });

  it('an add then a remove of the same tag in the sheet is no edit', async () => {
    await mountEditor(TAGGED);
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    type(tagInput()!, 'work');
    key(tagInput()!, 'Enter');
    pop()!.querySelector<HTMLButtonElement>('button[aria-label="Remove tag work"]')!.click();
    flushSync();
    expect(rows()).toEqual(['#home']);
    sheet()!.querySelector<HTMLButtonElement>('button.close')!.click();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await settle();
    expect(calls).toEqual([]);
  });

  async function queueTwo(note: Note = TAGGED) {
    await mountEditor(note);
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    type(tagInput()!, 'work');
    key(tagInput()!, 'Enter');
    pop()!.querySelector<HTMLButtonElement>('button[aria-label="Remove tag home"]')!.click();
    flushSync();
    expect(rows()).toEqual(['#work']);
    expect(calls).toEqual([]);
  }
  const QUEUED = [
    { cmd: 'insertText', arg: ' #work', selected: '' },
    { cmd: 'delete', arg: undefined, selected: '#' },
  ];

  it('Android Back applies the queued edits, in order', async () => {
    await queueTwo();
    history.back();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(calls.length).toBe(2));
    expect(calls).toEqual(QUEUED);
    expect(body().textContent).toContain('home #work');
  });

  it('a scrim click applies the queued edits, in order', async () => {
    await queueTwo();
    // The click lands on the <dialog> itself, outside its panel.
    sheet()!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(calls.length).toBe(2));
    expect(calls).toEqual(QUEUED);
  });

  it('a note switch while edits are pending applies nothing, to either note', async () => {
    // Its own uuid: an earlier test's autosave timer outlives its editor
    // (NoteEditor does not cancel it on unmount) and may land on TAGGED.
    const note = { ...TAGGED, uuid: 'uuid-switch', id: 'msg-switch' } as Note;
    await queueTwo(note);
    selectedNote.set(OTHER);
    flushSync();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await settle();
    expect(calls).toEqual([]);
    expect(body().textContent).toContain('see #errand');
    selectedNote.set(note);
    await settle();
    expect(pop()).toBeNull();
    expect(calls).toEqual([]);
    expect(body().textContent).toContain('#home');
    expect(body().textContent).not.toContain('#work');
    expect(get(notes).find((n) => n.uuid === note.uuid)?.body_html).toBe(note.body_html);
    expect(get(notes).find((n) => n.uuid === OTHER.uuid)?.body_html).toBe(OTHER.body_html);
  });

  it.each([
    ['the note is locked by its remote document', () => selectedNote.set({ ...TAGGED, push_blocked_reason: 'permanent: changed shape', push_blocked_by_remote: true } as Note)],
    ['the account turns out not to take note writes', () => setAccountCapabilities(GMAIL.id, { writes: { notes: false, relocate: false, folders: false, sidecars: false } } as never)],
  ])('tags turning read-only while edits are pending applies nothing (%s)', async (_why, flip) => {
    await queueTwo();
    flip();
    flushSync();
    await tick();
    flushSync();
    // The sheet stays (same note), back to what the body holds, read-only.
    expect(sheet()).not.toBeNull();
    expect(rows()).toEqual(['#home']);
    expect(tagInput()).toBeNull();
    sheet()!.querySelector<HTMLButtonElement>('button.close')!.click();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await settle();
    expect(calls).toEqual([]);
  });

  it('rotating out of the sheet applies the pending edits before any new one', async () => {
    await mountEditor(TAGGED);
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    type(tagInput()!, 'work');
    key(tagInput()!, 'Enter');
    expect(calls).toEqual([]);
    layout.set('tablet');
    flushSync();
    await vi.waitFor(() => expect(calls.length).toBe(1));
    expect(sheet()).toBeNull();
    expect(pop()).not.toBeNull(); // the anchored popover now
    type(tagInput()!, 'errand');
    key(tagInput()!, 'Enter');
    expect(calls.map((c) => c.arg)).toEqual([' #work', ' #errand']);
    expect(rows()).toEqual(['#home', '#work', '#errand']);
  });

  // Live pass 2026-10-21: the sheet closes when the device rotates — App
  // mounts a separate NoteEditor per layout, so the sheet goes with the
  // outgoing editor. R26's queued edits must still land, in order, on the
  // editor that is showing the same note after the swap.
  it('a rotation that swaps the editor applies the queued edits to the incoming one', async () => {
    accounts.set([GMAIL as never]);
    currentAccount.set(GMAIL.id);
    notes.set([TAGGED, OTHER]);
    selectedNote.set(TAGGED);
    app = mount(LayoutSwapEditor, { target });
    await settle();
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    type(tagInput()!, 'work');
    key(tagInput()!, 'Enter');
    pop()!.querySelector<HTMLButtonElement>('button[aria-label="Remove tag home"]')!.click();
    flushSync();
    expect(calls).toEqual([]);

    layout.set('tablet');
    flushSync();
    await vi.waitFor(() => expect(target.querySelector('.tablet-layout .editor-body')).not.toBeNull());
    await vi.waitFor(() => expect(calls.length).toBe(2));
    expect(calls).toEqual(QUEUED);
    expect(sheet()).toBeNull();
    // On the live editor: its body, and the #N its metadata line counts.
    expect(body().textContent).toContain('home #work');
    expect(tagsBtn()!.textContent).toContain('#1');
    expect(get(selectedNote)!.body_html).toContain('home #work');
  });

  it('queued edits for a note the incoming editor is not showing are dropped', async () => {
    accounts.set([GMAIL as never]);
    currentAccount.set(GMAIL.id);
    notes.set([TAGGED, OTHER]);
    selectedNote.set(TAGGED);
    app = mount(LayoutSwapEditor, { target });
    await settle();
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    type(tagInput()!, 'work');
    key(tagInput()!, 'Enter');
    flushSync();
    // The note changes and the layout flips in the same turn.
    selectedNote.set(OTHER);
    layout.set('tablet');
    flushSync();
    await vi.waitFor(() => expect(target.querySelector('.tablet-layout .editor-body')).not.toBeNull());
    // The incoming editor's body renders a tick later: wait for it, not a
    // fixed delay (a full parallel run can be slower than any sleep).
    await vi.waitFor(() => expect(body().textContent).toContain('see #errand'));
    await settle();
    expect(calls).toEqual([]);
    expect(body().textContent).not.toContain('#work');
  });

  // Live pass 2026-10-21: the list hung below the field, past the sheet's
  // bottom edge — under the keyboard while typing, under the navigation bar
  // otherwise. The sheet's own padding clears both (Modal: --modal-kb,
  // --modal-safe-bottom), so the list sits in it, above the field.
  it('the suggestions open above the field, inside the sheet', async () => {
    await mountEditor(TAGGED);
    openTags();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    const input = tagInput()!;
    input.focus();
    input.dispatchEvent(new FocusEvent('focus'));
    type(input, 'er');
    const list = pop()!.querySelector<HTMLElement>('[role="listbox"]');
    expect(list).not.toBeNull();
    // Before the field in the document, so it lays out above it.
    expect(list!.compareDocumentPosition(input) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(list!.classList.contains('in-sheet')).toBe(true);
    // A pick still works from there.
    list!.querySelector('[role="option"]')!.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true }));
    flushSync();
    expect(rows()).toEqual(['#home', '#errand']);
  });

  // On a phone the toolbar row (and its ⋯) does not render (spec §5.6): ⋯ is
  // the note PaneHeader's (Task 6.2), the same NoteActionsButton App mounts.
  it('opened from ⋯ › Tags…, closing the sheet returns focus to #N', async () => {
    await mountEditor(TAGGED);
    expect(target.querySelector('.editor-toolbar')).toBeNull();
    headerButton = mount(NoteActionsButton, { target });
    flushSync();
    target.querySelector<HTMLButtonElement>('button.note-actions-btn[aria-label="Note actions"]')!.click();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    const item = Array.from(sheet()!.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')).find(
      (b) => b.textContent?.trim() === 'Tags…',
    )!;
    item.click();
    await vi.waitFor(() => expect(pop()).not.toBeNull());
    await vi.waitFor(() => expect(sheet()?.querySelector('h2')?.textContent).toBe('Tags'));
    sheet()!.querySelector<HTMLButtonElement>('button.close')!.click();
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(document.activeElement).toBe(tagsBtn()));
  });
});

// ─── the component alone ───────────────────────────────────────────────────
describe('TagsPopover', () => {
  let anchor: HTMLButtonElement;
  let instance: ReturnType<typeof mount> | null = null;
  beforeEach(() => {
    anchor = document.createElement('button');
    anchor.setAttribute('aria-label', 'Tags (2)');
    document.body.appendChild(anchor);
  });
  afterEach(() => {
    if (instance) unmount(instance);
    instance = null;
    anchor.remove();
  });

  function render(props: Partial<Record<string, unknown>> = {}) {
    const p = {
      anchor,
      open: true,
      onclose: vi.fn(),
      tags: ['home', 'work'],
      editable: true,
      suggestions: () => [] as string[],
      onAdd: vi.fn(),
      onRemove: vi.fn(),
      ...props,
    };
    instance = mount(TagsPopover, { target: document.body, props: p });
    flushSync();
    return p;
  }

  it('normalizes what is typed before handing it on', () => {
    const p = render();
    type(tagInput()!, '  #Deep Work ');
    key(tagInput()!, 'Enter');
    expect(p.onAdd).toHaveBeenCalledWith('deepwork');
  });

  it('a composing Enter (IME) adds nothing, and arrows leave the candidates alone', () => {
    const p = render({ suggestions: () => ['errand'] });
    tagInput()!.focus();
    flushSync();
    type(tagInput()!, 'ทด');
    key(tagInput()!, 'Enter', { isComposing: true });
    key(tagInput()!, 'Enter', { keyCode: 229 });
    expect(p.onAdd).not.toHaveBeenCalled();
    expect(tagInput()!.value).toBe('ทด');
    const down = new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true, isComposing: true });
    tagInput()!.dispatchEvent(down);
    expect(down.defaultPrevented).toBe(false);
    // Once composition has ended, Enter commits.
    key(tagInput()!, 'Enter');
    expect(p.onAdd).toHaveBeenCalledWith('ทด');
  });

  it('a tag already on the note is not added twice', () => {
    const p = render();
    type(tagInput()!, 'home');
    key(tagInput()!, 'Enter');
    expect(p.onAdd).not.toHaveBeenCalled();
  });

  it('read-only without a reason shows no Learn more', () => {
    render({ editable: false });
    expect(tagInput()).toBeNull();
    expect(pop()!.textContent).toContain('Add or remove tags in Apple Notes.');
    expect(pop()!.querySelector('details')).toBeNull();
  });
});
