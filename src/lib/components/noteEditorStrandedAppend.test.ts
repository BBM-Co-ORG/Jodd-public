// @vitest-environment jsdom
//
// Gotcha #37. An agent append written before the fix sits AFTER `</html>` in
// the stored body. The editor used to read only `<body>…</body>`, so the
// appended text never appeared — and the editor's next save, built from what
// it showed, would drop it. Found in the note-provenance live pass on
// 2026-10-08, with a body shaped exactly like the one below. Pinned by
// MOUNTING the editor: the rendered DOM is what the user sees and what a
// save is built from.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const STRANDED: Note = {
  uuid: 'uuid-stranded', id: 'msg-stranded', account_id: 'gmail:test@example.com', title: 'Provenance test A',
  body_html:
    '<html><head></head><body style="overflow-wrap: break-word; -webkit-nbsp-mode: space; line-break: after-white-space;">' +
    'Written by a human in the Jodd app.</body></html>' +
    '<p>Appended by an OLD jodd-mcp build (provenance live test A2).</p>\n',
  date: '2026-10-08T01:28:14Z', label: 'Notes/_test_',
} as Note;

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('a body with content stranded after </html>', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      return Promise.resolve([]);
    });
    notes.set([STRANDED]);
    selectedNote.set(STRANDED);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await settle();
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
  });

  it('shows the stranded text in the editor', () => {
    const body = host.querySelector('.editor-body') as HTMLElement;
    expect(body.textContent).toContain('Written by a human in the Jodd app.');
    expect(body.textContent).toContain('Appended by an OLD jodd-mcp build');
  });
});
