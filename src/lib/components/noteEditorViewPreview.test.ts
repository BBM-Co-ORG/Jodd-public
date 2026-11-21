// @vitest-environment jsdom
//
// The live strip on a view note (spec §8.5) counts matches across the whole
// account, so typing is not the only thing that moves it. Gotcha #6: a body
// that arrives from outside (a pull re-renders the editor) and rows that change
// elsewhere (`viewDataRevision`) must re-run `preview_view` too — mounted, so
// the route from the store to the strip's request is what is pinned.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import { viewDataRevision } from '../views/viewsStore';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

const ACCOUNT = 'gmail:test@example.com';
const VIEW_NOTE = {
  uuid: 'uuid-view',
  id: 'msg-view',
  account_id: ACCOUNT,
  title: 'Due soon',
  body_html: '<div>Show: tasks</div><div>Due: today</div>',
  date: '2026-11-19T00:00:00Z',
  label: 'Notes/__Views__',
} as Note;

let cleanup: (() => void) | null = null;

afterEach(() => {
  cleanup?.();
  cleanup = null;
  selectedNote.set(null);
  notes.set([]);
  vi.clearAllMocks();
});

const previewCalls = () =>
  invoke.mock.calls.filter(([cmd]) => cmd === 'preview_view').map(([, a]) => (a as { bodyHtml: string }).bodyHtml);

async function waitFor(pred: () => boolean, ms = 2000) {
  const end = Date.now() + ms;
  while (!pred() && Date.now() < end) {
    await new Promise((r) => setTimeout(r, 20));
    flushSync();
  }
}

async function openViewNote() {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'platform_name') return Promise.resolve('macos');
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    if (cmd === 'preview_view') return Promise.resolve({ rows: { Tasks: [] }, notices: [] });
    return Promise.resolve([]);
  });
  notes.set([VIEW_NOTE]);
  selectedNote.set(VIEW_NOTE);
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = mount(NoteEditor, { target: host }) as Record<string, unknown>;
  cleanup = () => {
    unmount(app);
    host.remove();
  };
  await tick();
  await tick();
  flushSync();
  await waitFor(() => previewCalls().length > 0);
  expect(previewCalls().length, 'opening a view note runs the strip once').toBeGreaterThan(0);
  invoke.mockClear();
}

describe('the view note strip re-runs on changes that are not typing (gotcha #6)', () => {
  it('a body replaced from another device re-runs preview_view on the new body', async () => {
    await openViewNote();
    // A pull hands the open note a new body; focus is outside the editor, so
    // it renders (shouldRenderExternalBody) without any input event.
    const pulled = { ...VIEW_NOTE, body_html: '<div>Show: tasks</div><div>Due: tomorrow</div>' };
    notes.set([pulled]);
    selectedNote.set(pulled);
    flushSync();
    await waitFor(() => previewCalls().some((b) => b.includes('tomorrow')));

    expect(previewCalls().some((b) => b.includes('Due: tomorrow'))).toBe(true);
  });

  it('rows changing elsewhere (viewDataRevision) re-run preview_view', async () => {
    await openViewNote();
    viewDataRevision.update((n) => n + 1);
    flushSync();
    await waitFor(() => previewCalls().length > 0);

    expect(previewCalls().length).toBeGreaterThan(0);
  });
});
