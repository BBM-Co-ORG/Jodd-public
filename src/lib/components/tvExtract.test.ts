// @vitest-environment jsdom
//
// "What's on my TV" (spec 2026-11-02-tv-now-playing-ingest §4, §5.4): the AI
// menu item, and the Extract dialog's `kind: 'tv'` branch that asks the TV and
// fills Source. The answer only fills Source; nothing is ingested unasked.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));

import ExtractHost from './__fixtures__/ExtractHost.svelte';
import AiMenu from './AiMenu.svelte';
import { currentAccount, capabilitiesByAccount, selectedFolder } from '../stores/notes';
import { extractModalOpen, settingsRoute } from '../stores/ui';
import { extractPrefill } from '../capture';
import { tvs, type TvRow } from '../tvs';

const ACCT = 'gmail:a@b.com';
const URL1 = 'https://youtu.be/wnXoCNd57Ks?t=147';
const WATCHED = 'Watched on TV up to 2:27';

const tv = (o: Partial<TvRow> = {}): TvRow => ({
  tv_id: 'tv1', label: 'Living room', model: null, is_default: true, needs_repair: false, last_status: 'online', ...o,
});

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

const found = (o: Record<string, unknown> = {}) => ({
  kind: 'found', tv_id: 'tv1', label: 'Living room', url: URL1, watched_label: WATCHED, via_default: true, ...o,
});

