// @vitest-environment jsdom
//
// URL ingest in the Extract modal (spec 2026-09-15-url-ingest-design.md).
// Mounted, not unit-tested: the analysis and the selection must land in the
// same reactive turn (gotcha #28), which only a mounted component shows.
// `Channel` is new to this project — the core mock provides one that records
// every instance so a test can push progress through it.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import type { IngestAnalysis, IngestSource, Note } from '../types';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';

const { invoke, channels } = vi.hoisted(() => ({
  invoke: vi.fn(),
  channels: [] as { onmessage: (m: unknown) => void }[],
}));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
    constructor() {
      channels.push(this);
    }
  },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ACCT = 'gmail:a@b.com';

function yt(id: string, extra: Partial<IngestSource> = {}): IngestSource {
  return { url: `https://youtu.be/${id}`, kind: 'youtube', supported: true, reason: null, duplicate_owner: null, ...extra };
}

function analysis(sources: IngestSource[], mostly_urls: boolean): IngestAnalysis {
  return { sources, mostly_urls, context_text: 'my reason', context_chars: 9 };
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((res) => { resolve = res; });
  return { promise, resolve };
}

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('LessonExtractModal — sources from links', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let nextAnalysis: IngestAnalysis;
  let ingest: ReturnType<typeof deferred<unknown>>;

  beforeEach(async () => {
    vi.useFakeTimers();
    channels.length = 0;
    ingest = deferred();
    nextAnalysis = analysis([], false);
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'analyze_ingest_sources': return Promise.resolve(nextAnalysis);
        case 'ingest_sources': return ingest.promise;
        case 'cancel_extraction': return Promise.resolve(true);
        case 'search_notes': return Promise.resolve([SOURCE_NOTE]);
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

  const SOURCE_NOTE: Note = {
    uuid: 'SRC-UUID', id: 'm1', account_id: ACCT, title: 'Rust videos',
    body_html: '<div>Rust videos</div><div>https://youtu.be/jXtnhyro-QE</div>', date: '2026-09-15T00:00:00Z', label: 'Notes/Rust',
  } as Note;

  async function type(text: string) {
    const textarea = host.querySelector('textarea')!;
    textarea.value = text;
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
  }

  const checked = () => Array.from(host.querySelectorAll<HTMLInputElement>('.ingest-sources input[type=checkbox]')).filter((c) => c.checked);
  const primary = () => host.querySelector<HTMLButtonElement>('button.primary')!;

  it('a links-only analysis pre-checks the section and relabels the button', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE'), yt('ve4f7oz-UPs')], true);
    await type('https://youtu.be/jXtnhyro-QE https://youtu.be/ve4f7oz-UPs');
    expect(checked()).toHaveLength(2);
    expect(primary().textContent).toContain('Ingest 2 sources');
  });

  it('an article with inline links is not pre-checked', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE')], false);
    await type('A long article that happens to mention https://youtu.be/jXtnhyro-QE once.');
    expect(host.querySelector('.ingest-sources')).not.toBeNull();
    expect(checked()).toHaveLength(0);
    expect(primary().textContent).toContain('Extract');
  });

  it('more than 8 links: 8 checked, with a notice', async () => {
    nextAnalysis = analysis(Array.from({ length: 10 }, (_, i) => yt(`id${String(i).padStart(9, '0')}`)), true);
    await type('ten links');
    expect(checked()).toHaveLength(8);
    expect(host.querySelector('.ingest-sources')!.textContent).toContain('At most 8 links');
  });

  it('shows a duplicate badge naming the note that already cites the link', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE', { duplicate_owner: { uuid: 'OLD', title: 'Old note' } })], true);
    await type('https://youtu.be/jXtnhyro-QE');
    expect(host.querySelector('.dup-badge')!.textContent).toContain('Old note');
  });

  it('progress sent through the Channel updates the list', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE'), yt('ve4f7oz-UPs')], true);
    await type('two links');
    primary().click();
    await settle();
    expect(channels).toHaveLength(1);
    channels[0].onmessage({ stage: 'fetching', index: 1, total: 2, url_host: 'youtu.be' });
    channels[0].onmessage({ stage: 'summarizing', index: 1, total: 2, url_host: 'youtu.be' });
    await settle();
    const items = Array.from(host.querySelectorAll('.ingest-progress li')).map((li) => li.textContent);
    expect(items).toEqual(['Fetching 1 of 2 · youtu.be', 'Summarizing 1 of 2 · youtu.be']);
    const call = invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as Record<string, unknown>;
    expect(call).toMatchObject({ accountId: ACCT, urls: ['https://youtu.be/jXtnhyro-QE', 'https://youtu.be/ve4f7oz-UPs'], contextText: 'my reason' });
    expect(call.onProgress).toBe(channels[0]);
  });

  it('Cancel invokes cancel_extraction with the same request_id', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE')], true);
    await type('https://youtu.be/jXtnhyro-QE');
    primary().click();
    await settle();
    const requestId = (invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as { requestId: string }).requestId;
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.includes('Cancel'))!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId });
  });

  it('existing-note mode sends the source note uuid in exclude_uuids', async () => {
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent === 'Pick existing note')!.click();
    await settle();
    const search = host.querySelector<HTMLInputElement>('input[placeholder^="Search notes"]')!;
    search.value = 'Rust';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await vi.advanceTimersByTimeAsync(150);
    await settle();
    Array.from(host.querySelectorAll('.target-results button')).find((b) => b.textContent?.includes('Rust videos'))!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    flushSync();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
    const calls = invoke.mock.calls.filter(([c]) => c === 'analyze_ingest_sources').map(([, a]) => a as Record<string, unknown>);
    expect(calls.at(-1)).toMatchObject({ isHtml: true, excludeUuids: ['SRC-UUID'], text: SOURCE_NOTE.body_html });
  });
});
