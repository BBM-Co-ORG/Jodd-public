// @vitest-environment jsdom
//
// Spec 2026-10-11 (iCloud LLM tags → Keywords line): the Extract modal says,
// before running, that suggested tags become a Keywords line on iCloud. The
// run writes to $currentAccount (extract() and ingest() read it at click
// time), so the hint must follow that store while the modal stays open.
// Mounted, not just the predicate (gotcha #28).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { currentAccount, capabilitiesByAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import { KEYWORDS_HINT } from '../editor/bodyTags';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const ICLOUD = 'icloud:a@me.com';
const GMAIL = 'gmail:a@b.com';

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}
const hintShown = (host: HTMLElement) => (host.textContent ?? '').includes(KEYWORDS_HINT);
const workflowButton = (host: HTMLElement, label: string) =>
  [...host.querySelectorAll('button')].find((b) => b.textContent?.trim() === label)!;

describe('LessonExtractModal — Keywords hint', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;

  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'analyze_ingest_sources') {
        return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
      }
      return Promise.resolve([]);
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    capabilitiesByAccount.set({
      [ICLOUD]: { has_trash: true, tag_model: 'inline_objects' },
      [GMAIL]: { has_trash: true, tag_model: 'body_text' },
    });
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
    vi.restoreAllMocks();
    capabilitiesByAccount.set({});
  });

  async function open(account: string) {
    currentAccount.set(account);
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
  }

  it('shows on an iCloud account', async () => {
    await open(ICLOUD);
    expect(hintShown(host)).toBe(true);
  });

  // #178's review: the tag model must be right on the FIRST render, before
  // capabilities arrive — the account id already names the backend
  // (tagModelOf, mirror of db::tag_model_for_account).
  it('shows on an iCloud account before its capabilities have loaded', async () => {
    capabilitiesByAccount.set({});
    await open(ICLOUD);
    expect(hintShown(host)).toBe(true);
  });

  it('does not show on a Gmail account', async () => {
    await open(GMAIL);
    expect(hintShown(host)).toBe(false);
  });

  it('hides for Action items, which carries no tags', async () => {
    await open(ICLOUD);
    workflowButton(host, 'Action items').click();
    await settle();
    expect(hintShown(host)).toBe(false);
    workflowButton(host, 'Summarize').click();
    await settle();
    expect(hintShown(host)).toBe(true);
  });

  it('follows the current account while open', async () => {
    await open(ICLOUD);
    expect(hintShown(host)).toBe(true);
    currentAccount.set(GMAIL);
    await settle();
    expect(hintShown(host)).toBe(false);
    currentAccount.set(ICLOUD);
    await settle();
    expect(hintShown(host)).toBe(true);
  });
});
