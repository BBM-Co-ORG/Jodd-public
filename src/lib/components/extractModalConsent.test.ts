// @vitest-environment jsdom
//
// Privacy PR3 (spec 2026-10-08 §4.5/§5): a consent refusal in the Extract
// modal offers "Allow AI for this account" for the account that was refused —
// never whichever account is current when the button is clicked. The modal
// stays open across sidebar switches (pre-flight W3). Mounted (gotcha #28).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { currentAccount, selectedFolder, notes, selectedNote, folderSuggestions } from '../stores/notes';
import { extractPrefill } from '../capture';
import { ANALYZE_DEBOUNCE_MS } from '../ingestSources';
import { AI_CONSENT_NEEDED } from '../aiConsent';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class {
    onmessage: (m: unknown) => void = () => {};
  },
}));

import LessonExtractModal from './LessonExtractModal.svelte';

const A = 'gmail:a@b.com';
const B = 'gmail:other@b.com';

async function settle() {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}
const allowButton = (host: HTMLElement) =>
  [...host.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Allow AI for this account');

describe('LessonExtractModal — consent refusal', () => {
  let host: HTMLElement;
  let app: Record<string, unknown>;
  let extractError: unknown;

  beforeEach(async () => {
    vi.useFakeTimers();
    extractError = `provider not configured: ${AI_CONSENT_NEEDED}`;
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'analyze_ingest_sources') {
        return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
      }
      if (cmd === 'extract_note') return Promise.reject(extractError);
      if (cmd === 'allow_ai_for_account') return Promise.resolve();
      return Promise.resolve([]);
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    currentAccount.set(A);
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

  async function extractRefused() {
    extractPrefill.set({ captureId: 'c1', text: 'plain text, no link', title: '' });
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
    host.querySelector<HTMLButtonElement>('button.primary')!.click();
    await settle();
  }

  it('offers the allow for the refused account, and withdraws it once answered', async () => {
    await extractRefused();
    expect(host.querySelector('.error')?.textContent).toContain(AI_CONSENT_NEEDED);
    allowButton(host)!.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('allow_ai_for_account', { accountId: A });
    expect(allowButton(host)).toBeUndefined();
  });

  it('withdraws the offer when the account changes, so it can never grant another account', async () => {
    await extractRefused();
    expect(allowButton(host)).toBeDefined();
    currentAccount.set(B);
    await settle();
    expect(allowButton(host)).toBeUndefined();
    expect(invoke.mock.calls.some(([c]) => c === 'allow_ai_for_account')).toBe(false);
  });

  it('offers no allow for a refusal that lands after the user switched account', async () => {
    let refuse!: (e: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'analyze_ingest_sources') {
        return Promise.resolve({ sources: [], mostly_urls: false, context_text: '', context_chars: 0 });
      }
      if (cmd === 'extract_note') return new Promise((_, r) => { refuse = r; });
      return Promise.resolve([]);
    });
    extractPrefill.set({ captureId: 'c1', text: 'plain text, no link', title: '' });
    app = mount(LessonExtractModal, { target: host, props: { open: true } }) as Record<string, unknown>;
    await settle();
    await vi.advanceTimersByTimeAsync(ANALYZE_DEBOUNCE_MS);
    await settle();
    host.querySelector<HTMLButtonElement>('button.primary')!.click();
    await settle();
    currentAccount.set(B);
    await settle();
    refuse(`provider not configured: ${AI_CONSENT_NEEDED}`);
    await settle();
    // The refusal was A's; B is current, so granting from here would grant B.
    expect(allowButton(host)).toBeUndefined();
    expect(invoke.mock.calls.some(([c]) => c === 'allow_ai_for_account')).toBe(false);
  });

  it('offers nothing for any other refusal', async () => {
    extractError = 'provider not configured: AI data access is disabled or this account is unavailable. Review its AI data permission in Account Settings.';
    await extractRefused();
    expect(host.querySelector('.error')).not.toBeNull();
    expect(allowButton(host)).toBeUndefined();
  });
});
