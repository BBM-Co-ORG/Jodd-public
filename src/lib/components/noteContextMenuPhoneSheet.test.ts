// @vitest-environment jsdom
//
// Task 2.7: on an Android phone the note menu is a bottom sheet (a Modal with
// phone="sheet") holding the SAME items in the SAME order, with touch-sized
// rows. Desktop and tablet keep the anchored menu. Gate order is global:
// $isAndroid first, then $androidLayoutMode.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync } from 'svelte';
import { readFileSync } from 'node:fs';
import { get } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false), supportsIcloud: writable(true), isMacos: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteContextMenu from './NoteContextMenu.svelte';
import type { Writable } from 'svelte/store';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { handlePopState, initPhoneNavHistory, overlayFromHistoryState, paneFromHistoryState } from '../stores/phoneNav';
import { accounts, currentAccount, selectedNote, setAccountCapabilities } from '../stores/notes';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const layout = layoutReadonly as unknown as Writable<string>;

const ACCOUNT_ID = 'gmail:a@b.com';
const NOTE: Note = {
  uuid: 'uuid-1', id: 'msg-1', account_id: ACCOUNT_ID, title: 'Groceries',
  body_html: '<p>milk</p>', date: '2026-10-20T00:00:00Z', label: 'Notes',
} as Note;
const OTHER: Note = { ...NOTE, uuid: 'uuid-2', id: 'msg-2', title: 'Other' };

const commandCalls = () => invoke.mock.calls.filter((c) => c[0] !== 'platform_name');

let target: HTMLElement;
let mounted: ReturnType<typeof mount> | undefined;
let onClose: ReturnType<typeof vi.fn<() => void>>;

function open(props: { note?: Note; selection?: Note[] } = {}) {
  onClose = vi.fn<() => void>();
  mounted = mount(NoteContextMenu, {
    target,
    props: {
      x: 10, y: 10, note: props.note ?? NOTE, selection: props.selection ?? [],
      onClose, onLinkSuggestions: () => {},
    },
  });
  flushSync();
}

const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
const buttonByLabel = (root: ParentNode, label: string) =>
  Array.from(root.querySelectorAll<HTMLButtonElement>('button')).find(
    (b) => b.querySelector('.label')?.textContent?.trim() === label,
  );
const click = (b: HTMLElement | undefined | null) => { expect(b).toBeTruthy(); b!.click(); flushSync(); };

