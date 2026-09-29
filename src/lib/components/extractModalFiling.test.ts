// @vitest-environment jsdom
//
// After a new-note Extract: navigate to the folder the backend chose, then —
// sequentially, because concurrent agent-CLI runs are unmeasured (gotcha
// #7) — ask for a folder proposal and only then for wiki links. A failed
// proposal is logged and never shown.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import type { Note } from '../types';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ACCT = 'gmail:a@b.com';
const CREATED: Note = {
  uuid: 'new-uuid', id: '', account_id: ACCT, title: 'Extracted', body_html: '<p>x</p>',
  date: '2026-09-15T00:00:00Z', label: 'Notes/Inbox',
} as Note;

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe('LessonExtractModal filing', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let suggestion: ReturnType<typeof deferred<unknown>>;

  beforeEach(async () => {
    suggestion = deferred();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'check_duplicate_citations': return Promise.resolve([]);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'extract_note': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_note_tags': return Promise.resolve([]);
        case 'get_ai_limits': return { automatic_enrichment: true };
        case 'suggest_note_folder': return suggestion.promise;
        case 'suggest_wiki_links': return Promise.resolve({ auto_links: [], proposed_appends: [] });
        default: return Promise.resolve([]);
      }
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    currentAccount.set(ACCT);
    selectedFolder.set('Notes');
    notes.set([]);
    selectedNote.set(null);
    folderSuggestions.set({});
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    vi.restoreAllMocks();
  });

  async function runExtract() {
    const textarea = host.querySelector('textarea')!;
    textarea.value = 'some source text';
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    const btn = Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Extract')!;
    btn.click();
    await settle();
  }

  const commands = () => invoke.mock.calls.map((c) => c[0] as string);

  it('saves and opens the main note with automatic enrichment disabled', async () => {
    const original = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, ...args: unknown[]) => cmd === 'get_ai_limits' ? Promise.resolve({ automatic_enrichment: false }) : original(cmd, ...args));
    await runExtract();
    expect(get(selectedNote)?.uuid).toBe(CREATED.uuid);
    expect(commands()).toContain('extract_note');
    expect(commands()).not.toContain('suggest_note_folder');
    expect(commands()).not.toContain('suggest_wiki_links');
  });

  it('checks enrichment again between folder and link calls', async () => {
    await runExtract();
    const original = invoke.getMockImplementation()!;
    invoke.mockImplementation((cmd: string, ...args: unknown[]) => cmd === 'get_ai_limits' ? Promise.resolve({ automatic_enrichment: false }) : original(cmd, ...args));
    suggestion.resolve({ kind: 'none_fits' });
    await settle();
    expect(commands()).toContain('suggest_note_folder');
    expect(commands()).not.toContain('suggest_wiki_links');
    expect(get(selectedNote)?.uuid).toBe(CREATED.uuid);
  });

  it('navigates to the label the backend returned', async () => {
    await runExtract();
    expect(get(selectedFolder)).toBe('Notes/Inbox');
    expect(invoke.mock.calls.find((c) => c[0] === 'list_cached_notes_in_folder')?.[1])
      .toMatchObject({ path: 'Notes/Inbox' });
    expect(JSON.stringify(invoke.mock.calls)).not.toContain('__Extracts__');
  });

  it('asks for a folder first and for wiki links only after that answer', async () => {
    await runExtract();
    expect(commands()).toContain('suggest_note_folder');
    expect(commands()).not.toContain('suggest_wiki_links');

    // Ruling 1: suggested fixtures carry `uuid` (gotcha #16 — a later fix
    // made recordFolderSuggestion key on outcome.uuid, not the passed-in
    // uuid); resolve with the created note's uuid so the suggestion lands
    // under a key the modal (and a real editor) would actually look up.
    suggestion.resolve({ kind: 'suggested', uuid: CREATED.uuid, path: 'Notes/Trading', reason: 'r' });
    await settle();
    const order = commands();
    expect(order.indexOf('suggest_note_folder')).toBeLessThan(order.indexOf('suggest_wiki_links'));
    const rootId = invoke.mock.calls.find(c => c[0] === 'extract_note')?.[1].requestId;
    expect(rootId).toBeTruthy();
    for (const command of ['suggest_note_folder', 'suggest_wiki_links']) {
      expect(invoke.mock.calls.find(c => c[0] === command)?.[1]).toMatchObject({ parentRequestId: rootId });
    }
    expect(Object.values(get(folderSuggestions))).toEqual([{ path: 'Notes/Trading', reason: 'r' }]);
  });

  // Measured 2026-09-16 on an Android phone: a folder settle for Notes/Inbox
  // ran before the new note had reached Gmail, found 0 messages, and dropped
  // the note from $notes. Auto-link then saved it with `label: ''`, which the
  // Gmail vertical files under the root `Notes` — the note left Inbox with no
  // user action behind it.
  const AUTO_LINK = { auto_links: [{ uuid: 'other', title: 'Other', slug: 'other' }], proposed_appends: [] };

  it('auto-link keeps the label SQLite holds when the note has left $notes', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'extract_note': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_cached_notes': return Promise.resolve([{ ...CREATED, id: 'gmail-id-1' }]);
        case 'get_ai_limits': return { automatic_enrichment: true };
        case 'suggest_note_folder': return suggestion.promise;
        case 'suggest_wiki_links': return Promise.resolve(AUTO_LINK);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        default: return Promise.resolve([]);
      }
    });
    await runExtract();
    notes.set([]);
    suggestion.resolve({ kind: 'no_suggestion' });
    await settle();

    const save = invoke.mock.calls.find((c) => c[0] === 'save_note');
    expect(save?.[1]).toMatchObject({ existingUuid: CREATED.uuid, label: 'Notes/Inbox', existingGmailId: 'gmail-id-1' });
    expect(save?.[1].bodyHtml).toContain('[[other]]');
  });

  it('auto-link writes nothing when the note is in neither $notes nor SQLite', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'extract_note': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_cached_notes': return Promise.resolve([]);
        case 'get_ai_limits': return { automatic_enrichment: true };
        case 'suggest_note_folder': return suggestion.promise;
        case 'suggest_wiki_links': return Promise.resolve(AUTO_LINK);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        default: return Promise.resolve([]);
      }
    });
    await runExtract();
    notes.set([]);
    suggestion.resolve({ kind: 'no_suggestion' });
    await settle();

    expect(commands()).toContain('suggest_wiki_links');
    expect(commands()).not.toContain('save_note');
  });

  it('a failing folder suggestion surfaces nothing and auto-link still runs', async () => {
    await runExtract();
    suggestion.reject('transport error: timeout');
    await settle();
    expect(commands()).toContain('suggest_wiki_links');
    // Pre-flight ruling 1: open=false after a successful extract means
    // `.error` is null regardless of the suggestion outcome — this
    // assertion alone would pass even if runFolderSuggestion never caught
    // its rejection. The console.warn assertion below is what actually
    // proves the failure was logged by runFolderSuggestion rather than
    // silently swallowed or left unhandled.
    expect(host.querySelector('.error')).toBeNull();
    expect(console.warn).toHaveBeenCalledWith(
      expect.stringContaining('suggest_note_folder failed'),
      expect.anything(),
    );
  });
});
