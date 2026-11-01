// @vitest-environment jsdom
//
// The make-a-note dialog's layout (spec 2026-10-28 §7): the first line says
// where the result goes, the source is the textarea or an existing note (one
// at a time), and "Add to" shows only when it is a real choice.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { accounts, currentAccount, selectedFolder, notes, selectedNote, folderSuggestions, capabilitiesByAccount } from '../stores/notes';
import type { Account, IngestAnalysis, Note } from '../types';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ACCT = 'gmail:a@b.com';
const TARGET: Note = {
  uuid: 'T-UUID', id: 'm1', account_id: ACCT, title: 'Reading list',
  body_html: '<p>x</p>', date: '2026-09-15T00:00:00Z', label: 'Notes/Reading',
} as Note;
const LINKS: IngestAnalysis = {
  sources: [{ url: 'https://youtu.be/jXtnhyro-QE', kind: 'youtube', supported: true, reason: null, duplicate_owner: null, may_transcribe: false, comments_closed: false }],
  mostly_urls: true, context_text: '', context_chars: 0, stt_host: null, stt_insecure: false,
};

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('the make-a-note dialog layout', () => {
  let host: HTMLElement;
  let app: Record<string, unknown> | undefined;
  let captureDefault: unknown;
  let nextAnalysis: IngestAnalysis;

  beforeEach(() => {
    vi.useFakeTimers();
    captureDefault = 'Notes/Inbox';
    nextAnalysis = { ...LINKS, sources: [], mostly_urls: false };
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'list_capture_folders': return Promise.resolve({ default: captureDefault, folders: [] });
        case 'analyze_ingest_sources': return Promise.resolve(nextAnalysis);
        case 'search_notes': return Promise.resolve([TARGET]);
        default: return Promise.resolve([]);
      }
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    accounts.set([{ id: ACCT, email: 'a@b.com' } as Account]);
    currentAccount.set(ACCT);
    capabilitiesByAccount.set({});
    selectedFolder.set('Notes');
    notes.set([]);
    selectedNote.set(null);
    folderSuggestions.set({});
    host = document.createElement('div');
    document.body.appendChild(host);
  });

  afterEach(() => {
    if (app) unmount(app);
    app = undefined;
    host.remove();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  async function open() {
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
  }
  const line = () => host.querySelector('.destination-line')!.textContent;
  const button = (label: string) =>
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === label);
  async function type(text: string) {
    const textarea = host.querySelector('textarea')!;
    textarea.value = text;
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
  }

  it('opens with where the result goes, the backend\'s own answer', async () => {
    await open();
    expect(host.querySelector('h2')!.textContent).toBe('Make a note with AI');
    expect(line()).toBe('Saves to Inbox in a@b.com.');
    expect(invoke).toHaveBeenCalledWith('list_capture_folders', { accountId: ACCT });
  });

  it('names the root when the backend files there (no Inbox, no folder writes)', async () => {
    captureDefault = 'Notes';
    await open();
    expect(line()).toBe('Saves to Notes in a@b.com.');
  });

  it('before the backend answers, the capability decides', async () => {
    invoke.mockImplementation((cmd: string) => (cmd === 'list_capture_folders' ? new Promise(() => {}) : Promise.resolve([])));
    capabilitiesByAccount.set({ [ACCT]: { has_trash: true, writes: { notes: true, relocate: true, folders: false, sidecars: false } } as never });
    await open();
    expect(line()).toBe('Saves to Notes in a@b.com.');
  });

  it('with Existing note chosen, the line names the note it adds to', async () => {
    await open();
    button('Existing note…')!.click();
    flushSync();
    expect(line()).toBe('Adds to the note you pick below.');
    const search = host.querySelector<HTMLInputElement>('input[placeholder^="Search notes"]')!;
    search.value = 'Reading';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await vi.advanceTimersByTimeAsync(200);
    await settle();
    host.querySelector<HTMLButtonElement>('.target-results button')!.click();
    flushSync();
    expect(line()).toBe('Adds to “Reading list”.');
  });

  it('the source is the textarea or an existing note, one at a time', async () => {
    await open();
    expect(host.querySelector('textarea')).not.toBeNull();
    button('Use an existing note instead')!.click();
    flushSync();
    expect(host.querySelector('textarea')).toBeNull();
    expect(host.textContent).toContain('Source note');
    button('Paste text instead')!.click();
    flushSync();
    expect(host.querySelector('textarea')).not.toBeNull();
  });

  it('"Add to" is hidden while ticked links will run — they always make a new note', async () => {
    await open();
    expect(button('Existing note…')).toBeDefined();
    nextAnalysis = LINKS;
    await type('https://youtu.be/jXtnhyro-QE');
    expect(host.querySelector('.ingest-sources input[type=checkbox]:checked')).not.toBeNull();
    expect(button('Existing note…')).toBeUndefined();
    expect(button('New note')).toBeUndefined();
  });

  it('says what the chosen option makes, on one line under the choice', async () => {
    await open();
    expect(host.querySelector('.option-line')!.textContent).toBe('Jodd will distill it into structured key points.');
    button('Summarize')!.click();
    flushSync();
    expect(host.querySelector('.option-line')!.textContent).toBe('Jodd will summarize it.');
  });
});
