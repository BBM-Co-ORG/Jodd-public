// @vitest-environment jsdom
//
// The Extract modal on Modal (Task 2.6): what closing does. A close attempt
// while a run is in flight cancels the run and keeps the modal (the
// pre-migration rule); the person's own unsaved input makes closing ask; a
// finished run closes INTO its destination with Modal.closeInto, so Android
// Back from the note lands beneath the modal instead of re-opening it.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));

import LessonExtractModal from './LessonExtractModal.svelte';
import { currentAccount, selectedFolder, selectedSmartFolder, selectedTags, notes, selectedNote, folderSuggestions } from '../stores/notes';
import { extractPrefill } from '../capture';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import {
  handlePopState,
  initPhoneNavHistory,
  depthFromHistoryState,
  overlayFromHistoryState,
  navigateToPane,
  backLabel,
} from '../stores/phoneNav';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;

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

const esc = () => new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true });

describe('LessonExtractModal on Modal', () => {
  let host: HTMLElement;
  let app: Record<string, unknown> | null;
  let run: ReturnType<typeof deferred<unknown>>;

  beforeEach(() => {
    run = deferred();
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'check_duplicate_citations': return Promise.resolve([]);
        case 'analyze_ingest_sources': return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
        case 'extract_note': return run.promise;
        case 'cancel_extraction': return Promise.resolve(true);
        case 'list_cached_notes_in_folder': return Promise.resolve([CREATED]);
        case 'list_note_tags': return Promise.resolve([]);
        case 'get_ai_limits': return { automatic_enrichment: false };
        default: return Promise.resolve([]);
      }
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    isAndroid.set(false);
    currentAccount.set(ACCT);
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
    notes.set([]);
    selectedNote.set(null);
    folderSuggestions.set({});
    extractPrefill.set(null);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = null;
  });

  afterEach(() => {
    if (app) unmount(app);
    host.remove();
    vi.restoreAllMocks();
  });

  async function open() {
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
  }
  const dialogs = () => [...host.querySelectorAll('dialog')];
  const modal = () => dialogs()[0];
  const guard = () => dialogs().find((d) => d.textContent?.includes('Discard unsaved changes?'));
  const button = (label: string, root: ParentNode = host) =>
    [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === label) as HTMLButtonElement | undefined;
  async function typeSource(text: string) {
    const textarea = host.querySelector('textarea')!;
    textarea.value = text;
    textarea.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
  }

  // Final review 2026-10-20: at md (480px) the five-option workflow toggle
  // wrapped its labels; main's Extract card was 600px. lg is 640px.
  it('is a large card, wide enough for the workflow toggle on one row', async () => {
    await open();
    expect(modal().dataset.size).toBe('lg');
  });

  it('with nothing typed, Esc closes at once', async () => {
    await open();
    modal().dispatchEvent(esc());
    await settle();
    expect(guard()).toBeUndefined();
    expect(host.querySelector('textarea')).toBeNull();
  });

  it('typed source makes closing ask; Keep editing keeps it, Discard closes', async () => {
    await open();
    await typeSource('my notes');
    modal().dispatchEvent(esc());
    await settle();
    expect(guard()).toBeTruthy();
    button('Keep editing', guard())!.click();
    await settle();
    expect(host.querySelector('textarea')!.value).toBe('my notes');
    modal().dispatchEvent(esc());
    await settle();
    button('Discard', guard())!.click();
    await settle();
    expect(host.querySelector('textarea')).toBeNull();
  });

  it('a typed title alone makes closing ask', async () => {
    await open();
    const title = host.querySelector<HTMLInputElement>('input[placeholder="Made from the result"]')!;
    title.value = 'Mine';
    title.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    modal().dispatchEvent(esc());
    await settle();
    expect(guard()).toBeTruthy();
  });

  it('what a capture handed over is not unsaved input: closing does not ask', async () => {
    extractPrefill.set({ captureId: 'c1', text: 'shared text', title: 'Shared', workflow: 'summarize' });
    await open();
    expect(host.querySelector('textarea')!.value).toBe('shared text');
    modal().dispatchEvent(esc());
    await settle();
    expect(guard()).toBeUndefined();
    expect(host.querySelector('textarea')).toBeNull();
  });

  it('Esc while a run is in flight cancels the run and keeps the modal and the source', async () => {
    await open();
    await typeSource('my notes');
    button('Make note')!.click();
    await settle();
    const requestId = (invoke.mock.calls.find(([c]) => c === 'extract_note')![1] as { requestId: string }).requestId;
    modal().dispatchEvent(esc());
    await settle();
    expect(invoke).toHaveBeenCalledWith('cancel_extraction', { requestId });
    expect(guard()).toBeUndefined();
    expect(modal().open).toBe(true);
    // The backend answers the cancel; the source is still there for a retry.
    run.reject('cancelled');
    await settle();
    expect(host.querySelector('textarea')!.value).toBe('my notes');
    expect(host.querySelector('.error')).toBeNull();
  });

  it('the primary is the one button whose label follows the state; Stop shows only during a run', async () => {
    await open();
    const primary = () => host.querySelector<HTMLButtonElement>('dialog button.primary')!;
    expect(host.querySelectorAll('dialog button.primary')).toHaveLength(1);
    expect(primary().textContent?.trim()).toBe('Make note');
    expect(primary().disabled).toBe(true);
    expect(button('Cancel')).toBeUndefined();
    expect(button('Stop')).toBeUndefined();
    await typeSource('my notes');
    button('Existing note…')!.click();
    await settle();
    expect(primary().textContent?.trim()).toBe('Append');
    button('New note')!.click();
    await settle();
    primary().click();
    await settle();
    expect(primary().textContent?.trim()).toBe('Working…');
    expect(primary().getAttribute('aria-busy')).toBe('true');
    button('Stop')!.click();
    await settle();
    expect(invoke.mock.calls.some(([c]) => c === 'cancel_extraction')).toBe(true);
  });

  describe('Android: a finished run closes into its destination', () => {
    beforeEach(() => {
      isAndroid.set(true);
      window.addEventListener('popstate', handlePopState);
      history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
      initPhoneNavHistory();
    });
    afterEach(async () => {
      if (app) { unmount(app); app = null; }
      flushSync();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
      window.removeEventListener('popstate', handlePopState);
    });

    async function finishRun() {
      await typeSource('my notes');
      button('Make note')!.click();
      await settle();
      run.resolve({ uuid: CREATED.uuid, label: 'Notes/Inbox' });
      await settle();
    }

    it('opened over a note, filed in another folder: the overlay entry becomes that list, depth unchanged', async () => {
      navigateToPane('note');
      selectedFolder.set('Notes');
      await open();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      const depth = depthFromHistoryState(history.state);
      const length = history.length;
      await finishRun();
      expect(host.querySelector('textarea')).toBeNull();
      expect(overlayFromHistoryState(history.state)).toBeNull();
      expect(history.state).toMatchObject({ pane: 'list', depth });
      expect(history.length).toBe(length);
      expect(get(selectedNote)?.uuid).toBe(CREATED.uuid);
      // That list is titled by the folder the run filed into (Task 4.2), so
      // the note opened above it names it in its Back.
      navigateToPane('note');
      expect(get(backLabel)).toBe('Inbox');
    });

    it('filed in the folder already shown: the overlay entry becomes the note, depth unchanged', async () => {
      selectedFolder.set('Notes/Inbox');
      await open();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      const depth = depthFromHistoryState(history.state);
      const length = history.length;
      await finishRun();
      expect(overlayFromHistoryState(history.state)).toBeNull();
      expect(history.state).toMatchObject({ pane: 'note', depth });
      expect(history.length).toBe(length);
      expect(get(selectedNote)?.uuid).toBe(CREATED.uuid);
    });

    it('opened over the list, filed in another folder: one Back to the list beneath, no dead entry', async () => {
      selectedFolder.set('Notes');
      await open();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeTruthy());
      const depth = depthFromHistoryState(history.state);
      await finishRun();
      await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBeNull());
      expect(history.state).toMatchObject({ pane: 'list', depth: depth - 1 });
      expect(get(selectedNote)?.uuid).toBe(CREATED.uuid);
    });
  });
});
