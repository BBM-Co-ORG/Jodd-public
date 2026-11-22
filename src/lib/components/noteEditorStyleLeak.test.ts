// @vitest-environment jsdom
//
// WebKit copies the editor root's own font declarations onto text it moves on
// a line merge (Backspace at a line start, Delete at a line end) — because
// they are CSS variables, not plain values, it cannot tell they match the
// line the text lands in. Found in a live note 2026-11-17: four link lines
// wrapped in <span style="font-family: var(--font-note); font-size:
// var(--size-md);">, saved to Gmail and Notes.app. The variables mean nothing
// outside Jodd; the save leaves them out.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { accounts, currentAccount, selectedNote, selectedFolder, selectedSmartFolder, selectedTags, notes } from '../stores/notes';
import type { Note } from '../types';
import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = { id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-08-12T11:58:15Z', backend_kind: 'gmail', status: 'active' };
const doc = (inner: string) => `<html><head></head><body>${inner}</body></html>`;
const A: Note = {
  uuid: 'uuid-a', id: 'msg-a', account_id: ACCOUNT.id, title: 'Links',
  body_html: doc('<div>https://x.test/1</div><div>https://x.test/2</div>'), date: '2026-11-17T00:00:00Z', label: 'Notes', local_version: 1,
} as Note;

interface SaveArgs { existingUuid: string | null; bodyHtml: string }
const saves = () => invoke.mock.calls.filter(([c]) => c === 'save_note').map(([, a]) => a as SaveArgs);

describe('a line merge in the editor', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'save_note') return Promise.resolve({ id: 'msg-a2', uuid: args.existingUuid, local_version: 2 });
      return Promise.resolve([]);
    });
    accounts.set([ACCOUNT as never]);
    currentAccount.set(ACCOUNT.id);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    notes.set([A]);
    selectedNote.set(A);
  });
  afterEach(() => {
    vi.useRealTimers();
    selectedNote.set(null);
    notes.set([]);
  });

  it('saves the merged text without the editor\'s own CSS variables', async () => {
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(NoteEditor, { target });
    flushSync();
    await vi.advanceTimersByTimeAsync(0);
    await tick();
    const editor = target.querySelector('.editor-body') as HTMLElement;
    // The DOM WebKit leaves after Backspace at the start of line 2.
    editor.innerHTML =
      '<div>https://x.test/1<span style="font-family: var(--font-note); font-size: var(--size-md);">https://x.test/2</span></div>';
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(2000);
    await tick();

    const saved = saves().filter((s) => s.existingUuid === 'uuid-a');
    expect(saved).toHaveLength(1);
    expect(saved[0].bodyHtml).toContain('<div>https://x.test/1https://x.test/2</div>');
    expect(saved[0].bodyHtml).not.toContain('var(--');
    unmount(host);
    target.remove();
  });
});
