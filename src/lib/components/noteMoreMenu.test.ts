// @vitest-environment jsdom
//
// Task 5.2 (spec 2026-10-20 §6.1, §6.3, §5.6): the editor's ⋯ menu. It holds
// the note actions the toolbar used to carry as icon buttons (Graph, the
// shortcuts cheat sheet, Delete) beside the note-list context menu's, and it
// calls the SAME action functions as that menu (controllers/noteActions.ts) —
// the anti-goal is a second implementation that drifts. Those functions are
// spied here so a test fails if the menu stops calling them.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(false), isMacos: writable(false), supportsIcloud: writable(true) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/viewport')>();
  return { ...orig, androidLayoutMode: writable('tablet') };
});
vi.mock('../controllers/noteActions', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../controllers/noteActions')>();
  return {
    ...orig,
    togglePin: vi.fn(async () => {}),
    moveNoteTo: vi.fn(async () => {}),
    duplicateNote: vi.fn(async () => {}),
    refetchNote: vi.fn(async () => {}),
    deleteNoteWithConfirm: vi.fn(async () => {}),
  };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteMoreMenu from './NoteMoreMenu.svelte';
import NoteMoreMenuHost from './__fixtures__/NoteMoreMenuHost.svelte';
import NoteEditor from './NoteEditor.svelte';
import NoteActionsButton from './NoteActionsButton.svelte';
import * as actions from '../controllers/noteActions';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import { accounts, currentAccount, notes, selectedNote, setAccountCapabilities, capabilitiesByAccount, error, unreviewedTrust, unreviewedKey, type Trust } from '../stores/notes';
import { persistenceByNote } from '../notePersistence';
import { noteKey } from '../noteIdentity';
import { toast } from '../stores/toast';
import { handlePopState, overlayFromHistoryState } from '../stores/phoneNav';
import type { Note } from '../types';
import { stubWebviewClipboard, type ClipboardStub } from './__fixtures__/clipboardStub';
import { askModalOpen, pendingAskScope } from '../stores/ui';
import { hiddenFromAi, AI_HIDDEN_HINT } from '../stores/aiScope';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const layout = layoutReadonly as unknown as Writable<string>;

const ACCOUNT_ID = 'gmail:a@b.com';
const NOTE: Note = {
  uuid: 'abcdef12-3456-7890-abcd-ef1234567890', id: 'msg-1', account_id: ACCOUNT_ID, title: 'Groceries list',
  body_html: '<p>milk</p>', date: '2026-10-20T00:00:00Z', label: 'Notes',
} as Note;
const SLUG = 'groceries-list-abcdef12';

let target: HTMLElement;
let anchor: HTMLButtonElement;
let mounted: ReturnType<typeof mount> | undefined;

type Handler = ReturnType<typeof vi.fn<() => void>>;
type Handlers = {
  onclose: Handler;
  onFind: Handler;
  onShowGraph: Handler;
  onShortcuts: Handler;
  onMarkReviewed: Handler;
  onTags: Handler;
};
let h: Handlers;

function open(props: { note?: Note; trustPending?: boolean; reviewBlocked?: boolean; showTags?: boolean } = {}) {
  h = {
    onclose: vi.fn<() => void>(), onFind: vi.fn<() => void>(), onShowGraph: vi.fn<() => void>(),
    onShortcuts: vi.fn<() => void>(), onMarkReviewed: vi.fn<() => void>(), onTags: vi.fn<() => void>(),
  };
  mounted = mount(NoteMoreMenu, {
    target,
    props: {
      note: props.note ?? NOTE,
      anchor,
      open: true,
      trustPending: props.trustPending ?? false,
      reviewBlocked: props.reviewBlocked ?? false,
      showTags: props.showTags ?? true,
      ...h,
    },
  });
  flushSync();
}

const menu = () => document.querySelector<HTMLElement>('[data-testid="note-more-menu"]');
const labels = (root: ParentNode | null = menu()) =>
  Array.from(root!.querySelectorAll('.item .label')).map((l) => l.textContent!.trim());
const item = (label: string, root: ParentNode | null = menu()) =>
  Array.from(root!.querySelectorAll<HTMLButtonElement>('button.item')).find(
    (b) => b.querySelector('.label')?.textContent?.trim() === label,
  );
const click = (b: HTMLElement | undefined | null) => { expect(b).toBeTruthy(); b!.click(); flushSync(); };

beforeEach(() => {
  vi.clearAllMocks();
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'list_folders') return Promise.resolve(['Notes', 'Notes/Work']);
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
  accounts.set([{ id: ACCOUNT_ID, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set(ACCOUNT_ID);
  capabilitiesByAccount.set({});
  setAccountCapabilities(ACCOUNT_ID, { has_trash: true });
  isAndroid.set(false);
  layout.set('tablet');
  toast.set(null);
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.appendChild(target);
  anchor = document.createElement('button');
  anchor.setAttribute('aria-label', 'Note actions');
  document.body.appendChild(anchor);
});

afterEach(async () => {
  if (mounted) unmount(mounted);
  mounted = undefined;
  target.remove();
  anchor.remove();
  // A phone sheet pops its history entry asynchronously on unmount (R8).
  await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  window.removeEventListener('popstate', handlePopState);
});

describe('NoteMoreMenu — items', () => {
  // Spec 2026-10-23 (owner, 2026-10-24): Ask Jodd on this note from the
  // editor's ⋯ menu too, not only the note list's context menu.
  it('Ask Jodd about this note closes the menu, then opens Ask Jodd on this note', async () => {
    pendingAskScope.set(null);
    askModalOpen.set(false);
    open();
    click(item('Ask Jodd about this note'));
    expect(h.onclose).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(get(askModalOpen)).toBe(true));
    expect(get(pendingAskScope)).toEqual({
      kind: 'note', account_id: ACCOUNT_ID, uuid: NOTE.uuid, title: 'Groceries list', links: false, tags: false, depth: 1,
    });
  });

  it('Ask Jodd about this note is disabled, saying why, in a folder hidden from AI', () => {
    hiddenFromAi.set({ [ACCOUNT_ID]: ['Notes'] });
    open();
    const b = item('Ask Jodd about this note')!;
    expect(b.disabled).toBe(true);
    expect(b.title).toBe(AI_HIDDEN_HINT);
    hiddenFromAi.set({});
  });

  it('Ask Jodd about this note is absent for a note not saved yet', () => {
    open({ note: { ...NOTE, uuid: 'tmp:1' } });
    expect(labels()).not.toContain('Ask Jodd about this note');
  });

  it('menu items in order', () => {
    open({ trustPending: true });
    expect(labels()).toEqual([
      'Pin', 'Move to…', 'Tags…', 'Find in note', 'Copy wiki link', 'Show graph', 'Ask Jodd about this note',
      'Keyboard shortcuts', 'Duplicate', 'Refetch from Gmail', 'Mark reviewed', 'Delete',
    ]);
    // The divider sits directly above Delete, which is in danger colour.
    const del = item('Delete')!;
    expect(del.classList.contains('danger')).toBe(true);
    expect(del.previousElementSibling?.classList.contains('sep')).toBe(true);
  });

  it('a pinned note offers Unpin', () => {
    open({ note: { ...NOTE, pinned: true } });
    expect(labels()[0]).toBe('Unpin');
  });

  it('Keyboard shortcuts is desktop only', () => {
    isAndroid.set(true);
    layout.set('tablet');
    open();
    expect(labels()).not.toContain('Keyboard shortcuts');
  });

  it('Mark reviewed shows only while the trust status is pending', () => {
    open({ trustPending: false });
    expect(labels()).not.toContain('Mark reviewed');
  });

  it('Mark reviewed calls the status popover\'s handler, and is disabled while review is blocked', async () => {
    open({ trustPending: true, reviewBlocked: true });
    const b = item('Mark reviewed')!;
    expect(b.disabled).toBe(true);
    expect(b.title).toBe('Load the latest version of this note first');
    unmount(mounted!);
    mounted = undefined;

    open({ trustPending: true });
    click(item('Mark reviewed'));
    expect(h.onclose).toHaveBeenCalledTimes(1);
    await vi.waitFor(() => expect(h.onMarkReviewed).toHaveBeenCalledTimes(1));
  });

  it('Tags… is hidden when the #N button would be', () => {
    open({ showTags: false });
    expect(labels()).not.toContain('Tags…');
  });

  // The context menu's gates (NoteContextMenu.svelte): Pin is sidecars;
  // Duplicate is notes; Move to and Delete are relocate.
  it('a read-only account hides every write the context menu hides', () => {
    setAccountCapabilities(ACCOUNT_ID, {
      has_trash: true, writes: { notes: false, relocate: false, folders: false, sidecars: false },
    } as never);
    open();
    expect(labels()).toEqual([
      'Tags…', 'Find in note', 'Copy wiki link', 'Show graph', 'Ask Jodd about this note', 'Keyboard shortcuts', 'Refetch from Gmail',
    ]);
    expect(menu()!.querySelector('.sep')).toBeNull();
  });

  it('no Pin where sidecars cannot be written (Microsoft)', () => {
    setAccountCapabilities(ACCOUNT_ID, {
      has_trash: false, writes: { notes: true, relocate: true, folders: false, sidecars: false },
    } as never);
    open();
    expect(labels()).not.toContain('Pin');
    expect(labels()).toContain('Duplicate');
    expect(labels()).toContain('Delete');
  });

  it('Duplicate and Refetch are disabled for a note with no remote id, as in the context menu', () => {
    open({ note: { ...NOTE, id: '' } });
    expect(item('Duplicate')!.disabled).toBe(true);
    expect(item('Refetch from Gmail')!.disabled).toBe(true);
  });
});

describe('NoteMoreMenu — every action is the shared one', () => {
  it('Pin calls togglePin', () => {
    open();
    click(item('Pin'));
    expect(actions.togglePin).toHaveBeenCalledWith(NOTE);
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });

  it('Duplicate calls duplicateNote', () => {
    open();
    click(item('Duplicate'));
    expect(actions.duplicateNote).toHaveBeenCalledWith(NOTE);
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });

  it('Refetch calls refetchNote', () => {
    open();
    click(item('Refetch from Gmail'));
    expect(actions.refetchNote).toHaveBeenCalledWith(NOTE);
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });

  it('Delete calls deleteNoteWithConfirm, and the menu closes when the action says so', () => {
    open();
    click(item('Delete'));
    expect(actions.deleteNoteWithConfirm).toHaveBeenCalledTimes(1);
    const [note, confirm, onDecided] = vi.mocked(actions.deleteNoteWithConfirm).mock.calls[0];
    expect(note).toEqual(NOTE);
    expect(typeof confirm).toBe('function');
    // The confirm dialog renders inside this menu: it must not close before
    // the action decides (R6).
    expect(h.onclose).not.toHaveBeenCalled();
    onDecided();
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });

  // Each closes first, then runs once the close has rendered: on phone a
  // dialog opened in the same pass would push its history entry under the
  // sheet's pop (see paneHeader.test.ts, graph → folder).
  it.each([
    ['Find in note', 'onFind'],
    ['Show graph', 'onShowGraph'],
    ['Keyboard shortcuts', 'onShortcuts'],
    ['Tags…', 'onTags'],
  ] as const)('%s closes the menu, then calls its handler', async (label, handler) => {
    open();
    click(item(label));
    expect(h.onclose).toHaveBeenCalledTimes(1);
    expect(h[handler]).not.toHaveBeenCalled();
    await vi.waitFor(() => expect(h[handler]).toHaveBeenCalledTimes(1));
  });

  it('Move to… opens a submenu of the context menu\'s targets; a pick calls moveNoteTo', async () => {
    open();
    click(item('Move to…'));
    const sub = () => document.querySelector<HTMLElement>('[data-testid="note-move-menu"]');
    await vi.waitFor(() => expect(item('Work', sub())).toBeTruthy());
    // The account heading names the backend, as in the context menu.
    expect(sub()!.querySelector('.account-row')!.textContent).toMatch(/Gmail/);
    // The current folder is the disabled, checked row.
    expect(item('Notes', sub())!.disabled).toBe(true);
    click(item('Work', sub()));
    expect(actions.moveNoteTo).toHaveBeenCalledWith(NOTE, 'Notes/Work');
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });

  it('the Move to folders are fetched once per opening, not on every level toggle', async () => {
    isAndroid.set(true);
    layout.set('phone');
    open();
    const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    for (let i = 0; i < 3; i++) {
      click(item('Move to…', sheet()));
      await vi.waitFor(() => expect(item('Work', sheet())).toBeTruthy());
      click(item('Back', sheet()));
    }
    expect(invoke.mock.calls.filter((c) => c[0] === 'list_folders').length).toBe(1);
  });

  it('on phone, Move to… is a second level in the same sheet, with Back', async () => {
    isAndroid.set(true);
    layout.set('phone');
    open();
    const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
    expect(labels(sheet()).at(-1)).toBe('Delete');
    click(item('Move to…', sheet()));
    await vi.waitFor(() => expect(item('Work', sheet())).toBeTruthy());
    expect(document.querySelectorAll('dialog').length).toBe(1);
    expect(item('Pin', sheet())).toBeUndefined();
    click(item('Back', sheet()));
    expect(item('Pin', sheet())).toBeTruthy();
    click(item('Move to…', sheet()));
    await vi.waitFor(() => expect(item('Work', sheet())).toBeTruthy());
    click(item('Work', sheet()));
    expect(actions.moveNoteTo).toHaveBeenCalledWith(NOTE, 'Notes/Work');
    expect(h.onclose).toHaveBeenCalledTimes(1);
  });
});

describe('NoteMoreMenu on phone — permanent delete confirm', () => {
  const sheet = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="sheet"]');
  const alertDialog = () => document.querySelector<HTMLDialogElement>('dialog[data-presentation="alert"]');
  let hostClose: ReturnType<typeof vi.fn<() => void>>;

  beforeEach(async () => {
    isAndroid.set(true);
    layout.set('phone');
    setAccountCapabilities(ACCOUNT_ID, { has_trash: false });
    // The real shared action, so its confirm reaches this menu's dialog.
    const real = await vi.importActual<typeof import('../controllers/noteActions')>('../controllers/noteActions');
    vi.mocked(actions.deleteNoteWithConfirm).mockImplementation(real.deleteNoteWithConfirm);
    notes.set([NOTE]);
    hostClose = vi.fn<() => void>();
    mounted = mount(NoteMoreMenuHost, { target, props: { note: NOTE, anchor, onclose: hostClose } });
    flushSync();
    await vi.waitFor(() => expect(sheet()).not.toBeNull());
  });
  // clearAllMocks keeps implementations: put the spy's no-op back, so a later
  // suite does not run the real delete.
  afterEach(() => {
    vi.mocked(actions.deleteNoteWithConfirm).mockImplementation(async () => {});
  });

  it('the sheet stays open under the alert; Back closes the alert, then the sheet, in that order', async () => {
    const sheetEntry = overlayFromHistoryState(history.state);
    expect(sheetEntry).not.toBeNull();
    click(item('Delete', sheet()));
    await vi.waitFor(() => expect(alertDialog()).not.toBeNull());
    expect(alertDialog()!.textContent).toMatch(/cannot undo this/i);
    // Both are open, the alert's history entry above the sheet's.
    expect(sheet()).not.toBeNull();
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).not.toBe(sheetEntry));
    expect(overlayFromHistoryState(history.state)).not.toBeNull();
    expect(hostClose).not.toHaveBeenCalled();

    // Android Back: the alert answers Cancel; the action then closes the
    // menu (R6), whose sheet pops its own entry — the second Back's step.
    history.back();
    await vi.waitFor(() => expect(alertDialog()).toBeNull());
    await vi.waitFor(() => expect(hostClose).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(sheet()).toBeNull());
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
    expect(invoke.mock.calls.some((c) => c[0] === 'delete_note')).toBe(false);
  });

  it('confirming deletes and closes the sheet', async () => {
    click(item('Delete', sheet()));
    await vi.waitFor(() => expect(alertDialog()).not.toBeNull());
    click(Array.from(alertDialog()!.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Delete permanently'));
    await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'delete_note')).toBe(true));
    await vi.waitFor(() => expect(sheet()).toBeNull());
    expect(hostClose).toHaveBeenCalledTimes(1);
  });
});

