// @vitest-environment jsdom
//
// Settings › Links: per-account auto-follow (spec 2026-10-28 D2).
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { accounts } from '../stores/notes';
import type { Account } from '../types';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import AutoFollowSettings from './AutoFollowSettings.svelte';

async function settle() {
  for (let i = 0; i < 6; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

const acct = (id: string, email: string): Account => ({ id, email, added_at: '2026-10-28T00:00:00Z' }) as Account;

describe('AutoFollowSettings', () => {
  let host: HTMLElement;
  let form: { isDirty(): boolean; save(): Promise<void> };

  beforeEach(async () => {
    accounts.set([acct('gmail:a@x', 'a@x'), acct('gmail:b@x', 'b@x')]);
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_link_settings') return Promise.resolve({ auto_follow: ['gmail:b@x'], auto_follow_max: 3, auto_follow_min_score: 40, check_saved_sources: true });
      if (cmd === 'set_link_settings') return Promise.resolve(null);
      return Promise.reject(new Error(`unexpected ${cmd}`));
    });
    host = document.createElement('div');
    document.body.appendChild(host);
    form = mount(AutoFollowSettings, { target: host }) as unknown as typeof form;
    await settle();
  });

  afterEach(() => {
    unmount(form as never);
    host.remove();
    accounts.set([]);
  });

  const boxes = () => Array.from(host.querySelectorAll<HTMLInputElement>('input[type=checkbox]:not([data-testid=check-saved-sources])'));
  const checkBox = () => host.querySelectorAll<HTMLInputElement>('[data-testid=check-saved-sources]');

  it('shows each account with its saved state and saves the new list', async () => {
    expect(boxes().map((b) => b.checked)).toEqual([false, true]);
    expect(host.textContent).toContain('Up to 3 links per source');
    expect(form.isDirty()).toBe(false);
    boxes()[0].click();
    boxes()[1].click();
    flushSync();
    expect(form.isDirty()).toBe(true);
    await form.save();
    expect(invoke).toHaveBeenCalledWith('set_link_settings', { autoFollow: ['gmail:a@x'], checkSavedSources: true });
    expect(form.isDirty()).toBe(false);
  });

  it('offers one checkbox to check saved links, on by default, and saves it off', async () => {
    expect(checkBox().length).toBe(1);
    expect(checkBox()[0].checked).toBe(true);
    expect(host.textContent).toContain('Check saved links for changes before reusing them');
    expect(host.textContent).toContain('Off: Jodd uses the copy it saved, without going online.');
    expect(form.isDirty()).toBe(false);
    checkBox()[0].click();
    flushSync();
    expect(form.isDirty()).toBe(true);
    await form.save();
    expect(invoke).toHaveBeenCalledWith('set_link_settings', { autoFollow: ['gmail:b@x'], checkSavedSources: false });
    expect(form.isDirty()).toBe(false);
  });
});
