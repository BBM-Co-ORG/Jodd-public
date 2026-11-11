// @vitest-environment jsdom
//
// Reported 2026-10-03 (Gmail, v0.31.1): scroll to the bottom of a long note,
// type, and a few seconds later the editor jumps back to the top. The push is
// insert-new + trash-old, and the refresh after it reads back a body that is
// the same note in different bytes — a same-note re-render, which is correct.
// But every full render mounts a FRESH `.editor-body` (see
// noteEditorFreshElement.test.ts), and that element is its own scroll
// container, so it starts at scrollTop 0. `innerHTML =` on the old element
// used to keep the position for free.
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
    date: '2026-10-03T00:00:00Z',
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

// jsdom has no layout: scrollTop always reads 0 and ignores writes. Back it
// with a per-element store so the test can see what the editor writes.
const scrollStore = new WeakMap<Element, number>();
const original = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop');

async function settle() {
  flushSync();
  await tick();
  await tick();
}

describe('a same-note re-render keeps the scroll position', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    Object.defineProperty(Element.prototype, 'scrollTop', {
      configurable: true,
      get(this: Element) { return scrollStore.get(this) ?? 0; },
      set(this: Element, v: number) { scrollStore.set(this, v); },
    });
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
    if (original) Object.defineProperty(Element.prototype, 'scrollTop', original);
  });

  it('the refreshed body of the open note lands where the user was reading', async () => {
    editorOf(host).scrollTop = 640;

    // The same note read back after a push: same text, different bytes.
    selectedNote.set({ ...A, id: 'msg-uuid-a-2', body_html: '<html><head></head><body><div>A1</div><div>A2</div></body></html>' });
    await settle();

    const after = editorOf(host);
    expect(after.textContent).toBe('A1A2');
    expect(after.scrollTop).toBe(640);
  });

  it('opening a different note still starts at the top', async () => {
    editorOf(host).scrollTop = 640;

    selectedNote.set(B);
    await settle();

    expect(editorOf(host).scrollTop).toBe(0);
  });
});
