// @vitest-environment jsdom
//
// Settings › Links: the YouTube Data API key form (spec 2026-10-28 §6.2).
// Status comes from the flag-file command only (gotcha #15); the key is
// written, never read back.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import YoutubeKeySettings from './YoutubeKeySettings.svelte';

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('YoutubeKeySettings', () => {
  let host: HTMLElement;
  let form: { isDirty(): boolean; save(): Promise<void> };

  beforeEach(async () => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_youtube_key_status') return Promise.resolve(false);
      if (cmd === 'set_youtube_key') return Promise.resolve(null);
      return Promise.reject(new Error(`unexpected ${cmd}`));
    });
    host = document.createElement('div');
    document.body.appendChild(host);
    form = mount(YoutubeKeySettings, { target: host }) as unknown as typeof form;
    await settle();
  });

  afterEach(() => {
    unmount(form as never);
    host.remove();
  });

  const status = () => host.querySelector('[data-testid="youtube-key-status"]')!.textContent!.replace(/\s+/g, ' ').trim();

  it('reads only the status flag, and a typed key makes the form dirty and saves trimmed', async () => {
    expect(invoke.mock.calls.map(([c]) => c)).toEqual(['get_youtube_key_status']);
    expect(status()).toBe('YouTube comments: no key saved.');
    expect(form.isDirty()).toBe(false);
    const input = host.querySelector('input')!;
    input.value = '  AIzaSyA1234567890abcdefghijklmnopqrstu ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    expect(form.isDirty()).toBe(true);
    await form.save();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_youtube_key', { key: 'AIzaSyA1234567890abcdefghijklmnopqrstu' });
    expect(status()).toBe('YouTube comments: a key is saved.');
    expect(form.isDirty()).toBe(false);
  });

  it('a refused key throws for the page Save and shows why', async () => {
    invoke.mockImplementation((cmd: string) => (cmd === 'set_youtube_key' ? Promise.reject('That does not look like a key') : Promise.resolve(false)));
    const input = host.querySelector('input')!;
    input.value = 'nope';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    flushSync();
    await expect(form.save()).rejects.toThrow('That does not look like a key');
    await settle();
    expect(host.querySelector('[role="alert"]')!.textContent).toContain('That does not look like a key');
  });
});
