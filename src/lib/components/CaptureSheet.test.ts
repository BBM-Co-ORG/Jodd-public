// @vitest-environment jsdom
//
// Share to Jodd's capture sheet (spec 2026-10-06-share-to-jodd §3.5, §6).
// Mounted, not just its helpers, because the cold-start drain and the
// reset-on-new-capture are reactive behaviour (gotcha #28).
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { get } from 'svelte/store';
import './__fixtures__/dialogStub';

const invoke = vi.fn();
const events = vi.hoisted(() => ({ handlers: {} as Record<string, (e: { payload: unknown }) => void> }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
const openUrl = vi.hoisted(() => vi.fn(async () => {}));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name: string, cb: (e: { payload: unknown }) => void) => {
    events.handlers[name] = cb;
    return () => {};
  }),
}));

import CaptureSheet from './CaptureSheet.svelte';
import { accounts, currentAccount, capabilitiesByAccount, error } from '../stores/notes';
import { extractModalOpen } from '../stores/ui';
import { extractPrefill, pendingCaptures, type PendingCapture } from '../capture';

const CAPTURE: PendingCapture = {
  id: 'c1',
  payload: { url: 'https://a.example/post', text: 'worth keeping', title: null },
  links: ['https://a.example/post'],
  default_title: 'worth keeping',
  received_at_ms: 0,
};

let queue: PendingCapture[] = [];

async function settle() {
  for (let i = 0; i < 20; i++) await Promise.resolve();
  flushSync();
}

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(CaptureSheet, { target });
  flushSync();
  return { target, host };
}

function button(target: HTMLElement, label: string): HTMLButtonElement {
  const b = [...target.querySelectorAll('button')].find((x) => x.textContent?.trim() === label);
  if (!b) throw new Error(`no button "${label}"`);
  return b as HTMLButtonElement;
}

beforeEach(() => {
  invoke.mockReset();
  queue = [CAPTURE];
  invoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    switch (cmd) {
      case 'take_pending_captures':
        return queue;
      case 'save_capture':
        queue = [];
        return { uuid: 'NEW', label: 'Notes/Inbox' };
      case 'discard_capture':
        queue = queue.filter((c) => c.id !== args?.captureId);
        return true;
      case 'list_cached_notes_in_folder':
        return [];
      default:
        return null;
    }
  });
  accounts.set([
    { id: 'gmail:a@x', email: 'a@x', added_at: '' },
    { id: 'microsoft:b@x', email: 'b@x', added_at: '' },
  ]);
  capabilitiesByAccount.set({
    'microsoft:b@x': { has_trash: false, writes: { notes: false, relocate: false, folders: false, sidecars: false } },
  });
  currentAccount.set('gmail:a@x');
  extractModalOpen.set(false);
  extractPrefill.set(null);
  pendingCaptures.set([]);
  error.set(null);
  document.body.innerHTML = '';
});

