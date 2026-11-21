// @vitest-environment jsdom
//
// The editor's two read-only dialogs — the connections graph and the
// keyboard-shortcuts cheat sheet — render through the shared Modal. Modal owns
// Esc, the backdrop, focus return, the Tab trap and the Android Back entry;
// NoteEditor owns only the open flags and what a graph node does.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes, selectedFolder } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const NOTE: Note = {
  uuid: 'uuid-dialogs',
  id: 'msg-1',
  account_id: 'gmail:test@example.com',
  title: 'Dialogs',
  body_html: '<p>body</p>',
  date: '2026-09-14T00:00:00Z',
  label: 'Notes/Projects',
} as Note;

let connections: { outgoing: Note[]; backlinks: Note[] };
let host: HTMLElement;
let app: Record<string, unknown>;

async function mountEditor() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'platform_name') return Promise.resolve('macos');
    if (cmd === 'note_connections') return Promise.resolve(connections);
    return Promise.resolve([]);
  });
  notes.set([NOTE]);
  selectedNote.set(NOTE);
  host = document.createElement('div');
  document.body.appendChild(host);
  app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  await tick();
  await tick();
  await tick();
  flushSync();
}

beforeEach(() => {
  connections = { outgoing: [], backlinks: [] };
});
afterEach(async () => {
  await unmount(app);
  host.remove();
  document.body.replaceChildren();
  selectedNote.set(null);
  notes.set([]);
  vi.clearAllMocks();
});

const dialog = () => host.querySelector('dialog') as HTMLDialogElement | null;
const click = (selector: string) => (host.querySelector(selector) as HTMLElement).click();

// Task 5.2: Graph and the shortcuts cheat sheet moved from toolbar buttons
// into the toolbar's ⋯ menu. Opens ⋯ and picks the item by its label.
async function pickFromMoreMenu(root: ParentNode, label: string) {
  (root.querySelector('.editor-toolbar button[aria-label="Note actions"]') as HTMLElement).click();
  flushSync();
  const item = Array.from(document.querySelectorAll<HTMLButtonElement>('[data-testid="note-more-menu"] button.item')).find(
    (b) => b.querySelector('.label')?.textContent?.trim() === label,
  );
  if (!item) throw new Error(`no "${label}" in the ⋯ menu`);
  item.click();
  // The item runs once the menu's close has rendered.
  await tick();
  flushSync();
}
async function esc() {
  dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await tick();
  flushSync();
}

describe('keyboard-shortcuts cheat sheet', () => {
  it('opens as a Modal titled Keyboard shortcuts, with the table', async () => {
    await mountEditor();
    expect(dialog()).toBeNull();
    await pickFromMoreMenu(host, 'Keyboard shortcuts');
    flushSync();
    expect(dialog()!.hasAttribute('data-presentation')).toBe(true);
    expect(dialog()!.querySelector('h2')!.textContent).toBe('Keyboard shortcuts');
    expect(dialog()!.querySelector('.cheat-table')).not.toBeNull();
  });

  it('Esc closes it', async () => {
    await mountEditor();
    await pickFromMoreMenu(host, 'Keyboard shortcuts');
    flushSync();
    await esc();
    expect(dialog()).toBeNull();
  });

  it('has exactly one close control', async () => {
    await mountEditor();
    await pickFromMoreMenu(host, 'Keyboard shortcuts');
    flushSync();
    const closers = [...dialog()!.querySelectorAll('button')].filter(
      (b) => b.getAttribute('aria-label') === 'Close' || b.textContent?.trim() === 'Close',
    );
    expect(closers).toHaveLength(1);
  });
});

describe('connections graph', () => {
  it('opens as a Modal titled Note graph', async () => {
    await mountEditor();
    await pickFromMoreMenu(host, 'Show graph');
    flushSync();
    expect(dialog()!.hasAttribute('data-presentation')).toBe(true);
    expect(dialog()!.querySelector('h2')!.textContent).toBe('Note graph');
    // The note's folder is always a node, so the graph is never empty here.
    expect(dialog()!.querySelector('.graph-svg')).not.toBeNull();
  });

  it('Esc closes it', async () => {
    await mountEditor();
    await pickFromMoreMenu(host, 'Show graph');
    flushSync();
    await esc();
    expect(dialog()).toBeNull();
  });

  it('a folder node navigates to the folder and closes the graph', async () => {
    await mountEditor();
    await pickFromMoreMenu(host, 'Show graph');
    flushSync();
    (dialog()!.querySelector('[aria-label="Go to folder Notes/Projects"]') as HTMLElement).dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    );
    await tick();
    flushSync();
    expect(get(selectedFolder)).toBe('Notes/Projects');
    expect(dialog()).toBeNull();
  });

  it('a note node opens that note and closes the graph', async () => {
    const other = { ...NOTE, uuid: 'uuid-other', id: 'msg-2', title: 'Other', label: 'Notes/Projects' } as Note;
    connections = { outgoing: [other], backlinks: [] };
    await mountEditor();
    await pickFromMoreMenu(host, 'Show graph');
    flushSync();
    (dialog()!.querySelector('[aria-label="Open note Other"]') as HTMLElement).dispatchEvent(
      new MouseEvent('click', { bubbles: true }),
    );
    await tick();
    flushSync();
    expect(get(selectedNote)?.uuid).toBe('uuid-other');
    expect(dialog()).toBeNull();
  });
});
