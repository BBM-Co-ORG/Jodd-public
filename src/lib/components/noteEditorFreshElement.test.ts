// @vitest-environment jsdom
//
// Reported 2026-09-26: "added one line, Cmd+Z took away two". WebKit's undo
// history is a page-wide list of edit commands holding node references, and
// `editorEl.innerHTML = …` neither clears it nor detaches the editor root. A
// stale step that re-inserts a node into that root therefore lands in whatever
// body is showing now. Measured in real WebKit
// (scripts/webkit-editor-harness, keys/render-undo.txt): delete a line in note
// A, open note B, add a line, Cmd+Z twice → A's deleted line appears in B.
// Against a FRESH element the stale steps act on the detached old root.
//
// jsdom has no undo history, so this pins the mechanism the harness proved
// sufficient: every full body render mounts a new editor element.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';

function noteOf(uuid: string, body: string, title: string): Note {
  return {
    uuid,
    id: `msg-${uuid}`,
    account_id: ACCOUNT_ID,
    title,
    body_html: body,
    date: '2026-09-26T00:00:00Z',
    label: 'Notes',
  } as Note;
}

const A = noteOf('uuid-a', '<html><body><div>A1</div><div>A2</div></body></html>', 'A');
const B = noteOf('uuid-b', '<html><body><div>B1</div><div>B2</div></body></html>', 'B');

function editorOf(host: HTMLElement): HTMLElement {
  const el = host.querySelector<HTMLElement>('.editor-body');
  if (!el) throw new Error('no editor');
  return el;
}

describe('a full body render mounts a fresh editor element', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      return Promise.resolve([]);
    });
    notes.set([A, B]);
    selectedNote.set(A);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await tick();
    await tick();
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
  });

  it('switching notes replaces the element instead of rewriting its innerHTML', async () => {
    const before = editorOf(host);
    expect(before.textContent).toBe('A1A2');

    selectedNote.set(B);
    flushSync();
    await tick();
    await tick();

    const after = editorOf(host);
    expect(after).not.toBe(before);
    expect(before.isConnected).toBe(false);
    expect(after.textContent).toBe('B1B2');
  });

  it('the fresh element is still wired: typing into it reaches the store', async () => {
    selectedNote.set(B);
    flushSync();
    await tick();
    await tick();

    const el = editorOf(host);
    el.innerHTML = '<div>B1</div><div>B2</div><div>NEW</div>';
    el.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();

    let body = '';
    selectedNote.subscribe((n) => { body = n?.body_html ?? ''; })();
    expect(body).toContain('NEW');
  });
});