beforeEach(async () => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) =>
    Promise.resolve(cmd === 'list_folders' ? ['Notes', 'Notes/Work'] : []),
  );
  accounts.set([{ id: ACCOUNT_ID, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set(ACCOUNT_ID);
  setAccountCapabilities(ACCOUNT_ID, { has_trash: true });
  selectedNote.set(null);
  isAndroid.set(false);
  layout.set('phone');
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(async () => {
  if (mounted) unmount(mounted);
  mounted = undefined;
  target.remove();
  // Unmounting a sheet pops its history entry asynchronously: let that land
  // before the next test starts, or its popstate leaks into it.
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

/** Item labels of the anchored menu, top level only (the hover flyouts are not items of it). */
function desktopLabels(): string[] {
  return Array.from(document.querySelectorAll('.context-menu .item .label'))
    .filter((l) => !l.closest('.submenu'))
    .map((l) => l.textContent!.trim());
}
function sheetLabels(): string[] {
  return Array.from(sheet()!.querySelectorAll('.sheet-menu .item .label')).map((l) => l.textContent!.trim());
}

describe('note context menu on phone', () => {
  it('phone long-press opens a sheet with the same item labels as desktop', async () => {
    open();
    const desktop = desktopLabels();
    expect(desktop.length).toBeGreaterThan(3);
    expect(sheet()).toBeNull();
    unmount(mounted!);
    mounted = undefined;

    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(document.querySelector('.context-menu')).toBeNull();
    // Same items; only Delete moves, to the end (ruling R17).
    expect([...sheetLabels()].sort()).toEqual([...desktop].sort());
    expect(sheetLabels()).toEqual([...desktop.filter((l) => l !== 'Delete'), 'Delete']);
    expect(sheet()!.querySelector('h2')!.textContent).toBe('Groceries');
  });

  it('desktop still renders the anchored menu', () => {
    open();
    expect(document.querySelector('.context-menu')).not.toBeNull();
    expect(document.querySelector('dialog')).toBeNull();
  });

  it('tablet (Android, wide) still renders the anchored menu', () => {
    isAndroid.set(true);
    layout.set('tablet');
    open();
    expect(document.querySelector('.context-menu')).not.toBeNull();
    expect(document.querySelector('dialog')).toBeNull();
  });

  it('a narrow desktop window never gets the sheet', () => {
    layout.set('phone'); // isAndroid stays false
    open();
    expect(document.querySelector('.context-menu')).not.toBeNull();
    expect(document.querySelector('dialog')).toBeNull();
  });

  it('titles a multi-selection "N notes" and keeps the batch items', async () => {
    isAndroid.set(true);
    open({ selection: [NOTE, OTHER] });
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheet()!.querySelector('h2')!.textContent).toBe('2 notes');
    expect(sheetLabels()).toContain('Delete 2 notes');
  });

  it('on phone Delete is the last row, in danger colour, after Move to', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    const rows = Array.from(sheet()!.querySelectorAll('.sheet-menu > .item'));
    expect(rows[rows.length - 1].querySelector('.label')!.textContent).toBe('Delete');
    expect(rows[rows.length - 1].classList.contains('danger')).toBe(true);
    expect(sheetLabels().indexOf('Move to')).toBeLessThan(sheetLabels().indexOf('Delete'));
  });

  it('on phone the batch Delete is also last', async () => {
    isAndroid.set(true);
    open({ selection: [NOTE, OTHER] });
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(sheetLabels().at(-1)).toBe('Delete 2 notes');
  });

  it('on desktop the anchored order is unchanged: Delete before Move to, Move to last', () => {
    open();
    const labels = desktopLabels();
    expect(labels.indexOf('Delete')).toBeLessThan(labels.indexOf('Move to'));
    expect(labels.at(-1)).toBe('Move to');
  });

  it('the layout leaving phone while the sheet is open closes the menu', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    layout.set('tablet');
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(sheet()).toBeNull();
  });

  it('a disabled account row says why, in text, and does not react to hover', async () => {
    isAndroid.set(true);
    accounts.set([
      { id: ACCOUNT_ID, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never,
      { id: 'gmail:c@d.com', email: 'c@d.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never,
    ]);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Move to'));
    await vi.waitFor(() => expect(sheet()!.querySelector('.row-note')).not.toBeNull());
    expect(sheet()!.querySelector('.row-note')!.textContent).toBe('Cross-account move not supported yet');
    expect(sheet()!.querySelectorAll('.row-note').length).toBe(1); // only the disabled account
    // The Move to level's markup and row styles moved to MoveToTargets.svelte
    // (shared with the editor's ⋯ menu, Task 5.2).
    const src = readFileSync('src/lib/components/MoveToTargets.svelte', 'utf8');
    expect(src).toMatch(/\.sheet-menu\) \.item\.account-row:hover[^{]*\{[^}]*background: transparent/);
  });

  // The component's scoped reset above loses to the shared stylesheet's
  // hover rule on specificity (Task 4.3 review), so the shared rule itself
  // must not select heading rows. jsdom applies no :hover, so the rule's
  // selector — minus :hover — is matched against the rendered rows.
  it('the shared sheet hover rule never selects an account heading row', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Move to'));
    await vi.waitFor(() => expect(sheet()!.querySelector('.item.account-row')).not.toBeNull());
    const css = readFileSync('src/styles/sheet-menu.css', 'utf8');
    const hoverRules = [...css.matchAll(/([^{}]*\.item[^{}]*:hover[^{}]*)\{[^}]*background:\s*var\(--surface-hover\)/g)]
      .map((m) => m[1].replace(/\/\*[\s\S]*?\*\//g, '').trim());
    expect(hoverRules.length).toBe(1);
    const selector = hoverRules[0].replace(':hover', '');
    const heading = sheet()!.querySelector('.item.account-row')!;
    expect(heading.matches(selector)).toBe(false);
    // …while an ordinary row in the same sheet still highlights.
    const folderRow = sheet()!.querySelector('button.item:not(.account-row):not(:disabled)')!;
    expect(folderRow).not.toBeNull();
    expect(folderRow.matches(selector)).toBe(true);
  });

  it('Move to is a second level inside the same sheet, with a Back row', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Move to'));

    await vi.waitFor(() => expect(buttonByLabel(sheet()!, 'Back')).toBeTruthy());
    expect(document.querySelectorAll('dialog').length).toBe(1);
    await vi.waitFor(() => expect(sheetLabels()).toContain('Work'));
    expect(buttonByLabel(sheet()!, 'Pin')).toBeUndefined();

    click(buttonByLabel(sheet()!, 'Back'));
    expect(buttonByLabel(sheet()!, 'Pin')).toBeTruthy();
    expect(buttonByLabel(sheet()!, 'Back')).toBeUndefined();
    expect(onClose).not.toHaveBeenCalled();
  });

  // Final review 2026-10-20: the sheet's folders were fetched only when the
  // menu MOUNTED as a sheet; an empty Map then read as "fetched" to
  // MoveToTargets, so a tablet → phone flip listed only Notes.
  it('a tablet → phone flip while the menu is open still lists the folders under Move to', async () => {
    isAndroid.set(true);
    layout.set('tablet');
    open();
    expect(sheet()).toBeNull();
    invoke.mockClear();
    layout.set('phone');
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'list_folders')).toBe(true);
    click(buttonByLabel(sheet()!, 'Move to'));
    await vi.waitFor(() => expect(sheetLabels()).toContain('Work'));
  });

  it('picking a folder moves the note and closes the sheet', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Move to'));
    await vi.waitFor(() => expect(buttonByLabel(sheet()!, 'Work')).toBeTruthy());
    click(buttonByLabel(sheet()!, 'Work'));

    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(commandCalls().some((c) => /move/i.test(String(c[0])))).toBe(true);
  });

  it('picking an item runs it, then closes through the Modal exactly once', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Pin'));

    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(commandCalls().some((c) => c[0] === 'set_pin')).toBe(true);
  });

  it('New Note closes the sheet INTO the note pane before selecting', async () => {
    isAndroid.set(true);
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    open();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBeNull());
    const depth = history.state.depth;

    click(buttonByLabel(sheet()!, 'New Note'));

    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(overlayFromHistoryState(history.state)).toBeNull();
    expect(paneFromHistoryState(history.state)).toBe('note');
    expect(history.state.depth).toBe(depth); // the sheet's entry was replaced, not stacked on
    expect(get(selectedNote)?.title).toBe('New Note');
  });

  it('Duplicate closes the sheet before the copy is selected (selecting opens the note pane)', async () => {
    isAndroid.set(true);
    let finishSave: (v: unknown) => void = () => {};
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'save_note') return new Promise((r) => { finishSave = r; });
      return Promise.resolve(cmd === 'list_folders' ? ['Notes'] : []);
    });
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Duplicate'));

    // The save is still pending; the sheet is already closed.
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(commandCalls().some((c) => c[0] === 'save_note')).toBe(true);
    finishSave({ id: 'msg-dup', uuid: 'uuid-dup' });
    await vi.waitFor(() => expect(get(selectedNote)?.uuid).toBe('uuid-dup'));
  });

  it('closes with Esc through the Modal and calls onClose once', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    sheet()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it('an outside pointerdown does not tear the sheet down before a tap lands', async () => {
    isAndroid.set(true);
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    // The sheet lives in a <dialog>, not in .context-menu: the desktop
    // outside-click handler must not treat a tap on a row as "outside".
    buttonByLabel(sheet()!, 'Pin')!.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true }));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('rows are touch-sized, from tokens only', () => {
    // The rows are the shared sheet stylesheet's (src/styles/sheet-menu.css);
    // the menu keeps only its own row kinds, also tokens only.
    const css = readFileSync('src/styles/sheet-menu.css', 'utf8');
    expect(css).toMatch(/\.sheet-menu \.item \{[^}]*min-height: var\(--touch-target\)/);
    const src = readFileSync('src/lib/components/NoteContextMenu.svelte', 'utf8');
    const block = src.slice(src.indexOf('Phone bottom sheet'));
    expect(block).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
    // …and so does the Move to level's own (a new file: tokens only).
    const moveCss = readFileSync('src/lib/components/MoveToTargets.svelte', 'utf8');
    expect(moveCss.slice(moveCss.indexOf('<style>'))).not.toMatch(/(padding|margin|gap)[\w-]*:[^;]*\d+px/);
  });
});

describe('note context menu on phone — permanent delete confirm', () => {
  beforeEach(() => {
    setAccountCapabilities(ACCOUNT_ID, { has_trash: false });
    isAndroid.set(true);
  });

  it('keeps the sheet open while the confirm is asked, and closes it when the user cancels', async () => {
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Delete'));

    await vi.waitFor(() => expect(document.body.textContent).toMatch(/cannot undo this/i));
    expect(onClose).not.toHaveBeenCalled();
    expect(commandCalls().some((c) => c[0] === 'delete_note')).toBe(false);

    click(Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Cancel'));
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(commandCalls().some((c) => c[0] === 'delete_note')).toBe(false);
  });

  it('closes the sheet right after the user confirms, and deletes', async () => {
    open();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    click(buttonByLabel(sheet()!, 'Delete'));
    await vi.waitFor(() => expect(document.body.textContent).toMatch(/cannot undo this/i));
    expect(onClose).not.toHaveBeenCalled();

    click(Array.from(document.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Delete permanently'));
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(commandCalls().some((c) => c[0] === 'delete_note')).toBe(true));
  });
});
