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
import './__fixtures__/dialogStub';
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
    expect(text).toContain('No extra copies found');
    expect(text).not.toMatch(/24 hours|last \d+|recent/i);

    unmount(host);
  });
});

describe('DupReviewModal as a Modal', () => {
  const group = {
    uuid: 'u1',
    keeper: { id: 'k', title: 'Keep me', label: 'Notes/Inbox', date: '', body_preview: 'x' },
    orphans: [{ id: 'o1', date: '', body_preview: 'y' }],
  };
  let onClose: ReturnType<typeof vi.fn<() => void>>;
  let host: ReturnType<typeof mount>;
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  async function open() {
    invoke.mockImplementation((cmd: string) =>
      cmd === 'preview_orphans' ? Promise.resolve([group]) : Promise.reject(new Error(`unexpected ${cmd}`)),
    );
    onClose = vi.fn<() => void>();
    const target = document.createElement('div');
    document.body.appendChild(target);
    host = mount(DupReviewModal, { target, props: { accountId: 'gmail:a@b.com', onClose } });
    for (let i = 0; i < 5; i++) await tick();
    flushSync();
    return target.querySelector('dialog')!;
  }
  const footerButton = (d: HTMLElement, text: RegExp) =>
    [...d.querySelectorAll<HTMLButtonElement>('footer button')].find((b) => text.test(b.textContent ?? ''))!;

  it('is an lg Modal; Trash is the danger action (never primary) beside Cancel', async () => {
    const d = await open();
    expect(d.hasAttribute('data-presentation')).toBe(true);
    expect(d.dataset.size).toBe('lg');
    expect(d.querySelector('h2')!.textContent).toBe('Extra Gmail copies');
    const trash = footerButton(d, /Move 1 to Trash/);
    expect(trash.classList.contains('danger')).toBe(true);
    expect(trash.classList.contains('primary')).toBe(false);
    expect(footerButton(d, /^Cancel$/)).toBeTruthy();
    expect(d.querySelector('footer .primary')).toBeNull();
    unmount(host);
  });

  it('Esc closes (new behaviour)', async () => {
    const d = await open();
    d.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount(host);
  });

  it('a backdrop click closes (new behaviour); a click inside does not', async () => {
    const d = await open();
    d.querySelector<HTMLElement>('.body')!.click();
    flushSync();
    expect(onClose).not.toHaveBeenCalled();
    d.click();
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount(host);
  });

  it('Trash invokes the trash command with the selected ids', async () => {
    const d = await open();
    invoke.mockImplementation((cmd: string) =>
      cmd === 'trash_specific_messages' ? Promise.resolve(1) : Promise.reject(new Error(`unexpected ${cmd}`)),
    );
    footerButton(d, /Move 1 to Trash/).click();
    for (let i = 0; i < 5; i++) await tick();
    expect(invoke).toHaveBeenCalledWith('trash_specific_messages', { accountId: 'gmail:a@b.com', messageIds: ['o1'] });
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount(host);
  });

  it('Cancel closes without trashing', async () => {
    const d = await open();
    footerButton(d, /^Cancel$/).click();
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalledWith('trash_specific_messages', expect.anything());
    unmount(host);
  });

  it('Esc while trashing asks first; Keep editing leaves the dialog open', async () => {
    const d = await open();
    invoke.mockImplementation((cmd: string) =>
      cmd === 'trash_specific_messages' ? new Promise(() => {}) : Promise.reject(new Error(`unexpected ${cmd}`)),
    );
    footerButton(d, /Move 1 to Trash/).click();
    for (let i = 0; i < 3; i++) await tick();
    expect(footerButton(d, /Trashing/)).toBeTruthy();
    d.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(onClose).not.toHaveBeenCalled();
    const guard = [...document.querySelectorAll('dialog')][1];
    expect(guard.querySelector('h2')!.textContent).toBe('Trashing is in progress. Close anyway?');
    [...guard.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Keep editing')!.click();
    flushSync();
    expect(document.querySelectorAll('dialog')).toHaveLength(1);
    expect(onClose).not.toHaveBeenCalled();
    unmount(host);
  });

  it('Esc when not trashing closes without a guard', async () => {
    const d = await open();
    d.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(document.querySelectorAll('dialog')).toHaveLength(1);
    expect(onClose).toHaveBeenCalledTimes(1);
    unmount(host);
  });
});

// Measured 2026-11-17: the backend's orphan fetches came back rate-limited and
// were skipped in silence, so the modal listed 2 of the 7 notes the pill
// counted, and the other copies could not be trashed from it. The backend now
// reports what it could not read (`unloaded`); the modal must say so and offer
// another try instead of showing a shorter list as if it were complete.
describe('DupReviewModal with duplicates it could not load', () => {
  const keeper = { id: 'k', title: 'Keep me', label: 'Notes/Inbox', date: '', body_preview: 'x' };
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  async function mountWith(responses: unknown[]) {
    let call = 0;
    invoke.mockImplementation((cmd: string) =>
      cmd === 'preview_orphans'
        ? Promise.resolve(responses[Math.min(call++, responses.length - 1)])
        : Promise.reject(new Error(`unexpected ${cmd}`)),
    );
    const target = document.createElement('div');
    document.body.appendChild(target);
    const host = mount(DupReviewModal, { target, props: { accountId: 'gmail:a@b.com', onClose: () => {} } });
    for (let i = 0; i < 5; i++) await tick();
    flushSync();
    return { host, d: target.querySelector('dialog')! };
  }

  it('says how many it could not load, and does not call the list empty', async () => {
    const { host, d } = await mountWith([
      [{ uuid: 'u1', keeper, orphans: [], unloaded: ['o1', 'o2'] }],
    ]);
    const notice = d.querySelector('.unloaded');
    expect(notice?.textContent).toMatch(/Couldn.t load 2 extra copies/);
    expect(d.textContent).not.toContain('No extra copies found');
    unmount(host);
  });

  it('Try again re-runs the scan and shows what loads the second time', async () => {
    const { host, d } = await mountWith([
      [{ uuid: 'u1', keeper, orphans: [], unloaded: ['o1'] }],
      [{ uuid: 'u1', keeper, orphans: [{ id: 'o1', date: '', body_preview: 'y', title: '', label: '' }], unloaded: [] }],
    ]);
    const retry = [...d.querySelectorAll<HTMLButtonElement>('.unloaded button')].find((b) => /Try again/.test(b.textContent ?? ''))!;
    retry.click();
    for (let i = 0; i < 5; i++) await tick();
    flushSync();
    expect(invoke.mock.calls.filter(([cmd]) => cmd === 'preview_orphans')).toHaveLength(2);
    expect(d.querySelector('.unloaded')).toBeNull();
    expect(d.textContent).toContain('Move 1 to Trash');
    unmount(host);
  });
});
