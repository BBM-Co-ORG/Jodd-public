// @vitest-environment jsdom
//
// The empty state must describe the scan the backend actually runs.
//
// It used to say "No duplicates found in notes you've edited in the last 24
// hours." That window was the old recent-edit gate, removed from
// `preview_orphans` and `safe_cleanup_orphans_for_account` (lib.rs — "No 24h
// recent-edit gate") precisely so this modal would show every duplicate the
// sidebar's "N dup" pill counts. The copy kept promising the gate after the
// code dropped it, so a user with older duplicates could read "none found"
// as "none recent, maybe some older" — a scope the scan no longer has.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import DupReviewModal from './DupReviewModal.svelte';

describe('DupReviewModal empty state', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  it('does not claim a recency window the scan no longer applies', async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === 'preview_orphans' ? Promise.resolve([]) : Promise.reject(new Error(`unexpected ${cmd}`)),
    );
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(DupReviewModal, { target, props: { accountId: 'gmail:a@b.com', onClose: () => {} } });
    for (let i = 0; i < 5; i++) await tick();
    flushSync();

    const text = target.querySelector('.placeholder')?.textContent ?? '';
    expect(text).toContain('No duplicates found');
    expect(text).not.toMatch(/24 hours|last \d+|recent/i);

    unmount(host);
  });
});
