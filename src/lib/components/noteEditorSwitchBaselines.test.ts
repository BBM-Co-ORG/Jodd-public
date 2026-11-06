// @vitest-environment jsdom
//
// Measured live 2026-09-05 on a Gmail account with ONE Jodd running and
// nobody else editing: opening a note raised "Edited on another device"
// every time. On-screen instrumentation showed why — `lastRemoteBody` still
// held the PREVIOUSLY VIEWED note's body:
//
//   incoming(266B)   …>Fetch and execute the appropriate instructions…
//   remembered(383B) …><div>https://trustgraph.ai/</div>…
//
// The cause is a timing split, not bad logic. `editorUuid` is assigned
// synchronously while the three comparison baselines are assigned one
// microtask later, inside `tick().then()` — that callback exists because
// `editorEl.innerHTML` needs `bind:this` to have run, which the baselines do
// not. Any reactive pass landing in the gap sees `uuidChanged` already false
// (so the banner is not cleared), `bodyChanged` true and `isEcho` false
// against the old note's body — and the banner latches.
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
    date: '2026-09-05T00:00:00Z',
    label: 'Notes',
  } as Note;
}

// The two real bodies from the live capture, trimmed to their distinguishing
// halves. Different lengths and different content: not a byte-level echo miss.
const PREVIOUS = noteOf('uuid-trustgraph', '<html><body><div>https://trustgraph.ai/</div></body></html>', 'Links');
const OPENED = noteOf('uuid-cloudflare', '<html><body>Fetch and execute the appropriate instructions</body></html>', 'Cloudflare Agent Setup');

function bannerIsUp(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll('span')).some((s) =>
    (s.textContent ?? '').includes('Edited on another device'),
  );
}

describe('switching notes must move identity and comparison baselines together', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(() => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      return Promise.resolve([]);
    });
    notes.set([PREVIOUS, OPENED]);
    selectedNote.set(PREVIOUS);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
  });

  // The symptom exactly as reported: open a note, nobody else touching it,
  // banner up. No switch and no external store touch are needed — assigning
  // `title` inside the same reactive block is itself a reactive write, so the
  // block re-enters before its own deferred baselines have been assigned.
  it('does not accuse another device when a note is simply opened', async () => {
    await tick();
    await tick();
    expect(bannerIsUp(host)).toBe(false);
  });

  it('does not accuse another device when a pass lands between the switch and the baselines', async () => {
    await tick();
    await tick();

    selectedNote.set(OPENED);
    flushSync();
    // A second pass in the gap. Deliberately NOT awaited in between, because
    // awaiting is what closes the gap and would make this test pass against
    // the bug it is written for.
    selectedNote.set({ ...OPENED });
    flushSync();

    await tick();
    await tick();

    expect(bannerIsUp(host)).toBe(false);
  });
});