describe('NoteMoreMenu — Copy wiki link', () => {
  it('Copy wiki link copies [[slug]] and toasts', async () => {
    vi.useFakeTimers();
    try {
      const writeText = vi.fn(() => Promise.resolve());
      Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
      open();
      click(item('Copy wiki link'));
      expect(writeText).toHaveBeenCalledWith(`[[${SLUG}]]`);
      expect(h.onclose).toHaveBeenCalledTimes(1);
      // writeText's promise settles, then the toast shows. (vi.waitFor would
      // advance the fake clock itself, so the 1500 ms below is counted here.)
      await Promise.resolve();
      await Promise.resolve();
      expect(get(toast)?.text).toBe(`Copied [[${SLUG}]]`);
      vi.advanceTimersByTime(1499);
      expect(get(toast)).not.toBeNull();
      vi.advanceTimersByTime(1);
      expect(get(toast)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it('truncates a long slug in the toast, not in the clipboard', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } });
    const title = 'A very long note title that keeps going well past what a toast can show';
    open({ note: { ...NOTE, title } });
    click(item('Copy wiki link'));
    const copied = (writeText.mock.calls[0] as unknown as [string])[0];
    expect(copied.length).toBeGreaterThan(60);
    await vi.waitFor(() => expect(get(toast)).not.toBeNull());
    const text = get(toast)!.text;
    expect(text.startsWith('Copied [[')).toBe(true);
    expect(text.endsWith('…]]')).toBe(true);
    expect(text.length).toBeLessThan(copied.length);
  });

  describe('with the webview clipboard refusing', () => {
    let clip: ClipboardStub | undefined;
    beforeEach(() => error.set(null));
    afterEach(() => {
      clip?.restore();
      clip = undefined;
      error.set(null);
    });

    it('navigator.clipboard rejecting still copies [[slug]] and toasts', async () => {
      clip = stubWebviewClipboard('rejects');
      open();
      click(item('Copy wiki link'));
      expect(clip.writeText).toHaveBeenCalledWith(`[[${SLUG}]]`);
      await vi.waitFor(() => expect(get(toast)?.text).toBe(`Copied [[${SLUG}]]`));
      expect(clip.copied).toEqual([`[[${SLUG}]]`]);
      expect(get(error)).toBeNull();
    });

    it('both paths failing shows an error and no "Copied" toast', async () => {
      clip = stubWebviewClipboard('fails');
      open();
      click(item('Copy wiki link'));
      await vi.waitFor(() => expect(get(error)).toBe("Couldn't copy the wiki link."));
      expect(get(toast)).toBeNull();
    });
  });

  it('no Copy wiki link for tmp:', () => {
    open({ note: { ...NOTE, uuid: 'tmp:123', id: '' } });
    expect(labels()).not.toContain('Copy wiki link');
    expect(labels()).toContain('Find in note');
  });
});