describe("What's on my TV", () => {
  let host: HTMLElement;
  let app: Record<string, unknown> | null;
  let tvCalls: { requestId: string; tvId: string | null }[];
  let answer: ReturnType<typeof deferred<unknown>>;

  beforeEach(() => {
    answer = deferred();
    tvCalls = [];
    invoke.mockReset();
    invoke.mockImplementation((cmd: string, args: { requestId: string; tvId: string | null }) => {
      switch (cmd) {
        case 'tv_now_playing':
          tvCalls.push(args);
          return answer.promise;
        case 'analyze_ingest_sources':
          return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'cancel_extraction':
          return Promise.resolve(true);
        case 'list_tvs':
          return Promise.resolve([]);
        default:
          return Promise.resolve([]);
      }
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    currentAccount.set(ACCT);
    selectedFolder.set('Notes');
    extractPrefill.set(null);
    extractModalOpen.set(false);
    settingsRoute.set(null);
    tvs.set([tv()]);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = null;
  });

  afterEach(() => {
    if (app) unmount(app);
    host.remove();
    extractModalOpen.set(false);
    extractPrefill.set(null);
    vi.restoreAllMocks();
  });

  async function pressTv() {
    extractPrefill.set({ kind: 'tv' });
    extractModalOpen.set(true);
    if (!app) app = mount(ExtractHost, { target: host }) as Record<string, unknown>;
    await settle();
  }
  const source = () => host.querySelector('textarea')!.value;
  const button = (label: string) =>
    [...host.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as HTMLButtonElement | undefined;
  const text = () => host.textContent ?? '';

  describe('the AI menu item', () => {
    let menuHost: HTMLElement;
    let menu: Record<string, unknown>;
    beforeEach(() => {
      capabilitiesByAccount.set({ [ACCT]: { writes: { notes: true, relocate: true, folders: true, sidecars: false } } } as never);
      menuHost = document.createElement('div');
      document.body.appendChild(menuHost);
      menu = mount(AiMenu, { target: menuHost, props: { run: (a: () => void) => a() } }) as Record<string, unknown>;
    });
    afterEach(() => { unmount(menu); menuHost.remove(); });
    const item = () =>
      [...menuHost.querySelectorAll('button')].find((b) => b.textContent?.includes("What's on my TV")) as HTMLButtonElement | undefined;

    it('menu_item_opens_settings_without_tvs', async () => {
      tvs.set([]);
      flushSync();
      item()!.click();
      await settle();
      expect(get(settingsRoute)).toBe('links');
      expect(get(extractModalOpen)).toBe(false);
      expect(get(extractPrefill)).toBeNull();
    });

    it('menu_item_opens_extract_with_tvs', async () => {
      item()!.click();
      await settle();
      expect(get(extractPrefill)).toEqual({ kind: 'tv' });
      expect(get(extractModalOpen)).toBe(true);
      expect(get(settingsRoute)).toBeNull();
    });
  });

  it('extract_shows_asking_with_default_label', async () => {
    tvs.set([tv({ tv_id: 'b', label: 'Bedroom', is_default: false }), tv()]);
    await pressTv();
    expect(text()).toContain('Asking Living room…');
    expect(tvCalls).toHaveLength(1);
    expect(tvCalls[0].tvId).toBeNull();
    button('Cancel')!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId: tvCalls[0].requestId });
    // A user-pressed Cancel is silent and leaves the dialog open and empty.
    expect(text()).not.toContain('Asking');
    expect(text()).not.toContain('Stopped asking');
    expect(source()).toBe('');
    // ...even when the backend then answers `cancelled`.
    answer.reject({ code: 'cancelled' });
    await settle();
    expect(text()).not.toContain('Stopped asking');
    expect(source()).toBe('');
  });

  it('extract_fills_found', async () => {
    await pressTv();
    answer.resolve(found());
    await settle();
    expect(source()).toBe(`${URL1}\n${WATCHED}`);
    expect(text()).not.toContain('Asking');
    // Closing an untouched filled dialog is not "unsaved input".
    host.querySelector('dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await settle();
    expect(text()).not.toContain('Discard unsaved changes?');
    expect(host.querySelector('textarea')).toBeNull();
  });

  it('found_without_a_time_is_the_bare_link', async () => {
    await pressTv();
    answer.resolve(found({ url: 'https://youtu.be/wnXoCNd57Ks', watched_label: null }));
    await settle();
    expect(source()).toBe('https://youtu.be/wnXoCNd57Ks');
  });

  it('tv_fill_uses_paste_mode', async () => {
    await pressTv();
    answer.resolve(found());
    await settle();
    // Paste mode: the textarea (not the existing-note search) holds the link,
    // so aiSourceFor gives { kind: 'pasted' } (spec §5.4).
    expect(host.querySelector('textarea')).not.toBeNull();
    expect(text()).not.toContain('Source note');
  });

  it('typed_text_is_kept', async () => {
    await pressTv();
    const ta = host.querySelector('textarea')!;
    ta.value = 'my note';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    answer.resolve(found());
    await settle();
    expect(source()).toBe(`my note\n${URL1}\n${WATCHED}`);
  });

  it('a_stale_tv_answer_is_ignored', async () => {
    const first = answer;
    await pressTv();
    const firstId = tvCalls[0].requestId;
    // Close (Esc), then open again: a new request.
    host.querySelector('dialog')!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId: firstId });
    answer = deferred();
    await pressTv();
    expect(tvCalls).toHaveLength(2);
    expect(tvCalls[1].requestId).not.toBe(firstId);
    first.resolve(found({ url: 'https://youtu.be/STALE' }));
    await settle();
    expect(source()).toBe('');
    expect(text()).toContain('Asking Living room…');
    answer.resolve(found());
    await settle();
    expect(source()).toBe(`${URL1}\n${WATCHED}`);
  });

  it('a_late_tv_answer_never_changes_a_running_extract', async () => {
    const extracting = deferred<unknown>();
    invoke.mockImplementation((cmd: string, args: { requestId: string; tvId: string | null }) => {
      switch (cmd) {
        case 'tv_now_playing':
          tvCalls.push(args);
          return answer.promise;
        case 'extract_note':
          return extracting.promise;
        case 'cancel_extraction':
          return Promise.resolve(true);
        case 'analyze_ingest_sources':
          return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'check_duplicate_citations':
          return Promise.resolve([]);
        default:
          return Promise.resolve([]);
      }
    });
    await pressTv();
    const tvId = tvCalls[0].requestId;
    const ta = host.querySelector('textarea')!;
    ta.value = 'my note';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    // Make note while the TV is still being asked.
    button('Make note')!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId: tvId });
    const sent = invoke.mock.calls.filter((c) => c[0] === 'extract_note');
    expect(sent).toHaveLength(1);
    expect((sent[0][1] as { sourceText: string }).sourceText).toBe('my note');
    // The TV answers late: the running Extract and the form are untouched.
    answer.resolve(found());
    await settle();
    expect(source()).toBe('my note');
    expect(text()).not.toContain(URL1);
    expect((invoke.mock.calls.filter((c) => c[0] === 'extract_note')[0][1] as { sourceText: string }).sourceText).toBe('my note');
    extracting.reject('cancelled');
    await settle();
    expect(source()).toBe('my note');
  });

  it('a_picker_tap_while_extracting_changes_nothing', async () => {
    const extracting = deferred<unknown>();
    invoke.mockImplementation((cmd: string, args: { requestId: string; tvId: string | null }) => {
      switch (cmd) {
        case 'tv_now_playing':
          tvCalls.push(args);
          return answer.promise;
        case 'extract_note':
          return extracting.promise;
        case 'check_duplicate_citations':
          return Promise.resolve([]);
        case 'analyze_ingest_sources':
          return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        default:
          return Promise.resolve(cmd === 'cancel_extraction' ? true : []);
      }
    });
    tvs.set([tv(), tv({ tv_id: 'tv2', label: 'Bedroom', is_default: false })]);
    await pressTv();
    answer.resolve({
      kind: 'choose',
      candidates: [
        { tv_id: 'tv1', label: 'Living room', url: 'https://youtu.be/AAA?t=147', watched_label: 'Watched on Living room up to 2:27', playing: true },
        { tv_id: 'tv2', label: 'Bedroom', url: 'https://youtu.be/BBB', watched_label: null, playing: true },
      ],
    });
    await settle();
    const ta = host.querySelector('textarea')!;
    ta.value = 'my note';
    ta.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button('Make note')!.click();
    await settle();
    expect(invoke.mock.calls.filter((c) => c[0] === 'extract_note')).toHaveLength(1);
    // The picker is still on screen while the run is under way; a tap on it
    // must not rewrite the source the run was started with.
    [...host.querySelectorAll('button')].find((x) => x.textContent?.trim() === 'Bedroom')!.click();
    await settle();
    expect(source()).toBe('my note');
    extracting.reject('cancelled');
    await settle();
    expect(source()).toBe('my note');
  });

  it('choose_shows_picker', async () => {
    tvs.set([tv(), tv({ tv_id: 'tv2', label: 'Bedroom', is_default: false })]);
    await pressTv();
    answer.resolve({
      kind: 'choose',
      candidates: [
        { tv_id: 'tv1', label: 'Living room', url: 'https://youtu.be/AAA?t=147', watched_label: 'Watched on Living room up to 2:27', playing: true },
        { tv_id: 'tv2', label: 'Bedroom', url: 'https://youtu.be/BBB', watched_label: null, playing: true },
      ],
    });
    await settle();
    expect(source()).toBe('');
    const a = [...host.querySelectorAll('button')].find((b) => b.textContent?.includes('Living room'))!;
    const b = [...host.querySelectorAll('button')].find((x) => x.textContent?.trim() === 'Bedroom')!;
    expect(a.textContent).toContain('Watched on Living room up to 2:27');
    expect(b).toBeDefined();
    b.click();
    await settle();
    expect(source()).toBe('https://youtu.be/BBB');
    expect(tvCalls).toHaveLength(1);
    expect([...host.querySelectorAll('button')].some((x) => x.textContent?.trim() === 'Bedroom')).toBe(false);
  });

  it('choose_tap_adds_the_watched_line', async () => {
    tvs.set([tv(), tv({ tv_id: 'tv2', label: 'Bedroom', is_default: false })]);
    await pressTv();
    answer.resolve({
      kind: 'choose',
      candidates: [
        { tv_id: 'tv1', label: 'Living room', url: 'https://youtu.be/AAA?t=147', watched_label: 'Watched on Living room up to 2:27', playing: true },
        { tv_id: 'tv2', label: 'Bedroom', url: 'https://youtu.be/BBB', watched_label: null, playing: false },
      ],
    });
    await settle();
    [...host.querySelectorAll('button')].find((b) => b.textContent?.includes('Living room'))!.click();
    await settle();
    expect(source()).toBe('https://youtu.be/AAA?t=147\nWatched on Living room up to 2:27');
  });

  describe('errors', () => {
    it('nothing_playing with several TVs', async () => {
      tvs.set([tv(), tv({ tv_id: 'tv2', label: 'Bedroom', is_default: false })]);
      await pressTv();
      answer.reject({ code: 'nothing_playing' });
      await settle();
      expect(text()).toContain('Nothing is playing on your TVs.');
      expect(source()).toBe('');
      expect(text()).not.toContain('Asking');
    });

    it('nothing_playing with one TV names it', async () => {
      tvs.set([tv({ label: 'YouTube on TV' })]);
      await pressTv();
      answer.reject({ code: 'nothing_playing' });
      await settle();
      expect(text()).toContain('Nothing is playing on YouTube on TV.');
    });

    it('needs_repair names the TV', async () => {
      await pressTv();
      answer.reject({ code: 'needs_repair', label: 'Living room' });
      await settle();
      expect(text()).toContain('Living room needs re-pairing — open Settings.');
    });

    it('network uses the dialog error line and leaves Source for pasting', async () => {
      await pressTv();
      answer.reject({ code: 'network' });
      await settle();
      expect(host.querySelector('.error')!.textContent).toContain('Couldn’t reach your TV — check the connection.');
      expect(source()).toBe('');
    });

    it('no_tvs points at Settings', async () => {
      await pressTv();
      answer.reject({ code: 'no_tvs' });
      await settle();
      expect(text()).toContain('No TV is linked — link one in Settings → Links.');
    });

    it('a cancel the user did not cause says so, without blaming them', async () => {
      await pressTv();
      answer.reject({ code: 'cancelled' });
      await settle();
      expect(text()).toContain('Stopped asking your TV.');
      expect(text()).not.toMatch(/you cancelled/i);
    });
  });
});
