// @vitest-environment jsdom
//
// The open note's property block and task due dates reach the screen as CSS
// highlights (editor/propertyPaint.ts) — mounted, so the wiring is pinned and
// not only the scan: the note opens, a frame passes, the highlights hold the
// recognized text, and the body is exactly what it was.
import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { selectedNote, notes } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteEditor from './NoteEditor.svelte';

class FakeHighlight {
  ranges: Range[];
  priority?: number;
  constructor(...ranges: Range[]) {
    this.ranges = ranges;
  }
}

let highlights: Map<string, FakeHighlight>;
let cleanup: (() => void) | null = null;

beforeEach(() => {
  highlights = new Map();
  vi.stubGlobal('CSS', { highlights, supports: () => false, escape: (s: string) => s });
  vi.stubGlobal('Highlight', FakeHighlight);
  if (!Range.prototype.getBoundingClientRect) {
    Range.prototype.getBoundingClientRect = () => new DOMRect(0, 0, 0, 0);
  }
});

afterEach(() => {
  cleanup?.();
  cleanup = null;
  selectedNote.set(null);
  notes.set([]);
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const pad = (n: number) => String(n).padStart(2, '0');
function isoDaysFromToday(days: number): string {
  const d = new Date();
  d.setDate(d.getDate() + days);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

async function open(body_html: string) {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'platform_name') return Promise.resolve('macos');
    if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
    return Promise.resolve([]);
  });
  const n = {
    uuid: 'uuid-props',
    id: 'msg-1',
    account_id: 'gmail:test@example.com',
    title: 'Project',
    body_html,
    date: '2026-11-19T00:00:00Z',
    label: 'Notes',
  } as Note;
  notes.set([n]);
  selectedNote.set(n);
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
  return host.querySelector('.editor-body') as HTMLElement;
}

const texts = (name: string) => highlights.get(name)?.ranges.map(String) ?? [];

describe('NoteEditor paints recognized properties and due dates', () => {
  it('the block and an overdue task date are highlighted, and the body is untouched', async () => {
    const yesterday = isoDaysFromToday(-1);
    const editor = await open(
      `<div>Status:&nbsp;active</div><div>Due: 25/11</div><div><br></div>` +
        `<div class="jodd-task"><input type="checkbox">&nbsp;ship 📅 ${yesterday}</div>`,
    );
    const before = editor.innerHTML;
    await vi.waitFor(() => expect(texts('jodd-prop-key')).toEqual(['Status', 'Due']));
    expect(texts('jodd-prop-value')).toEqual(['active']);
    expect(texts('jodd-prop-warn')).toEqual(['25/11']);
    expect(texts('jodd-due-overdue')).toEqual([yesterday]);
    expect(highlights.has('jodd-prop-gone')).toBe(true);
    expect(editor.innerHTML).toBe(before);
  });

  it('a note with nothing recognized registers no property highlight at all', async () => {
    await open('<div>just text</div><div>Status: below the first line</div>');
    // Let a scan frame pass: links register theirs, properties stay away.
    await vi.waitFor(() => expect(highlights.has('jodd-link')).toBe(true));
    expect([...highlights.keys()].filter((k) => k.startsWith('jodd-prop') || k.startsWith('jodd-due'))).toEqual([]);
  });
});
