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
        queue = queue.filter((c) => c.id !== args?.captureId);
        return { uuid: 'NEW', label: (args?.folder as string | null) ?? 'Notes/Inbox' };
      case 'append_capture':
        queue = [];
        return { uuid: 'N1', label: 'Notes/Reading' };
      case 'list_capture_folders':
        return args?.accountId === 'gmail:a@x'
          ? { default: 'Notes/Inbox', folders: ['Notes', 'Notes/Reading'] }
          : { default: 'Notes', folders: ['Notes'] };
      case 'search_notes':
        return [
          { uuid: 'N1', id: 'm1', title: 'Reading list', label: 'Notes/Reading', account_id: 'gmail:a@x' },
          { uuid: 'L1', id: 'm2', title: 'Locked note', label: 'Notes', account_id: 'gmail:a@x', push_blocked_by_remote: true },
        ];
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
    expect(save?.[1]).toEqual({ accountId: 'gmail:a@x', captureId: 'c1', title: 'Kept', folder: null });
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

  it('a new note goes to Inbox by default, or to the folder chosen from the ones Rust offers', async () => {
    const { target, host } = render();
    await settle();
    const select = target.querySelector('#capture-folder') as HTMLSelectElement;
    const options = [...select.querySelectorAll('option')].map((o) => [o.value, o.textContent?.trim()]);
    expect(options).toEqual([['', 'Inbox (default)'], ['Notes', 'Notes'], ['Notes/Reading', 'Notes/Reading']]);
    expect(target.querySelector('.capture-folder-hint')?.textContent?.replace(/\s+/g, ' ').trim()).toBe(
      'Saved as-is. In Inbox, Organize can file it later.',
    );
    select.value = 'Notes/Reading';
    select.dispatchEvent(new Event('change', { bubbles: true }));
    button(target, 'Save').click();
    await settle();
    const save = invoke.mock.calls.find((c) => c[0] === 'save_capture');
    expect(save?.[1]).toEqual({ accountId: 'gmail:a@x', captureId: 'c1', title: 'worth keeping', folder: 'Notes/Reading' });
    unmount(host);
  });

  it('changing the account forgets the folder chosen in the other one', async () => {
    capabilitiesByAccount.set({});
    const { target, host } = render();
    await settle();
    const folder = target.querySelector('#capture-folder') as HTMLSelectElement;
    folder.value = 'Notes/Reading';
    folder.dispatchEvent(new Event('change', { bubbles: true }));
    const acct = target.querySelector('#capture-account') as HTMLSelectElement;
    acct.value = 'microsoft:b@x';
    acct.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(folder.value).toBe('');
    // Outlook can't create the Inbox: the default says where the note really goes.
    expect([...folder.querySelectorAll('option')].map((o) => [o.value, o.textContent?.trim()])).toEqual([
      ['', 'Notes (default)'],
      ['Notes', 'Notes'],
    ]);
    button(target, 'Save').click();
    await settle();
    const save = invoke.mock.calls.find((c) => c[0] === 'save_capture');
    expect(save?.[1]).toMatchObject({ accountId: 'microsoft:b@x', folder: null });
    unmount(host);
  });

  it('Add to note: search, pick a note, and append — sending the ids, never the content', async () => {
    const { target, host } = render();
    await settle();
    button(target, 'Add to existing note').click();
    flushSync();
    expect(target.querySelector('#capture-title')).toBeNull();
    const add = button(target, 'Add to note');
    expect(add.disabled).toBe(true);
    const search = target.querySelector('#capture-target') as HTMLInputElement;
    search.value = 'reading';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
    await settle();
    const results = [...target.querySelectorAll('.capture-result')].map((b) => b.textContent);
    expect(results).toHaveLength(1);
    expect(results[0]).toContain('Reading list');
    // A note iCloud refuses is locked in the editor; the sheet doesn't offer it.
    expect(target.textContent).not.toContain('Locked note');
    (target.querySelector('.capture-result') as HTMLButtonElement).click();
    flushSync();
    expect(target.textContent).toContain('Reading list');
    expect(search.value).toBe('Reading list');
    expect(add.disabled).toBe(false);
    add.click();
    await settle();
    const call = invoke.mock.calls.find((c) => c[0] === 'append_capture');
    expect(call?.[1]).toEqual({ accountId: 'gmail:a@x', captureId: 'c1', targetUuid: 'N1' });
    expect(invoke.mock.calls.some((c) => c[0] === 'save_capture')).toBe(false);
    expect(target.querySelector('dialog')).toBeNull();
    unmount(host);
  });

  it('an account that stops taking notes takes its picked note with it', async () => {
    capabilitiesByAccount.set({});
    const { target, host } = render();
    await settle();
    button(target, 'Add to existing note').click();
    flushSync();
    const search = target.querySelector('#capture-target') as HTMLInputElement;
    search.value = 'reading';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
    await settle();
    (target.querySelector('.capture-result') as HTMLButtonElement).click();
    flushSync();
    expect(button(target, 'Add to note').disabled).toBe(false);
    // gmail:a@x can no longer write: the sheet falls back to microsoft:b@x.
    capabilitiesByAccount.set({
      'gmail:a@x': { has_trash: true, writes: { notes: false, relocate: false, folders: false, sidecars: false } },
    });
    await settle();
    expect((target.querySelector('#capture-account') as HTMLSelectElement).value).toBe('microsoft:b@x');
    expect(button(target, 'Add to note').disabled).toBe(true);
    expect(search.value).toBe('');
    unmount(host);
  });

  const SECOND: PendingCapture = {
    id: 'c2',
    payload: { url: null, text: 'second share', title: null },
    links: [],
    default_title: 'second share',
    received_at_ms: 1,
  };

  async function saveInto(target: HTMLElement, path: string) {
    const folder = target.querySelector('#capture-folder') as HTMLSelectElement;
    folder.value = path;
    folder.dispatchEvent(new Event('change', { bubbles: true }));
    button(target, 'Save').click();
    await settle();
  }

  // Live pass 2026-10-09: two shares were waiting; the second one appeared
  // in the same sheet with the folder silently back on Inbox, so a second
  // Save filed it there. Where things go is kept; what was shared is not.
  it('the next shared item keeps where the last one was filed, and says it is a new item', async () => {
    queue = [CAPTURE, SECOND];
    const { target, host } = render();
    await settle();
    await saveInto(target, 'Notes/Reading');
    expect((target.querySelector('#capture-title') as HTMLInputElement).value).toBe('second share');
    expect((target.querySelector('#capture-folder') as HTMLSelectElement).value).toBe('Notes/Reading');
    expect(target.querySelector('.capture-saved')?.textContent).toContain('Saved to Notes/Reading');
    button(target, 'Save').click();
    await settle();
    const saves = invoke.mock.calls.filter((c) => c[0] === 'save_capture').map((c) => c[1]);
    expect(saves).toEqual([
      { accountId: 'gmail:a@x', captureId: 'c1', title: 'worth keeping', folder: 'Notes/Reading' },
      { accountId: 'gmail:a@x', captureId: 'c2', title: 'second share', folder: 'Notes/Reading' },
    ]);
    unmount(host);
  });

  it('a share that arrives after the queue emptied starts fresh, with no notice', async () => {
    const { target, host } = render();
    await settle();
    await saveInto(target, 'Notes/Reading');
    expect(target.querySelector('dialog')).toBeNull();
    queue = [SECOND];
    events.handlers['capture-received']({ payload: null });
    await settle();
    expect((target.querySelector('#capture-folder') as HTMLSelectElement).value).toBe('');
    expect(target.querySelector('.capture-saved')).toBeNull();
    unmount(host);
  });

  it('a kept folder the next item can no longer use goes back to the default', async () => {
    queue = [CAPTURE, SECOND];
    const { target, host } = render();
    await settle();
    // By the time the second item shows, Notes/Reading has been deleted.
    const base = invoke.getMockImplementation()!;
    invoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) =>
      cmd === 'list_capture_folders' && queue[0]?.id === 'c2' ? { default: 'Notes/Inbox', folders: ['Notes'] } : base(cmd, args),
    );
    await saveInto(target, 'Notes/Reading');
    expect((target.querySelector('#capture-folder') as HTMLSelectElement).value).toBe('');
    unmount(host);
  });

  it('the next item keeps Add to existing note and its note; Discard clears the notice', async () => {
    queue = [CAPTURE, SECOND];
    const { target, host } = render();
    await settle();
    button(target, 'Add to existing note').click();
    flushSync();
    const search = target.querySelector('#capture-target') as HTMLInputElement;
    search.value = 'reading';
    search.dispatchEvent(new Event('input', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
    await settle();
    (target.querySelector('.capture-result') as HTMLButtonElement).click();
    flushSync();
    invoke.mockImplementationOnce(async () => {
      queue = [SECOND];
      return { uuid: 'N1', label: 'Notes/Reading' };
    });
    button(target, 'Add to note').click();
    await settle();
    expect(target.querySelector('.capture-saved')?.textContent).toContain('Added to Reading list');
    expect(button(target, 'Add to note').disabled).toBe(false);
    button(target, 'Discard').click();
    await settle();
    expect(target.querySelector('.capture-saved')).toBeNull();
    unmount(host);
  });
});