describe('CaptureSheet', () => {
  it('shows a capture that was queued before it mounted (cold start), with no event', async () => {
    const { target, host } = render();
    await settle();
    expect(target.querySelector('dialog')?.open).toBe(true);
    expect((target.querySelector('#capture-title') as HTMLInputElement).value).toBe('worth keeping');
    expect(target.textContent).toContain('https://a.example/post');
    unmount(host);
  });

  it('offers only accounts that can write notes', async () => {
    const { target, host } = render();
    await settle();
    const options = [...target.querySelectorAll('#capture-account option')].map((o) => (o as HTMLOptionElement).value);
    expect(options).toEqual(['gmail:a@x']);
    unmount(host);
  });

  it('Save sends the capture id and title, never the content, then closes', async () => {
    const { target, host } = render();
    await settle();
    const title = target.querySelector('#capture-title') as HTMLInputElement;
    title.value = '  Kept  ';
    title.dispatchEvent(new Event('input', { bubbles: true }));
    button(target, 'Save').click();
    await settle();
    const save = invoke.mock.calls.find((c) => c[0] === 'save_capture');
    expect(save?.[1]).toEqual({ accountId: 'gmail:a@x', captureId: 'c1', title: 'Kept' });
    expect(JSON.stringify(save?.[1])).not.toContain('worth keeping');
    expect(target.querySelector('dialog')).toBeNull();
    unmount(host);
  });

  it('Discard writes nothing', async () => {
    const { target, host } = render();
    await settle();
    button(target, 'Discard').click();
    await settle();
    expect(invoke.mock.calls.some((c) => c[0] === 'save_capture')).toBe(false);
    expect(invoke).toHaveBeenCalledWith('discard_capture', { captureId: 'c1' });
    expect(target.querySelector('dialog')).toBeNull();
    unmount(host);
  });

  it('shows what was shared — its source, the link and the text — apart from the form', async () => {
    const { target, host } = render();
    await settle();
    const shared = target.querySelector('section[aria-label="What was shared"]') as HTMLElement;
    expect(shared.textContent).toContain('from a.example');
    expect(shared.textContent).toContain('https://a.example/post');
    expect(shared.textContent).toContain('worth keeping');
    expect(shared.querySelector('input, select')).toBeNull();
    unmount(host);
  });

  it('a shared link opens in the browser', async () => {
    const { target, host } = render();
    await settle();
    (target.querySelector('.capture-url') as HTMLButtonElement).click();
    expect(openUrl).toHaveBeenCalledWith('https://a.example/post');
    unmount(host);
  });

  it('offers every AI mode, and hands the capture over with the chosen one', async () => {
    const { target, host } = render();
    await settle();
    const chips = [...target.querySelectorAll('.capture-chip')].map((b) => b.textContent?.trim());
    expect(chips).toEqual(['Key points', 'Summarize', 'Action items', 'Expand bullets', 'Transcript']);
    button(target, 'Summarize').click();
    await settle();
    expect(get(extractPrefill)).toEqual({
      captureId: 'c1',
      text: 'https://a.example/post\n\nworth keeping',
      title: 'worth keeping',
      workflow: 'summarize',
    });
    expect(get(extractModalOpen)).toBe(true);
    expect(target.querySelector('dialog')).toBeNull();
    // Closing the modal without a note brings the still-queued capture back.
    extractModalOpen.set(false);
    await settle();
    expect(target.querySelector('dialog')?.open).toBe(true);
    unmount(host);
  });

  it('a capture drained before the accounts loaded still preselects an account (cold start)', async () => {
    accounts.set([]);
    const { target, host } = render();
    await settle();
    expect(target.querySelector('dialog')).toBeNull();
    accounts.set([{ id: 'gmail:a@x', email: 'a@x', added_at: '' }]);
    await settle();
    expect((target.querySelector('#capture-account') as HTMLSelectElement).value).toBe('gmail:a@x');
    expect(button(target, 'Save').disabled).toBe(false);
    unmount(host);
  });

  it('waits for sign-in when there is no account', async () => {
    accounts.set([]);
    const { target, host } = render();
    await settle();
    expect(target.querySelector('dialog')).toBeNull();
    unmount(host);
  });

  it('a share that works clears the error a failed share left on the bar', async () => {
    queue = [];
    const { host } = render();
    await settle();
    events.handlers['capture-error']({ payload: "Couldn't read what was shared: it carried no link and no text." });
    expect(get(error)).toContain("Couldn't read what was shared");
    queue = [CAPTURE];
    events.handlers['capture-received']({ payload: null });
    await settle();
    expect(get(error)).toBeNull();
    unmount(host);
  });

  it('a share that works leaves any other error on the bar alone', async () => {
    queue = [];
    const { host } = render();
    await settle();
    events.handlers['capture-error']({ payload: "Couldn't read what was shared: it carried no link and no text." });
    error.set('Account gmail:a@x needs to be signed in again.');
    queue = [CAPTURE];
    events.handlers['capture-received']({ payload: null });
    await settle();
    expect(get(error)).toBe('Account gmail:a@x needs to be signed in again.');
    unmount(host);
  });

  it('a capture-received event re-reads the queue; a capture-error reaches ErrorBar', async () => {
    queue = [];
    const { target, host } = render();
    await settle();
    expect(target.querySelector('dialog')).toBeNull();
    queue = [CAPTURE];
    events.handlers['capture-received']({ payload: null });
    await settle();
    expect(target.querySelector('dialog')?.open).toBe(true);
    events.handlers['capture-error']({ payload: "Couldn't read what was shared: it carried no link and no text." });
    expect(get(error)).toContain("Couldn't read what was shared");
    unmount(host);
  });
});
