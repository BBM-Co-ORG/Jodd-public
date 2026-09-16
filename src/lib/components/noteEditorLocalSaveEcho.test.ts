// @vitest-environment jsdom
//
// Reported 2026-09-14 on a Gmail note ("character references") with ONE Jodd
// running: "Edited on another device" kept appearing while editing and
// pressing Cmd+Z. jodd.log shows no other writer at all — only this device's
// own `save_note (local-first)` → `sync_worker: pushed` pairs, with folder
// refreshes (`list_notes_in_label`) landing BETWEEN a local save and its push
// (10:43:37, 10:46:29, 10:48:47).
//
// A refresh answers from the SQLite row, and between a local save and its push
// that row holds the body this editor just SAVED — not the one the backend
// confirmed. If the user has typed (or undone) since, that body matches neither
// the live editor nor `lastRemoteBody`, and the banner blames another device
// for this device's own push lag.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT_ID = 'gmail:test@example.com';

const LOADED: Note = {
  uuid: 'uuid-character-references',
  id: 'msg-1',
  account_id: ACCOUNT_ID,
  title: 'character references',
  body_html: '<html><head></head><body><div>https://www.youtube.com/watch?v=one</div></body></html>',
  date: '2026-09-14T00:00:00Z',
  label: 'Notes',
  local_version: 1,
} as Note;

function bannerIsUp(host: HTMLElement): boolean {
  return Array.from(host.querySelectorAll('span')).some((s) =>
    (s.textContent ?? '').includes('Edited on another device'),
  );
}

describe('a refresh that returns this device\'s own unpushed save', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let editor: HTMLElement;

  function typeInto(html: string) {
    editor.innerHTML = html;
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
  }

  function savedBodies(): string[] {
    return invoke.mock.calls
      .filter(([cmd]) => cmd === 'save_note')
      .map(([, args]) => (args as { bodyHtml: string }).bodyHtml);
  }

  beforeEach(async () => {
    // jsdom has no layout, so no `innerText`. autoSave's data-loss guard reads
    // it, sees an empty editor against a non-empty stored body, and blocks the
    // save — which would make this test pass without ever reaching the banner.
    if (!('innerText' in HTMLElement.prototype)) {
      Object.defineProperty(HTMLElement.prototype, 'innerText', {
        configurable: true,
        get(this: HTMLElement) { return this.textContent ?? ''; },
      });
    }
    vi.useFakeTimers();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'save_note') {
        return Promise.resolve({ id: 'msg-1', uuid: LOADED.uuid, local_version: 2 });
      }
      return Promise.resolve([]);
    });
    notes.set([LOADED]);
    selectedNote.set(LOADED);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
    await tick();
    await tick();
    editor = host.querySelector('.editor-body') as HTMLElement;

    // Edit, and let autosave commit it locally. The sync worker has NOT pushed
    // it yet, so no `note-pushed` confirmation arrives.
    typeInto('<div>https://www.youtube.com/watch?v=one</div><div>https://www.youtube.com/watch?v=two</div>');
    await vi.advanceTimersByTimeAsync(1600);
    await tick();
    expect(savedBodies()).toHaveLength(1);
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it('does not accuse another device after the user keeps editing (or presses Cmd+Z)', async () => {
    const [locallySaved] = savedBodies();

    // Cmd+Z (or more typing) moves the editor past the saved body.
    typeInto('<div>https://www.youtube.com/watch?v=one</div>');

    // A folder refresh lands before the push: the SQLite row still holds the
    // body this editor saved.
    selectedNote.set({ ...get(selectedNote)!, body_html: locallySaved });
    flushSync();
    await tick();

    expect(bannerIsUp(host)).toBe(false);
  });

  it('still raises the banner for a body this device never wrote', async () => {
    typeInto('<div>https://www.youtube.com/watch?v=one</div>');

    selectedNote.set({
      ...get(selectedNote)!,
      body_html: '<html><head></head><body><div>edited in Apple Notes</div></body></html>',
    });
    flushSync();
    await tick();

    expect(bannerIsUp(host)).toBe(true);
  });
});
