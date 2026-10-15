// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { it, expect, vi, afterEach, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { readable } from 'svelte/store';
import { accounts, currentAccount } from '../stores/notes';

const invoke = vi.fn((cmd: string): Promise<unknown> => Promise.resolve(cmd === 'count_pending_pushes' ? { notes: 0, deletes: 0, pins: 0, folders: 0 } : []));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...(a as [string])) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}), emit: () => Promise.resolve() }));
vi.mock('../stores/platform', () => ({ isAndroid: readable(true), supportsIcloud: readable(true) }));

import Sidebar from './Sidebar.svelte';

let component: ReturnType<typeof mount>;
const vsDesc = Object.getOwnPropertyDescriptor(Document.prototype, 'visibilityState');
beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) => Promise.resolve(cmd === 'count_pending_pushes' ? { notes: 0, deletes: 0, pins: 0, folders: 0 } : []));
});
afterEach(() => {
  unmount(component);
  vi.useRealTimers();
  vi.restoreAllMocks();
  delete (document as unknown as Record<string, unknown>).visibilityState;
  if (vsDesc) Object.defineProperty(Document.prototype, 'visibilityState', vsDesc);
});

it('offers the simple SSH setup and a local folder on Android, but not advanced SSH', async () => {
  accounts.set([{ id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set('gmail:a@b.com');
  const host = document.createElement('div');
  document.body.append(host);
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  host.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')!.click();
  flushSync();
  const labels = [...host.querySelectorAll('.account-panel-action .label')].map((e) => e.textContent?.trim());
  expect(labels).toContain('Add SSH Server');
  expect(labels).not.toContain('Add SSH Server (advanced)');
  expect(labels).toContain('Add Local Folder');
});

async function openPanel() {
  accounts.set([{ id: 'gmail:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set('gmail:a@b.com');
  const host = document.createElement('div');
  document.body.append(host);
  component = mount(Sidebar, { target: host, props: { width: 200 } });
  flushSync();
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  const openMenu = () => host.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')!.click();
  const addBtn = () => [...host.querySelectorAll<HTMLButtonElement>('.account-panel-action')]
    .find((b) => b.textContent?.includes('Add Local Folder'))!;
  openMenu();
  flushSync();
  return { host, openMenu, addBtn };
}

it('on Android, Add Local Folder uses the Android picker, never the desktop dialog', async () => {
  const { open } = await import('@tauri-apps/plugin-dialog');
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return Promise.resolve({ granted: true, supported: true });
    if (cmd === 'pick_local_folder') return Promise.resolve(null);
    if (cmd === 'count_pending_pushes') return Promise.resolve({ notes: 0, deletes: 0, pins: 0, folders: 0 });
    return Promise.resolve([]);
  });
  const { addBtn } = await openPanel();
  addBtn().click();
  for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); }
  expect(invoke).toHaveBeenCalledWith('pick_local_folder');
  expect(open).not.toHaveBeenCalled();
});

const PENDING = { notes: 0, deletes: 0, pins: 0, folders: 0 };
const flush = async () => { for (let i = 0; i < 5; i++) { await tick(); await Promise.resolve(); } };
const goVisible = () => {
  Object.defineProperty(document, 'visibilityState', { value: 'visible', configurable: true });
  document.dispatchEvent(new Event('visibilitychange'));
};

it('the busy guard itself rejects a second launch (disabled attribute removed)', async () => {
  let release!: (v: unknown) => void;
  const pending = new Promise((r) => { release = r; });
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return pending;
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { openMenu, addBtn } = await openPanel();
  addBtn().click();
  flushSync();
  openMenu();
  flushSync();
  const b = addBtn();
  expect(b.disabled).toBe(true);
  b.disabled = false; // bypass the attribute so the handler's own guard is what is tested
  b.click();
  await flush();
  expect(invoke.mock.calls.filter((c) => c[0] === 'local_folder_access')).toHaveLength(1);
  release({ granted: true, supported: false });
  vi.spyOn(window, 'alert').mockImplementation(() => {});
  await flush();
});

it('a live flow survives a visibilitychange: the plugin result arrives before the orphan timer', async () => {
  vi.useFakeTimers();
  let pick!: (v: string) => void;
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return Promise.resolve({ granted: true, supported: true });
    if (cmd === 'pick_local_folder') return new Promise((r) => { pick = r as (v: string) => void; });
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { host, openMenu, addBtn } = await openPanel();
  addBtn().click();
  await flush();
  goVisible();
  await vi.advanceTimersByTimeAsync(1500); // still inside the 2 s window
  pick('/storage/emulated/0/Notes');
  await flush();
  await vi.advanceTimersByTimeAsync(2000); // the timer fires now, and must do nothing
  await flush();
  // the flow moved on to the encryption confirm; the button stays disabled
  expect(host.querySelector('dialog[open]')).not.toBeNull();
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(true);
  // end the flow: decline the confirm
  const btns = [...host.querySelectorAll<HTMLButtonElement>('dialog[open] button')];
  (btns.find((b) => /cancel|no/i.test(b.textContent ?? '')) ?? btns[0]).click();
  await flush();
});

it('an orphaned plugin call is freed 2 s after the app becomes visible', async () => {
  vi.useFakeTimers();
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return new Promise(() => {});
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { openMenu, addBtn } = await openPanel();
  addBtn().click();
  flushSync();
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(true);
  goVisible();
  await vi.advanceTimersByTimeAsync(1000);
  expect(addBtn().disabled).toBe(true); // not yet
  await vi.advanceTimersByTimeAsync(1100);
  flushSync();
  expect(addBtn().disabled).toBe(false);
});

it('a stale flow finishing late does not clobber the newer flow', async () => {
  vi.useFakeTimers();
  let releaseOld!: (v: unknown) => void;
  let calls = 0;
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') {
      calls++;
      return calls === 1 ? new Promise((r) => { releaseOld = r; }) : new Promise(() => {});
    }
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { openMenu, addBtn } = await openPanel();
  addBtn().click();
  await flush();
  goVisible();
  await vi.advanceTimersByTimeAsync(2100); // orphan recovery frees flow 1
  openMenu();
  flushSync();
  addBtn().click(); // flow 2
  flushSync();
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(true);
  releaseOld({ granted: true, supported: true }); // flow 1 finally settles, late
  await flush();
  expect(addBtn().disabled).toBe(true); // flow 2 still owns the button
  expect(invoke).not.toHaveBeenCalledWith('pick_local_folder'); // and the stale flow did not continue
});

function dialogButton(host: HTMLElement, label: RegExp) {
  return [...host.querySelectorAll<HTMLButtonElement>('dialog[open] button')].find((b) => label.test(b.textContent ?? ''))!;
}

/** First-time grant: access not granted, Settings confirm accepted, request pending. */
function grantThenPickMocks() {
  const r: { grant?: (v: unknown) => void; pick?: (v: unknown) => void } = {};
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return Promise.resolve({ granted: false, supported: true });
    if (cmd === 'request_local_folder_access') return new Promise((res) => { r.grant = res; });
    if (cmd === 'pick_local_folder') return new Promise((res) => { r.pick = res; });
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  return r;
}

it('grant-then-pick survives: a Settings round trip does not free the picker call', async () => {
  vi.useFakeTimers();
  const r = grantThenPickMocks();
  const { host, openMenu, addBtn } = await openPanel();
  addBtn().click();
  await flush();
  dialogButton(host, /^OK$/).click(); // the All files access explanation
  await flush();
  expect(r.grant).toBeDefined();
  goVisible(); // back from Settings: arms the timer for request_local_folder_access
  await vi.advanceTimersByTimeAsync(50);
  r.grant!({ granted: true, supported: true });
  await flush();
  expect(r.pick).toBeDefined(); // the picker call started at once
  Object.defineProperty(document, 'visibilityState', { value: 'hidden', configurable: true });
  document.dispatchEvent(new Event('visibilitychange')); // picker opens: app hidden
  await vi.advanceTimersByTimeAsync(3000);
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(true);
  goVisible();
  await vi.advanceTimersByTimeAsync(500);
  r.pick!('/storage/emulated/0/Notes');
  await flush();
  await vi.advanceTimersByTimeAsync(3000);
  await flush();
  expect(dialogButton(host, /^OK$/)).toBeDefined(); // the encryption confirm: flow continued
  expect(host.querySelector('dialog[open]')?.textContent).toMatch(/not encrypted/);
  dialogButton(host, /Cancel/).click();
  await flush();
});

it('a timer armed for call N does not free call N+1 (no hidden event)', async () => {
  vi.useFakeTimers();
  const r = grantThenPickMocks();
  const { host, openMenu, addBtn } = await openPanel();
  addBtn().click();
  await flush();
  dialogButton(host, /^OK$/).click();
  await flush();
  goVisible();
  await vi.advanceTimersByTimeAsync(50);
  r.grant!({ granted: true, supported: true });
  await flush();
  await vi.advanceTimersByTimeAsync(2500); // the old timer fires while pick is pending
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(true);
  r.pick!('/storage/emulated/0/Notes');
  await flush();
  expect(host.querySelector('dialog[open]')?.textContent).toMatch(/not encrypted/); // not discarded
  dialogButton(host, /Cancel/).click();
  await flush();
});

it('a stale settle does not clear awaitingPlugin for the newer flow', async () => {
  vi.useFakeTimers();
  const pend: Array<(v: unknown) => void> = [];
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return new Promise((r) => { pend.push(r); });
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { openMenu, addBtn } = await openPanel();
  addBtn().click();
  await flush();
  goVisible();
  await vi.advanceTimersByTimeAsync(2100); // flow 1 orphaned and freed
  openMenu();
  flushSync();
  addBtn().click(); // flow 2
  await flush();
  vi.spyOn(window, 'alert').mockImplementation(() => {});
  pend[0]({ granted: true, supported: false }); // flow 1 settles late
  await flush();
  goVisible();
  await vi.advanceTimersByTimeAsync(2100); // flow 2 is orphaned too
  openMenu();
  flushSync();
  expect(addBtn().disabled).toBe(false); // fails if the stale settle cleared awaitingPlugin
});

it('removes the visibilitychange listener on destroy', async () => {
  invoke.mockImplementation((cmd: string) => {
    if (cmd === 'local_folder_access') return new Promise(() => {});
    if (cmd === 'count_pending_pushes') return Promise.resolve(PENDING);
    return Promise.resolve([]);
  });
  const { addBtn } = await openPanel();
  addBtn().click();
  await flush();
  const spy = vi.spyOn(document, 'removeEventListener');
  unmount(component);
  expect(spy).toHaveBeenCalledWith('visibilitychange', expect.any(Function));
  // afterEach unmounts again; mount a throwaway so it has something to unmount
  component = mount(Sidebar, { target: document.createElement('div'), props: { width: 200 } });
});
