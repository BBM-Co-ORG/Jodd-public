// @vitest-environment jsdom
//
// The ⓘ cheatsheet used to say "Cmd/Ctrl" for every chord while the handler
// accepted either key on every platform — so the table was technically true
// and practically wrong: Ctrl+Cmd+Z undid on macOS, Cmd+Y redid, and Ctrl+E /
// Ctrl+K lost their system meaning. The handler now takes ⌘ alone on macOS and
// Ctrl alone elsewhere (shortcutKeys.ts); this pins the table to that rule.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const NOTE: Note = {
  uuid: 'uuid-shortcuts',
  id: 'msg-1',
  account_id: 'gmail:test@example.com',
  title: 'Shortcuts',
  body_html: '<p>body</p>',
  date: '2026-09-14T00:00:00Z',
  label: 'Notes',
} as Note;

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

async function openCheatsheet(platform: string) {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'platform_name') return Promise.resolve(platform);
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
  notes.set([NOTE]);
  selectedNote.set(NOTE);
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  await tick();
  await tick(); // platform_name resolves
  flushSync();
  await pickFromMoreMenu(host, 'Keyboard shortcuts');
  flushSync();
  // One string per row, cells joined by a space ("Undo Cmd+Z") — adjacent <td>s
  // have no whitespace between them, so textContent alone runs them together.
  const rows = Array.from(host.querySelectorAll('.cheat-table tr')).map((r) =>
    Array.from(r.children).map((c) => (c.textContent ?? '').replace(/\s+/g, ' ').trim()).join(' '),
  );
  const row = (name: string) => rows.find((r) => r.startsWith(name)) ?? '';
  const titles = Array.from(host.querySelectorAll('.fmt-btn')).map((b) => b.getAttribute('title') ?? '');
  return { host, app, rows, row, titles };
}

describe('editor shortcut labels match the keys the handler accepts', () => {
  let cleanup: (() => void) | null = null;
  afterEach(() => {
    cleanup?.();
    cleanup = null;
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
  });

  it('on macOS: Cmd and Option, and no Ctrl+Y', async () => {
    const { host, app, rows, row, titles } = await openCheatsheet('macos');
    cleanup = () => { unmount(app); host.remove(); };

    expect(row('Undo')).toBe('Undo Cmd+Z');
    expect(row('Redo')).toBe('Redo Cmd+Shift+Z');
    expect(row('Bold')).toBe('Bold Cmd+B');
    expect(row('Heading 1 / 2 / 3')).toBe('Heading 1 / 2 / 3 Cmd+Option+1/2/3');
    expect(row('Find / replace in note')).toBe('Find / replace in note Cmd+F');
    expect(titles).toContain('Heading 2 (Cmd+Option+2)');
    expect(rows.join('\n')).not.toMatch(/Ctrl|Cmd\/Ctrl/);
    expect(titles.join('\n')).not.toMatch(/Ctrl/);
  });

  it('elsewhere: Ctrl and Alt, with Ctrl+Y as the second redo', async () => {
    const { host, app, rows, row, titles } = await openCheatsheet('windows');
    cleanup = () => { unmount(app); host.remove(); };

    expect(row('Undo')).toBe('Undo Ctrl+Z');
    expect(row('Redo')).toBe('Redo Ctrl+Shift+Z or Ctrl+Y');
    expect(row('Heading 1 / 2 / 3')).toBe('Heading 1 / 2 / 3 Ctrl+Alt+1/2/3');
    expect(titles).toContain('Strikethrough (Ctrl+Shift+X)');
    expect(rows.join('\n')).not.toMatch(/Cmd/);
    expect(titles.join('\n')).not.toMatch(/Cmd/);
  });
});
