// @vitest-environment jsdom
//
// Share to Jodd → "Summarize with AI" (spec 2026-10-06-share-to-jodd §3.5).
// The capture sheet hands the shared content to this modal through
// `extractPrefill`; the modal consumes it once, and retires the capture only
// when a note made in THAT opening exists.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import { extractPrefill, pendingCaptures, type PendingCapture } from '../capture';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ACCT = 'gmail:a@b.com';
const LINK = 'https://a.example/post';

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('LessonExtractModal — a capture handed over by the share sheet', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'analyze_ingest_sources':
          return Promise.resolve({
            sources: [{ url: LINK, kind: 'web', supported: true, reason: null, duplicate_owner: null }],
            mostly_urls: true,
            context_text: '',
            context_chars: 0,
          });
        case 'ingest_sources':
          return Promise.resolve({ uuid: 'NEW', label: 'Notes/Inbox' });
        default:
          return Promise.resolve([]);
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
  });

  afterEach(() => {
    unmount(app);
    host.remove();
    vi.useRealTimers();
    vi.restoreAllMocks();
    extractPrefill.set(null);
  });

  async function open() {
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
  }

  it('fills the source and title once, and clears the hand-off', async () => {
    extractPrefill.set({ captureId: 'c1', text: LINK, title: 'A post' });
    await open();
    expect(host.querySelector('textarea')!.value).toBe(LINK);
    expect([...host.querySelectorAll('input')].some((i) => i.value === 'A post')).toBe(true);
    expect(get(extractPrefill)).toBeNull();
    // The link was analysed like a paste: it is offered for ingest.
    expect(host.querySelector('.ingest-sources')).not.toBeNull();
  });

  it('opens on the AI mode chosen in the sheet', async () => {
    extractPrefill.set({ captureId: 'c1', text: 'plain text, no link', title: '', workflow: 'summarize' });
    await open();
    const active = host.querySelector('[aria-label="Workflow"] button.active');
    expect(active?.textContent?.trim()).toBe('Summarize');
  });

  it('retires the capture once the note it became exists — from the sheet at once, then in Rust', async () => {
    const c1 = { id: 'c1' } as PendingCapture;
    pendingCaptures.set([c1, { id: 'c2' } as PendingCapture]);
    extractPrefill.set({ captureId: 'c1', text: LINK, title: '' });
    await open();
    host.querySelector<HTMLButtonElement>('button.primary')!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('complete_capture', { captureId: 'c1' });
    expect(get(pendingCaptures).map((c) => c.id)).not.toContain('c1');
  });

  it('an opening that did not come from a capture never retires one', async () => {
    await open();
    const textarea = host.querySelector('textarea')!;
    textarea.value = LINK;
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
    host.querySelector<HTMLButtonElement>('button.primary')!.click();
    await settle();
    expect(invoke.mock.calls.some(([c]) => c === 'ingest_sources')).toBe(true);
    expect(invoke.mock.calls.some(([c]) => c === 'complete_capture')).toBe(false);
  });
});
