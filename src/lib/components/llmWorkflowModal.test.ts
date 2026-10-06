// @vitest-environment jsdom
//
// Roadmap #2 (Summarize / Action Items / Expand Bullets): the modal gets a
// workflow-kind selector alongside its existing New-note/Append and
// Paste/Existing-note toggles. Selecting a non-default workflow branches the
// submit call from `extract_note` to `run_llm_workflow` (new-note mode); the
// default selection must keep calling `extract_note`, unchanged, so existing
// users see zero behavior change (mirrors extractModalFiling.test.ts's
// mount/invoke-mock pattern).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import type { Note } from '../types';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ACCT = 'gmail:a@b.com';
const CREATED: Note = {
  uuid: 'new-uuid', id: '', account_id: ACCT, title: 'Note', body_html: '<p>x</p>',
  date: '2026-09-17T00:00:00Z', label: 'Notes/Inbox',
} as Note;
const TARGET_NOTE: Note = {
  uuid: 'target-uuid', id: 'gmail-target-1', account_id: ACCT, title: 'Target note',
  body_html: '<p>existing</p>', date: '2026-09-16T00:00:00Z', label: 'Notes/Trading',
} as Note;

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('LessonExtractModal workflow selector', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(async () => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'check_duplicate_citations': return Promise.resolve([]);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'extract_note': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'run_llm_workflow': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_note_tags': return Promise.resolve([]);
        case 'suggest_note_folder': return Promise.resolve({ kind: 'no_suggestion' });
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
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  function pasteSourceText() {
    const textarea = host.querySelector('textarea')!;
    textarea.value = 'some source text';
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
  }

  const commands = () => invoke.mock.calls.map((c) => c[0] as string);
  const primary = () => host.querySelector<HTMLButtonElement>('button.primary')!;

  it('selecting Summarize and submitting calls run_llm_workflow with workflow: "summarize", not extract_note', async () => {
    // At this point only the selector's own option carries this exact text —
    // the primary submit button still reads "Extract" (default workflow).
    const summarizeOption = Array.from(host.querySelectorAll('button'))
      .find((b) => b.textContent?.trim() === 'Summarize')!;
    summarizeOption.click();
    flushSync();

    pasteSourceText();
    primary().click();
    await settle();

    expect(commands()).not.toContain('extract_note');
    expect(commands()).toContain('run_llm_workflow');
    const call = invoke.mock.calls.find((c) => c[0] === 'run_llm_workflow')!;
    expect(call[1]).toMatchObject({
      accountId: ACCT,
      workflow: 'summarize',
      sourceText: 'some source text',
    });
  });

  it('the default selection (no interaction) still calls extract_note — zero behavior change', async () => {
    pasteSourceText();
    primary().click();
    await settle();

    expect(commands()).toContain('extract_note');
    expect(commands()).not.toContain('run_llm_workflow');
    const call = invoke.mock.calls.find((c) => c[0] === 'extract_note')!;
    expect(call[1]).toMatchObject({
      accountId: ACCT,
      sourceText: 'some source text',
    });
  });

  // Final whole-branch review, Important finding 1: the append-mode branch
  // (destination: 'existing' combined with a non-default workflow) reaches
  // append_llm_workflow_note and was exercised by nothing but ipc_contract,
  // which only checks the command NAME, not the argument shapes. Target-note
  // picking mirrors extractModalIngest.test.ts's "existing-note mode" test
  // (search_notes → debounced results → click a `.target-results` button),
  // applied here to the Target-note picker (destination: 'existing') rather
  // than the Source-note picker (sourceMode: 'existing').
  it('append mode + a non-default workflow calls append_llm_workflow_note, not append_extract_note', async () => {
    vi.useFakeTimers();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'check_duplicate_citations': return Promise.resolve([]);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'search_notes': return Promise.resolve([TARGET_NOTE]);
        case 'append_extract_note': return Promise.resolve(TARGET_NOTE.uuid);
        case 'append_llm_workflow_note': return Promise.resolve(TARGET_NOTE.uuid);
        case 'list_cached_notes_in_folder': return Promise.resolve([TARGET_NOTE]);
        case 'list_note_tags': return Promise.resolve([]);
        case 'suggest_wiki_links': return Promise.resolve({ auto_links: [], proposed_appends: [] });
        default: return Promise.resolve([]);
      }
    });

    const summarizeOption = Array.from(host.querySelectorAll('button'))
      .find((b) => b.textContent?.trim() === 'Summarize')!;
    summarizeOption.click();
    flushSync();

    const appendToggle = Array.from(host.querySelectorAll('button'))
      .find((b) => b.textContent?.trim() === 'Append to existing note')!;
    appendToggle.click();
    flushSync();

    const targetInput = host.querySelector<HTMLInputElement>('input[placeholder^="Search notes"]')!;
    targetInput.value = 'Target';
    targetInput.dispatchEvent(new Event('input', { bubbles: true }));
    await vi.advanceTimersByTimeAsync(150);
    await settle();

    Array.from(host.querySelectorAll('.target-results button'))
      .find((b) => b.textContent?.includes(TARGET_NOTE.title))!
      .dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();

    pasteSourceText();
    primary().click();
    await settle();

    expect(commands()).not.toContain('append_extract_note');
    expect(commands()).toContain('append_llm_workflow_note');
    const call = invoke.mock.calls.find((c) => c[0] === 'append_llm_workflow_note')!;
    expect(call[1]).toMatchObject({
      accountId: ACCT,
      workflow: 'summarize',
      targetUuid: TARGET_NOTE.uuid,
      sourceText: 'some source text',
    });
  });

  // Final whole-branch review, Important finding 2, superseded by spec
  // 2026-09-27 (task 5): in URL-ingest mode the workflow selector stays live
  // except for Action items (paste-only) — Summarize/Expand
  // bullets/Transcript all carry through to `ingest_sources`'s `mode`.
  // Action items falls back to Key points for the duration (gotcha #28: a
  // $derived, not an $effect) and the user's choice returns when the links
  // are removed.
  const YT_URL = 'https://youtu.be/jXtnhyro-QE';

  async function enterIngestMode() {
    vi.useFakeTimers();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'analyze_ingest_sources': return Promise.resolve({
          sources: [{ url: YT_URL, kind: 'youtube', supported: true, reason: null, duplicate_owner: null }],
          mostly_urls: true, context_text: '', context_chars: 0,
        });
        case 'ingest_sources': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'run_llm_workflow': return Promise.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_note_tags': return Promise.resolve([]);
        case 'suggest_note_folder': return Promise.resolve({ kind: 'no_suggestion' });
        case 'suggest_wiki_links': return Promise.resolve({ auto_links: [], proposed_appends: [] });
        default: return Promise.resolve([]);
      }
    });
    const textarea = host.querySelector('textarea')!;
    textarea.value = YT_URL;
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
  }

  const option = (label: string) =>
    Array.from(host.querySelectorAll<HTMLButtonElement>('[aria-label="Workflow"] button')).find((b) => b.textContent?.trim() === label)!;
  const ingestCall = () => invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as Record<string, unknown>;

  it('with a link selected, Summarize stays enabled and ingest receives its mode', async () => {
    option('Summarize').click();
    flushSync();
    await enterIngestMode();
    expect(option('Summarize').disabled).toBe(false);
    expect(option('Action items').disabled).toBe(true);
    primary().click();
    await settle();
    expect(ingestCall().mode).toEqual({ kind: 'workflow', workflow: 'summarize' });
    expect(commands()).not.toContain('run_llm_workflow');
  });

  it('Action items selected before a link falls back to Key points in the same turn, and back after', async () => {
    option('Action items').click();
    flushSync();
    await enterIngestMode();
    expect(host.querySelector('h2')!.textContent).toBe('Extract');
    expect(option('Key points').classList.contains('active')).toBe(true);
    expect(host.querySelector('.ingest-note')!.textContent).toContain('Action items work on pasted text only');
    primary().click();
    await settle();
    expect(ingestCall().mode).toEqual({ kind: 'key_points' });
  });

  it('Transcript with a link can turn Clean off; without a link Clean is forced on', async () => {
    option('Transcript').click();
    flushSync();
    pasteSourceText();
    const clean = () => host.querySelector<HTMLInputElement>('input.clean-transcript')!;
    expect(clean().checked).toBe(true);
    expect(clean().disabled).toBe(true);
    await enterIngestMode();
    expect(clean().disabled).toBe(false);
    clean().click();
    flushSync();
    primary().click();
    await settle();
    expect(ingestCall().mode).toEqual({ kind: 'transcript', clean: false });
  });

  it('Transcript on pasted text runs run_llm_workflow with clean_transcript', async () => {
    option('Transcript').click();
    flushSync();
    pasteSourceText();
    primary().click();
    await settle();
    const call = invoke.mock.calls.find(([c]) => c === 'run_llm_workflow')![1] as Record<string, unknown>;
    expect(call.workflow).toBe('clean_transcript');
  });
});
