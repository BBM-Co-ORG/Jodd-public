// @vitest-environment jsdom
//
// URL ingest in the Extract modal (spec 2026-09-15-url-ingest-design.md).
// Mounted, not unit-tested: the analysis and the selection must land in the
// same reactive turn (gotcha #28), which only a mounted component shows.
// `Channel` is new to this project — the core mock provides one that records
// every instance so a test can push progress through it.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import type { IngestAnalysis, IngestSource, LinkCandidate, LinkDiscovery, Note } from '../types';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';
import { AI_CONSENT_NEEDED as AI_CONSENT } from '../aiConsent';
import { settingsRoute } from '../stores/ui';
import { get } from 'svelte/store';

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
  return { url: `https://youtu.be/${id}`, kind: 'youtube', supported: true, reason: null, duplicate_owner: null, may_transcribe: false, comments_closed: false, ...extra };
}

function analysis(sources: IngestSource[], mostly_urls: boolean, stt_host: string | null = null, stt_insecure = false): IngestAnalysis {
  return { sources, mostly_urls, context_text: 'my reason', context_chars: 9, stt_host, stt_insecure };
}

/** A social link: its audio may go to speech-to-text. */
function social(kind: IngestSource['kind'], n: number): IngestSource {
  return { url: `https://example.com/${kind}/${n}`, kind, supported: true, reason: null, duplicate_owner: null, may_transcribe: true, comments_closed: true };
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
  let discover: ReturnType<typeof deferred<unknown>>;
  let nextPasted: LinkDiscovery;
  let linkSettings: unknown;
  let pick: ReturnType<typeof deferred<unknown>>;

  beforeEach(async () => {
    vi.useFakeTimers();
    channels.length = 0;
    ingest = deferred();
    discover = deferred();
    nextPasted = { url: '', title: null, candidates: [], not_shown: 0, skipped: [] };
    linkSettings = { auto_follow: [], auto_follow_max: 3, auto_follow_min_score: 40 };
    pick = deferred();
    nextAnalysis = analysis([], false);
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'analyze_ingest_sources': return Promise.resolve(nextAnalysis);
        case 'ingest_sources': return ingest.promise;
        case 'discover_link_candidates': return discover.promise;
        case 'pasted_link_candidates': return Promise.resolve(nextPasted);
        case 'get_link_settings': return Promise.resolve(linkSettings);
        case 'pick_link_candidates': return pick.promise;
        case 'cancel_extraction': return Promise.resolve(true);
        case 'search_notes': return Promise.resolve([SOURCE_NOTE]);
        default: return Promise.resolve([]);
      }
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    settingsRoute.set(null);
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
    expect(primary().textContent).toContain('Make note from 2 links');
  });

  it('an article with inline links is not pre-checked', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE')], false);
    await type('A long article that happens to mention https://youtu.be/jXtnhyro-QE once.');
    expect(host.querySelector('.ingest-sources')).not.toBeNull();
    expect(checked()).toHaveLength(0);
    expect(primary().textContent).toContain('Make note');
  });

  it('more than 8 links: 8 checked, with a notice', async () => {
    nextAnalysis = analysis(Array.from({ length: 10 }, (_, i) => yt(`id${String(i).padStart(9, '0')}`)), true);
    await type('ten links');
    expect(checked()).toHaveLength(8);
    expect(host.querySelector('.ingest-sources')!.textContent).toContain('At most 8 links');
  });

  it('labels each platform by name', async () => {
    const src = (kind: IngestSource['kind'], n: number): IngestSource =>
      ({ url: `https://example.com/${n}`, kind, supported: true, reason: null, duplicate_owner: null, may_transcribe: true, comments_closed: true });
    nextAnalysis = analysis([src('tiktok', 1), src('instagram', 2), src('facebook', 3), src('x', 4)], true);
    await type('four links');
    const labels = Array.from(host.querySelectorAll('.source-kind')).map((e) => e.textContent);
    expect(labels).toEqual(['TikTok', 'Instagram', 'Facebook', 'X']);
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
    expect(call).toMatchObject({ accountId: ACCT, urls: ['https://youtu.be/jXtnhyro-QE', 'https://youtu.be/ve4f7oz-UPs'], contextText: 'my reason', mode: { kind: 'key_points' }, source: { kind: 'pasted' } });
    expect(call.onProgress).toBe(channels[0]);
  });

  it('Stop invokes cancel_extraction with the same request_id', async () => {
    nextAnalysis = analysis([yt('jXtnhyro-QE')], true);
    await type('https://youtu.be/jXtnhyro-QE');
    primary().click();
    await settle();
    const requestId = (invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as { requestId: string }).requestId;
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Stop')!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId });
  });

  it('existing-note mode sends the source note uuid in exclude_uuids', async () => {
    Array.from(host.querySelectorAll('button')).find((b) => b.textContent === 'Use an existing note instead')!.click();
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
  describe('where the audio goes (spec D5, §5.5)', () => {
    const line = () => host.querySelector('.audio-line')?.textContent?.replace(/\s+/g, ' ').trim();
    const own = () => Array.from(host.querySelectorAll('.ingest-sources label.own-transcription'));
    const ownBox = () => host.querySelector<HTMLInputElement>('.own-transcription input')!;
    const ingestArgs = () => invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as Record<string, unknown>;

    it('the_audio_destination_line_names_the_host', async () => {
      nextAnalysis = analysis([social('tiktok', 1)], true, 'api.groq.com');
      await type('one link');
      expect(line()).toBe('Audio from these clips will be sent to api.groq.com.');
    });

    it('the host is shown exactly as the backend gives it, port included', async () => {
      nextAnalysis = analysis([social('instagram', 1)], true, 'ai.example.test:8443');
      await type('one link');
      expect(line()).toBe('Audio from these clips will be sent to ai.example.test:8443.');
    });

    it('an http address is named as not encrypted', async () => {
      nextAnalysis = analysis([social('tiktok', 1)], true, '192.168.1.5:8000', true);
      await type('one link');
      expect(line()).toBe('Audio from these clips will be sent to 192.168.1.5:8000 (not encrypted).');
    });

    it('the_unconfigured_line_links_to_settings', async () => {
      nextAnalysis = analysis([social('facebook', 1)], true, null);
      await type('one link');
      expect(line()).toBe('Spoken content needs speech-to-text — set it up in Settings. Captions will be used instead.');
      expect(host.querySelector('.audio-line a, .audio-line button')!.textContent).toBe('set it up in Settings');
    });

    it('the settings link opens Settings over the modal and keeps what was pasted', async () => {
      nextAnalysis = analysis([social('facebook', 1)], true, null);
      await type('https://example.com/facebook/1');
      settingsRoute.set(null);
      host.querySelector<HTMLButtonElement>('.audio-line button')!.click();
      await settle();
      // The link is about speech-to-text, so it opens that page.
      expect(get(settingsRoute)).toBe('speech');
      expect(host.querySelector('textarea')!.value).toBe('https://example.com/facebook/1');
      expect(checked()).toHaveLength(1);
      expect(line()).toContain('set it up in Settings');
    });

    it('Settings stacks on top: the modal stays open (not suspended) underneath', async () => {
      // Settings is a Modal opened later, so the top layer puts it above this
      // one; nothing here is closed or hidden while it is open (R16: the old
      // `suspended` bridge is gone). Stacking and Back: settingsOverExtract.test.ts.
      const esc = () => new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });
      const dialog = () => host.querySelector('dialog')!;
      nextAnalysis = analysis([social('facebook', 1)], true, null);
      await type('https://example.com/facebook/1');
      settingsRoute.set('speech');
      await settle();
      expect(dialog().open).toBe(true);
      expect(host.querySelector('textarea')!.value).toBe('https://example.com/facebook/1');
      settingsRoute.set(null);
      await settle();
      expect(dialog().open).toBe(true);
      dialog().dispatchEvent(esc());
      await settle();
      // The pasted link is unsaved input, so the close asks first.
      Array.from(host.querySelectorAll('button')).find((b) => b.textContent?.trim() === 'Discard')!.click();
      await settle();
      expect(host.querySelector('textarea')).toBeNull();
    });

    it('closing Settings re-reads speech-to-text: the line flips to the host, the selection stays', async () => {
      nextAnalysis = analysis([social('tiktok', 1), social('x', 2)], true, null);
      await type('two links');
      host.querySelector<HTMLInputElement>('.source-row input')!.click(); // untick the first
      await settle();
      const before = checked().length;
      settingsRoute.set('speech');
      await settle();
      // The person configures speech-to-text in Settings, then closes it.
      nextAnalysis = analysis([social('tiktok', 1), social('x', 2)], true, 'api.groq.com');
      settingsRoute.set(null);
      await settle();
      expect(line()).toBe('Audio from these clips will be sent to api.groq.com.');
      expect(checked()).toHaveLength(before);
      expect(own()).toHaveLength(0); // the TikTok link is still unticked
    });

    it('says nothing about audio for a link with no platform transcript path (YouTube, a page)', async () => {
      nextAnalysis = analysis([yt('jXtnhyro-QE'), { ...yt('x'), kind: 'web', url: 'https://example.com/a' }], true, 'api.groq.com');
      await type('two links');
      expect(line()).toBeUndefined();
      expect(own()).toHaveLength(0);
    });

    it('the line follows the selection: unticking the only social link removes it', async () => {
      nextAnalysis = analysis([social('x', 1), yt('jXtnhyro-QE')], true, 'api.groq.com');
      await type('two links');
      expect(line()).toContain('api.groq.com');
      host.querySelector<HTMLInputElement>('.source-row input')!.click();
      await settle();
      expect(line()).toBeUndefined();
    });

    it('the_override_checkbox_only_shows_for_tiktok_with_stt', async () => {
      nextAnalysis = analysis([social('tiktok', 1)], true, 'api.groq.com');
      await type('tiktok with stt');
      expect(own()).toHaveLength(1);
      expect(own()[0].textContent).toContain('Transcribe the audio myself');

      nextAnalysis = analysis([social('tiktok', 1)], true, null);
      await type('tiktok without stt');
      expect(own()).toHaveLength(0);

      for (const kind of ['instagram', 'facebook', 'x'] as const) {
        nextAnalysis = analysis([social(kind, 1)], true, 'api.groq.com');
        await type(`${kind} with stt`);
        expect(own(), kind).toHaveLength(0);
      }

      // Offered only while the TikTok link is SELECTED.
      nextAnalysis = analysis([social('tiktok', 1), social('x', 2)], true, 'api.groq.com');
      await type('a mix');
      expect(own()).toHaveLength(1);
      host.querySelector<HTMLInputElement>('.source-row input')!.click();
      await settle();
      expect(own()).toHaveLength(0);
    });

    it('an unticked override leaves transcribeAudio out of the call', async () => {
      nextAnalysis = analysis([social('tiktok', 1)], true, 'api.groq.com');
      await type('tiktok');
      primary().click();
      await settle();
      expect('transcribeAudio' in ingestArgs()).toBe(false);
    });

    it('a ticked override is sent as transcribeAudio: true', async () => {
      nextAnalysis = analysis([social('tiktok', 1)], true, 'api.groq.com');
      await type('tiktok');
      ownBox().click();
      await settle();
      primary().click();
      await settle();
      expect(ingestArgs().transcribeAudio).toBe(true);
    });

    it('a ticked override does not outlive its box', async () => {
      nextAnalysis = analysis([social('tiktok', 1), social('x', 2)], true, 'api.groq.com');
      await type('a mix');
      ownBox().click();
      await settle();
      expect(ownBox().checked).toBe(true);
      // Untick the TikTok link: the box goes, and so must the "true" behind it.
      host.querySelector<HTMLInputElement>('.source-row input')!.click();
      await settle();
      primary().click();
      await settle();
      expect('transcribeAudio' in ingestArgs()).toBe(false);
    });
  });
  describe('Find links (spec 2026-10-28 §5)', () => {
    const POST = 'https://blog.example.com/post';
    const page = (): IngestSource => ({ url: POST, kind: 'web', supported: true, reason: null, duplicate_owner: null, may_transcribe: false, comments_closed: false });
    const cand = (url: string, preticked: boolean, kind: LinkCandidate['kind'] = 'web'): LinkCandidate => ({
      url, anchor: `anchor for ${url}`, context: '', region: 'body', kind, score: preticked ? 40 : 5,
      reasons: preticked ? ['cited in a sentence'] : [], preticked, duplicate_owner: null,
    });
    const found = (candidates: LinkCandidate[]): LinkDiscovery => ({
      url: POST, title: 'Post', candidates, not_shown: 0, skipped: [{ reason: 'a share button', count: 3 }],
    });
    const findButton = () => Array.from(host.querySelectorAll<HTMLButtonElement>('button.find-links'))[0];
    const candidateBoxes = () => Array.from(host.querySelectorAll<HTMLInputElement>('.candidate-row input[type=checkbox]'));
    const ingestArgs = () => invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as Record<string, unknown>;

    async function findWith(candidates: LinkCandidate[]) {
      nextAnalysis = analysis([page()], true);
      await type(POST);
      findButton().click();
      await settle();
      discover.resolve(found(candidates));
      await settle();
    }

    it('reads the source once, lists its links and ticks the best in the same turn', async () => {
      await findWith([cand('https://arxiv.org/abs/1', true), cand('https://github.com/a/b', true), cand('https://blog.example.com/x', false)]);
      const call = invoke.mock.calls.find(([c]) => c === 'discover_link_candidates')![1] as Record<string, unknown>;
      expect(call).toMatchObject({ accountId: ACCT, url: POST, excludeUrls: [POST] });
      expect(candidateBoxes().map((b) => b.checked)).toEqual([true, true, false]);
      expect(host.querySelector('.skipped-line')!.textContent).toContain('Skipped 3 links: a share button (3)');
      expect(primary().textContent).toContain('Make note from 3 links');
    });

    it('sends the ticked links with the source they came from', async () => {
      await findWith([cand('https://arxiv.org/abs/1', true), cand('https://blog.example.com/x', false)]);
      candidateBoxes()[1].click();
      await settle();
      primary().click();
      await settle();
      expect(ingestArgs()).toMatchObject({
        urls: [POST],
        followed: [{ url: 'https://arxiv.org/abs/1', from_url: POST }, { url: 'https://blog.example.com/x', from_url: POST }],
      });
    });

    it('unticking the source keeps its links out of the ingest', async () => {
      nextAnalysis = analysis([page(), yt('jXtnhyro-QE')], true);
      await type(`${POST} https://youtu.be/jXtnhyro-QE`);
      findButton().click();
      await settle();
      discover.resolve(found([cand('https://arxiv.org/abs/1', true)]));
      await settle();
      expect(primary().textContent).toContain('Make note from 3 links');
      // Untick the page itself: its followed link must not be sent.
      host.querySelector<HTMLInputElement>('.source-row input')!.click();
      await settle();
      expect(primary().textContent).toContain('Make note from 1 link');
      primary().click();
      await settle();
      expect(ingestArgs().urls).toEqual(['https://youtu.be/jXtnhyro-QE']);
      expect('followed' in ingestArgs()).toBe(false);
    });

    it('new text drops the list and cancels a read still in flight', async () => {
      nextAnalysis = analysis([page()], true);
      await type(POST);
      findButton().click();
      await settle();
      const requestId = (invoke.mock.calls.find(([c]) => c === 'discover_link_candidates')![1] as { requestId: string }).requestId;
      expect(host.querySelector('.finding')).not.toBeNull();
      await type(`${POST} and more words`);
      expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId });
      // The late answer belongs to text no longer in the box: ignored.
      discover.resolve(found([cand('https://arxiv.org/abs/1', true)]));
      await settle();
      expect(candidateBoxes()).toHaveLength(0);
    });

    it('a followed clip says where its audio would go', async () => {
      await findWith([cand('https://www.tiktok.com/@a/video/1', true, 'tiktok')]);
      expect(host.querySelector('[data-testid="audio-unconfigured"]')).not.toBeNull();
    });

    it('a failed read says why and offers to try again', async () => {
      nextAnalysis = analysis([page()], true);
      await type(POST);
      findButton().click();
      await settle();
      discover.resolve(Promise.reject('HTTP 403'));
      await settle();
      expect(host.querySelector('.ingest-sources')!.textContent).toContain('Could not read its links: HTTP 403');
    });
  });

  describe('Pasted comments (spec 2026-10-28 D8)', () => {
    const REEL = 'https://www.facebook.com/reel/123';
    const reel = (): IngestSource => ({ url: REEL, kind: 'facebook', supported: true, reason: null, duplicate_owner: null, may_transcribe: true, comments_closed: true });
    const ingestArgs = () => invoke.mock.calls.find(([c]) => c === 'ingest_sources')![1] as Record<string, unknown>;
    const pasteBox = () => host.querySelector<HTMLTextAreaElement>('textarea.paste-box');

    async function paste(text: string) {
      const box = pasteBox()!;
      box.value = text;
      box.dispatchEvent(new Event('input', { bubbles: true }));
      flushSync();
      await vi.advanceTimersByTimeAsync(400);
      await settle();
    }

    it('a closed platform offers the box; an open one does not', async () => {
      nextAnalysis = analysis([reel()], true);
      await type(REEL);
      expect(host.querySelector('.paste-comments')!.textContent).toContain('Facebook shows comments only to signed-in users');
      nextAnalysis = analysis([yt('jXtnhyro-QE')], true);
      await type('https://youtu.be/jXtnhyro-QE');
      expect(pasteBox()).toBeNull();
    });

    it('pasted links arrive ticked and both the paste and the links are sent', async () => {
      nextAnalysis = analysis([reel()], true);
      await type(REEL);
      nextPasted = {
        url: REEL, title: null, not_shown: 0, skipped: [],
        candidates: [{ url: 'https://youtu.be/full', anchor: 'https://youtu.be/full', context: '', region: 'pasted', kind: 'youtube', score: 50, reasons: ['you pasted it'], preticked: true, duplicate_owner: null }],
      };
      await paste('Full video: https://youtu.be/full');
      const call = invoke.mock.calls.find(([c]) => c === 'pasted_link_candidates')![1] as Record<string, unknown>;
      expect(call).toMatchObject({ accountId: ACCT, fromUrl: REEL, text: 'Full video: https://youtu.be/full' });
      expect(Array.from(host.querySelectorAll<HTMLInputElement>('.paste-comments .candidate-row input')).map((b) => b.checked)).toEqual([true]);
      primary().click();
      await settle();
      expect(ingestArgs()).toMatchObject({
        urls: [REEL],
        pasted: [{ url: REEL, text: 'Full video: https://youtu.be/full' }],
        followed: [{ url: 'https://youtu.be/full', from_url: REEL }],
      });
    });

    it('clearing the box unticks what it added', async () => {
      nextAnalysis = analysis([reel()], true);
      await type(REEL);
      nextPasted = {
        url: REEL, title: null, not_shown: 0, skipped: [],
        candidates: [{ url: 'https://youtu.be/full', anchor: 'x', context: '', region: 'pasted', kind: 'youtube', score: 50, reasons: [], preticked: true, duplicate_owner: null }],
      };
      await paste('https://youtu.be/full');
      expect(primary().textContent).toContain('Make note from 2 links');
      await paste('');
      expect(primary().textContent).toContain('Make note from 1 link');
    });
  });

  describe('Phase 3: Let AI choose and auto-follow (spec 2026-10-28 §5.4, D2)', () => {
    const POST = 'https://blog.example.com/post';
    const page = (): IngestSource => ({ url: POST, kind: 'web', supported: true, reason: null, duplicate_owner: null, may_transcribe: false, comments_closed: false });
    const cand = (url: string, score: number, preticked = false): LinkCandidate => ({
      url, anchor: `anchor ${url}`, context: 'sentence', region: 'body', kind: 'web', score, reasons: ['why'], preticked, duplicate_owner: null,
    });
    const found = (candidates: LinkCandidate[]): LinkDiscovery => ({ url: POST, title: 'Post', candidates, not_shown: 0, skipped: [] });
    const boxes = () => Array.from(host.querySelectorAll<HTMLInputElement>('.candidate-row input[type=checkbox]')).map((b) => b.checked);
    const aiButton = () => Array.from(host.querySelectorAll<HTMLButtonElement>('button.ai-pick'))[0];
    const discoverCalls = () => invoke.mock.calls.filter(([c]) => c === 'discover_link_candidates').length;

    async function listed(candidates: LinkCandidate[]) {
      nextAnalysis = analysis([page()], true);
      await type(POST);
      host.querySelector<HTMLButtonElement>('button.find-links')!.click();
      await settle();
      discover.resolve(found(candidates));
      await settle();
    }

    it("re-ticks the list with the AI's picks and shows why", async () => {
      await listed([cand('https://a.example/1', 60, true), cand('https://a.example/2', 20), cand('https://a.example/3', 10)]);
      expect(boxes()).toEqual([true, false, false]);
      aiButton().click();
      await settle();
      expect(aiButton().textContent).toContain('AI is choosing');
      const call = invoke.mock.calls.find(([c]) => c === 'pick_link_candidates')![1] as Record<string, unknown>;
      expect(call).toMatchObject({ accountId: ACCT, pageUrl: POST, pageTitle: 'Post', contextText: 'my reason', source: { kind: 'pasted' } });
      expect((call.candidates as unknown[]).length).toBe(3);
      pick.resolve([{ url: 'https://a.example/3', why: 'the original study' }]);
      await settle();
      expect(boxes()).toEqual([false, false, true]);
      expect(host.querySelector('.ai-why')!.textContent).toBe('AI: the original study');
    });

    it('a refused AI permission offers Allow and leaves the ticks alone', async () => {
      await listed([cand('https://a.example/1', 60, true), cand('https://a.example/2', 20)]);
      aiButton().click();
      await settle();
      pick.resolve(Promise.reject(`${AI_CONSENT} for this account`));
      await settle();
      expect(boxes()).toEqual([true, false]);
      expect(Array.from(host.querySelectorAll('button')).some((b) => /allow/i.test(b.textContent ?? ''))).toBe(true);
    });

    it('with auto-follow on, a selected source is read and its best links ticked without a tap', async () => {
      linkSettings = { auto_follow: [ACCT], auto_follow_max: 3, auto_follow_min_score: 40 };
      unmount(app);
      app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
      await settle();
      nextAnalysis = analysis([page()], true);
      await type(POST);
      expect(discoverCalls()).toBe(1);
      expect(host.querySelector('.auto-follow-line')).not.toBeNull();
      discover.resolve(found([cand('https://a.example/1', 75), cand('https://a.example/2', 30), cand('https://a.example/3', 50)]));
      await settle();
      expect(boxes()).toEqual([true, false, true]);
      // Editing the text reads the same page from the cache, not the network.
      await type(`${POST} more words`);
      expect(discoverCalls()).toBe(1);
      expect(boxes()).toEqual([true, false, true]);
    });
  });

  // ── The private reader (spec 2026-10-28 §7.3, P4) ──────────────────────
  describe('pages that need JavaScript', () => {
    const APP = 'https://app.example.com/post';
    const page = (): IngestSource => ({ url: APP, kind: 'web', supported: true, reason: null, duplicate_owner: null, may_transcribe: false, comments_closed: false });
    const FAILED = `Nothing could be read from the links:\n• ${APP} — failed: no readable text (the page may need JavaScript)`;
    let render: ReturnType<typeof deferred<unknown>>;
    let platform: string;
    let ingestResult: () => Promise<unknown>;
    const readerButton = () => host.querySelector<HTMLButtonElement>('[data-testid=private-reader] button.find-links');
    const ingestCalls = () => invoke.mock.calls.filter(([c]) => c === 'ingest_sources').map(([, a]) => a as Record<string, unknown>);

    beforeEach(() => {
      render = deferred();
      platform = 'macos';
      ingestResult = () => Promise.reject(FAILED);
      const base = invoke.getMockImplementation()!;
      invoke.mockImplementation((cmd: string, args?: unknown) => {
        if (cmd === 'render_source_for_ingest') return render.promise;
        if (cmd === 'platform_name') return Promise.resolve(platform);
        if (cmd === 'ingest_sources') return ingestResult();
        return base(cmd, args);
      });
    });

    async function failOnce() {
      nextAnalysis = analysis([page()], true);
      await type(APP);
      primary().click();
      await settle();
    }

    it('no offer until an ingest says the page needs JavaScript', async () => {
      nextAnalysis = analysis([page()], true);
      await type(APP);
      expect(host.querySelector('[data-testid=private-reader]')).toBeNull();
      await failOnce();
      expect(readerButton()!.textContent).toBe('Open in a private reader');
    });

    it('renders on request, says what it read, and the next note reads from it', async () => {
      await failOnce();
      readerButton()!.click();
      await settle();
      const call = invoke.mock.calls.find(([c]) => c === 'render_source_for_ingest')![1] as Record<string, unknown>;
      expect(call).toMatchObject({ url: APP });
      expect(host.querySelector('[data-testid=private-reader]')!.textContent).toContain('Reading it in a private reader');
      render.resolve({ url: APP, title: 'The app', words: 420 });
      await settle();
      expect(host.querySelector('.rendered-line')!.textContent).toContain('Read “The app” — 420 words');
      // The first attempt sent nothing rendered; the second names the page.
      ingestResult = () => new Promise(() => {});
      primary().click();
      await settle();
      expect(ingestCalls()[0]).not.toHaveProperty('rendered');
      expect(ingestCalls()[1]).toMatchObject({ urls: [APP], rendered: [APP] });
    });

    it('new text drops the offer and cancels a read in flight', async () => {
      await failOnce();
      readerButton()!.click();
      await settle();
      const reqId = (invoke.mock.calls.find(([c]) => c === 'render_source_for_ingest')![1] as { requestId: string }).requestId;
      await type(`${APP} more`);
      expect(host.querySelector('[data-testid=private-reader]')).toBeNull();
      expect(invoke.mock.calls.some(([c, a]) => c === 'cancel_extraction' && (a as { requestId: string }).requestId === reqId)).toBe(true);
      render.resolve({ url: APP, title: 'late', words: 1 });
      await settle();
      expect(host.querySelector('.rendered-line')).toBeNull();
    });

    it('a failed render says why and offers to try again', async () => {
      let reject!: (e: unknown) => void;
      render = { promise: new Promise((_, rej) => { reject = rej; }), resolve: () => {} };
      await failOnce();
      readerButton()!.click();
      await settle();
      reject('Even rendered, the page shows no readable text — it may need a sign-in.');
      await settle();
      const box = host.querySelector('[data-testid=private-reader]')!;
      expect(box.textContent).toContain('may need a sign-in');
      expect(Array.from(box.querySelectorAll('button')).some((b) => b.textContent === 'Try again')).toBe(true);
    });

    it('Android shows the action disabled, desktop only for now', async () => {
      platform = 'android';
      unmount(app);
      app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
      await settle();
      await failOnce();
      const b = readerButton()!;
      expect(b.disabled).toBe(true);
      expect(host.querySelector('[data-testid=private-reader]')!.textContent).toContain('desktop only for now');
    });
  });
});