describe('NoteEditor toolbar', () => {
  async function mountEditor(note: Note = NOTE) {
    notes.set([note]);
    selectedNote.set(note);
    mounted = mount(NoteEditor, { target });
    flushSync();
    await tick();
  }

  it('toolbar has no trash button', async () => {
    await mountEditor();
    const toolbar = target.querySelector('.editor-toolbar')!;
    expect(toolbar).not.toBeNull();
    expect(toolbar.querySelector('.delete-btn')).toBeNull();
    const titles = Array.from(toolbar.querySelectorAll('button')).map((b) => b.getAttribute('title'));
    expect(titles).not.toContain('Delete note');
    expect(titles).not.toContain('Connections graph');
    expect(titles).not.toContain('Editor shortcuts');
    // …they are in ⋯, which the toolbar carries last.
    const more = toolbar.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]')!;
    expect(more).not.toBeNull();
    const buttons = Array.from(toolbar.querySelectorAll('button'));
    expect(buttons.at(-1)).toBe(more);
    more.click();
    flushSync();
    expect(labels()).toContain('Delete');
    expect(labels()).toContain('Show graph');
    expect(labels()).toContain('Keyboard shortcuts');
  });

  it.each([
    ['Show graph', 'Note graph'],
    ['Keyboard shortcuts', 'Keyboard shortcuts'],
  ])('⋯ %s opens the dialog its toolbar button did', async (label, dialogTitle) => {
    await mountEditor();
    target.querySelector<HTMLButtonElement>('.editor-toolbar button[aria-label="Note actions"]')!.click();
    flushSync();
    click(item(label));
    await vi.waitFor(() => expect(document.querySelector('dialog[open] h2')?.textContent).toBe(dialogTitle));
    // The menu closed on the pick.
    expect(menu()).toBeNull();
  });

  it('⋯ Find in note opens the find bar, as Cmd+F does', async () => {
    await mountEditor();
    target.querySelector<HTMLButtonElement>('.editor-toolbar button[aria-label="Note actions"]')!.click();
    flushSync();
    click(item('Find in note'));
    await vi.waitFor(() => expect(target.querySelector('.find-bar .find-input')).not.toBeNull());
  });

  it('switching notes hides the menu in the same pass (gotcha #28)', async () => {
    const other: Note = { ...NOTE, uuid: 'fedcba98-0000-0000-0000-000000000000', id: 'msg-2', title: 'Other' };
    await mountEditor();
    notes.set([NOTE, other]);
    target.querySelector<HTMLButtonElement>('.editor-toolbar button[aria-label="Note actions"]')!.click();
    flushSync();
    expect(menu()).not.toBeNull();
    selectedNote.set(other);
    flushSync();
    expect(menu()).toBeNull();
    // …and switching back does not reopen it by itself.
    selectedNote.set(NOTE);
    flushSync();
    await tick();
    expect(menu()).toBeNull();
    expect(target.querySelector('.editor-toolbar button[aria-label="Note actions"]')!.getAttribute('aria-expanded')).toBe('false');
  });

  // Gotcha #14 / Important 1: the worker's refusal reaches the frontend
  // through the persisted state first. The toolbar must not say "Saved".
  it('a refusal held only by the persisted state reads "Not synced", with its reason on Try again and in the line', async () => {
    const n = { ...NOTE, local_version: 2 } as Note;
    persistenceByNote.set({
      [noteKey(n)]: { account_id: ACCOUNT_ID, uuid: n.uuid, local_version: 2, sync_state: 'dirty', push_blocked_reason: 'Folder is gone', push_blocked_by_remote: false },
    });
    try {
      await mountEditor(n);
      flushSync();
      const status = target.querySelector<HTMLElement>('.editor-toolbar .save-status')!;
      expect(status.textContent!.trim()).toBe('Not synced');
      expect(status.title).toBe('Saved on this device · Sync blocked');
      expect(target.querySelector('.editor-toolbar .retry-sync-btn')!.getAttribute('title')).toBe('Folder is gone');
      expect(target.querySelector('.meta-status[data-kind="notSynced"]')).not.toBeNull();
    } finally {
      persistenceByNote.set({});
    }
  });

  const TRUST: Trust = { tier: 'unreviewed', by: 'claude-code/2.1.0', at: '2026-10-07T01:00:00Z', event_id: 7, unreviewed_by: 'claude-code/2.1.0', unreviewed_at: '2026-10-07T01:00:00Z', created_by: 'claude-code/2.1.0', unrecorded_change: false, note_local_version: 3 };
  async function mountWithTrust(n: Note, verify: () => Promise<unknown>) {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_cached_note') return Promise.resolve(n);
      if (cmd === 'verify_note') return verify();
      return Promise.resolve([]);
    });
    unreviewedTrust.set({ [unreviewedKey(n.account_id!, n.uuid)]: { account: n.account_id!, trust: TRUST } });
    await mountEditor(n);
    // The SQLite re-read that gates Mark reviewed (reviewBlocked) lands.
    await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'get_cached_note')).toBe(true));
    await tick();
    flushSync();
    target.querySelector<HTMLButtonElement>('.editor-toolbar button[aria-label="Note actions"]')!.click();
    flushSync();
    expect(item('Mark reviewed')!.disabled).toBe(false);
  }

  it('a failed Mark reviewed from ⋯ is shown in the error bar', async () => {
    error.set(null);
    try {
      await mountWithTrust(NOTE, () => Promise.reject(new Error('offline')));
      click(item('Mark reviewed'));
      await vi.waitFor(() => expect(get(error)).toBe('Could not mark reviewed.'));
    } finally {
      error.set(null);
      unreviewedTrust.set({});
    }
  });

  it('Mark reviewed is in ⋯ whenever trust is pending, even when another status holds the line (R25)', async () => {
    const readOnly = { ...NOTE, push_blocked_reason: 'permanent: read-only', push_blocked_by_remote: true } as Note;
    try {
      await mountWithTrust(readOnly, () => Promise.resolve({ kind: 'verified', trust: { ...TRUST, tier: 'human_reviewed' } }));
      expect(target.querySelector('.meta-status')!.getAttribute('data-kind')).toBe('readonly');
      click(item('Mark reviewed'));
      await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'verify_note')).toBe(true));
    } finally {
      unreviewedTrust.set({});
    }
  });

  it('the save state is short, with the full sentence as its title and accessible name', async () => {
    await mountEditor({ ...NOTE, uuid: 'tmp:draft', id: '' });
    const status = target.querySelector<HTMLElement>('.editor-toolbar .save-status')!;
    expect(status.textContent!.trim()).toBe('Draft');
    expect(status.title).toBe('Draft — start typing to save');
    expect(status.getAttribute('aria-label')).toBe('Draft — start typing to save');
  });

  it('a blocked push reads "Not synced" with Try again', async () => {
    await mountEditor({ ...NOTE, push_blocked_reason: 'Folder is gone' } as Note);
    const status = target.querySelector<HTMLElement>('.editor-toolbar .save-status')!;
    expect(status.textContent!.trim()).toBe('Not synced');
    expect(status.title).toBe('Saved on this device · Sync blocked');
    expect(target.querySelector('.editor-toolbar .retry-sync-btn')!.textContent!.trim()).toBe('Try again');
  });
});

// Task 6.2 (spec §5.6, ruling R28): on a phone the toolbar row does not
// render, so ⋯ is the note PaneHeader's — NoteActionsButton, which App puts in
// the header. It opens the open editor's own NoteMoreMenu (one menu, one set
// of callbacks), as a sheet.
describe('NoteEditor on phone — the header ⋯', () => {
  let headerButton: ReturnType<typeof mount> | undefined;
  let header: HTMLElement;

  beforeEach(() => {
    isAndroid.set(true);
    layout.set('phone');
    header = document.createElement('div');
    document.body.appendChild(header);
  });
  afterEach(() => {
    if (headerButton) unmount(headerButton);
    headerButton = undefined;
    header.remove();
  });

  async function mountPhone(n: Note = NOTE) {
    notes.set([n]);
    selectedNote.set(n);
    headerButton = mount(NoteActionsButton, { target: header });
    mounted = mount(NoteEditor, { target });
    flushSync();
    await tick();
    flushSync();
  }
  const headerMore = () => header.querySelector<HTMLButtonElement>('button[aria-label="Note actions"]');
  const sheet = () => document.querySelector('dialog[data-presentation="sheet"]');

  it('phone ⋯ opens a sheet with Delete last', async () => {
    await mountPhone();
    // One ⋯ on a phone: the header's. The toolbar's does not render.
    expect(target.querySelector('.editor-toolbar')).toBeNull();
    // (The suite's own stand-alone `anchor` is outside both.)
    const mores = [header, target].flatMap((r) => Array.from(r.querySelectorAll('button[aria-label="Note actions"]')));
    expect(mores).toEqual([headerMore()]);
    expect(headerMore()!.getAttribute('aria-haspopup')).toBe('menu');
    expect(headerMore()!.getAttribute('aria-expanded')).toBe('false');
    click(headerMore());
    await vi.waitFor(() => expect(menu()).not.toBeNull());
    expect(menu()!.closest('dialog[data-presentation="sheet"]')).toBe(sheet());
    expect(headerMore()!.getAttribute('aria-expanded')).toBe('true');
    expect(labels()).toEqual([
      'Pin', 'Move to…', 'Tags…', 'Find in note', 'Copy wiki link', 'Show graph', 'Ask Jodd about this note',
      'Duplicate', 'Refetch from Gmail', 'Delete',
    ]);
    const del = item('Delete')!;
    expect(Array.from(menu()!.querySelectorAll('button.item')).at(-1)).toBe(del);
    expect(del.classList.contains('danger')).toBe(true);
    // Delete is the shared action, with the menu's close as onDecided (R6).
    click(del);
    expect(actions.deleteNoteWithConfirm).toHaveBeenCalledTimes(1);
    const [n, ask, onDecided] = vi.mocked(actions.deleteNoteWithConfirm).mock.calls[0];
    expect(n.uuid).toBe(NOTE.uuid);
    expect(typeof ask).toBe('function');
    expect(menu()).not.toBeNull(); // not closed until the action decides
    onDecided();
    flushSync();
    expect(menu()).toBeNull();
    expect(headerMore()!.getAttribute('aria-expanded')).toBe('false');
    await vi.waitFor(() => expect(sheet()).toBeNull());
  });

  it('phone ⋯ › Find in note opens the find bar and focuses it', async () => {
    await mountPhone();
    click(headerMore());
    await vi.waitFor(() => expect(menu()).not.toBeNull());
    click(item('Find in note'));
    expect(menu()).toBeNull();
    await vi.waitFor(() => expect(target.querySelector('.find-bar .find-input')).not.toBeNull());
    await vi.waitFor(() => expect(document.activeElement).toBe(target.querySelector('.find-bar .find-input')));
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
  });

  it('phone ⋯ › Show graph opens the graph once the sheet has gone', async () => {
    await mountPhone();
    click(headerMore());
    await vi.waitFor(() => expect(menu()).not.toBeNull());
    click(item('Show graph'));
    await vi.waitFor(() => expect(document.querySelector('dialog[open] h2')?.textContent).toBe('Note graph'));
    expect(menu()).toBeNull();
  });

  it('phone ⋯ › Mark reviewed runs the status popover\'s handler (R25)', async () => {
    const TRUST: Trust = { tier: 'unreviewed', by: 'claude-code/2.1.0', at: '2026-10-07T01:00:00Z', event_id: 7, unreviewed_by: 'claude-code/2.1.0', unreviewed_at: '2026-10-07T01:00:00Z', created_by: 'claude-code/2.1.0', unrecorded_change: false, note_local_version: 3 };
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_cached_note') return Promise.resolve(NOTE);
      if (cmd === 'verify_note') return Promise.resolve({ kind: 'verified', trust: { ...TRUST, tier: 'human_reviewed' } });
      return Promise.resolve([]);
    });
    unreviewedTrust.set({ [unreviewedKey(ACCOUNT_ID, NOTE.uuid)]: { account: ACCOUNT_ID, trust: TRUST } });
    try {
      await mountPhone();
      await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'get_cached_note')).toBe(true));
      await tick();
      flushSync();
      click(headerMore());
      await vi.waitFor(() => expect(menu()).not.toBeNull());
      expect(item('Mark reviewed')!.disabled).toBe(false);
      click(item('Mark reviewed'));
      await vi.waitFor(() => expect(invoke.mock.calls.some((c) => c[0] === 'verify_note')).toBe(true));
    } finally {
      unreviewedTrust.set({});
    }
  });

  it('switching notes hides the sheet in the same pass (gotcha #28)', async () => {
    const other: Note = { ...NOTE, uuid: 'fedcba98-0000-0000-0000-000000000000', id: 'msg-2', title: 'Other' };
    await mountPhone();
    notes.set([NOTE, other]);
    click(headerMore());
    await vi.waitFor(() => expect(menu()).not.toBeNull());
    selectedNote.set(other);
    flushSync();
    expect(menu()).toBeNull();
    expect(headerMore()!.getAttribute('aria-expanded')).toBe('false');
  });

  it('off the phone the header ⋯ renders nothing; the toolbar ⋯ is the one', async () => {
    isAndroid.set(true);
    layout.set('tablet');
    await mountPhone();
    expect(headerMore()).toBeNull();
    expect(target.querySelector('.editor-toolbar button[aria-label="Note actions"]')).not.toBeNull();
  });
});
