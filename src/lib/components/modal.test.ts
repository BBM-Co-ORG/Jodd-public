// @vitest-environment jsdom
import './__fixtures__/dialogStub';
// jsdom has no showModal/close, no inertness and no native focus traversal;
// these tests assert Modal's own routing (close paths, the dirty guard,
// presentation choice and its history entries), not browser behaviour.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, createRawSnippet } from 'svelte';
import type { Writable } from 'svelte/store';

// The presentation gate is `$isAndroid && $androidLayoutMode === 'phone'`.
// Both are replaced with plain writables the tests flip directly.
vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import Modal from './Modal.svelte';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import { androidLayoutMode as layoutReadonly } from '../stores/viewport';
import {
  handlePopState,
  initPhoneNavHistory,
  depthFromHistoryState,
  overlayFromHistoryState,
  navigateToPane,
  activePane,
} from '../stores/phoneNav';
import { get } from 'svelte/store';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const androidLayoutMode = layoutReadonly as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error('waitFor: condition did not become true in time');
    await new Promise((r) => setTimeout(r, 10));
  }
}
const settle = () => new Promise((r) => setTimeout(r, 30));

const body = createRawSnippet(() => ({ render: () => '<p class="fixture-body">Body</p>' }));

let host: HTMLElement;
let opener: HTMLButtonElement;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let instance: any = null;
let onclose: ReturnType<typeof vi.fn<() => void>>;

function dispose() {
  if (instance) {
    const i = instance;
    instance = null;
    unmount(i);
  }
}

/** Mounts a Modal whose parent behaves like a real caller: it unmounts on onclose. */
function open(props: Record<string, unknown> = {}) {
  onclose = vi.fn<() => void>(() => queueMicrotask(dispose));
  instance = mount(Modal, {
    target: host,
    props: { title: 'Fixture settings', onclose, children: body, ...props },
  });
  flushSync();
  return instance;
}

const dialogs = () => [...document.querySelectorAll('dialog')] as HTMLDialogElement[];
const outer = () => dialogs()[0];
const guard = () => dialogs().find((d) => d.textContent?.includes('Discard unsaved changes?'));
function buttonIn(root: Element | undefined, text: string): HTMLButtonElement {
  const b = [...(root?.querySelectorAll('button') ?? [])].find((x) => x.textContent?.trim() === text);
  if (!b) throw new Error(`no button "${text}"`);
  return b as HTMLButtonElement;
}
function key(target: Element, value: string) {
  const event = new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true });
  target.dispatchEvent(event);
  return event;
}

beforeEach(() => {
  isAndroid.set(false);
  androidLayoutMode.set('phone');
  // Android modals push entries in every layout; their popstates must reach
  // phoneNav or its in-flight bookkeeping would leak into the next test.
  window.addEventListener('popstate', handlePopState);
  opener = document.createElement('button');
  opener.textContent = 'Open';
  document.body.append(opener);
  opener.focus();
  host = document.createElement('div');
  document.body.append(host);
});
afterEach(async () => {
  dispose();
  flushSync();
  await settle();
  window.removeEventListener('popstate', handlePopState);
  host.remove();
  opener.remove();
});

describe('Modal — close paths', () => {
  it('returns focus to the opener on close', async () => {
    open();
    expect(outer().open).toBe(true);
    key(outer(), 'Escape');
    expect(onclose).toHaveBeenCalledTimes(1);
    await waitFor(() => instance === null);
    expect(document.activeElement).toBe(opener);
  });

  it('Esc and backdrop both route through requestClose', async () => {
    open();
    const parent = vi.fn();
    window.addEventListener('keydown', parent);
    const esc = key(outer(), 'Escape');
    window.removeEventListener('keydown', parent);
    expect(esc.defaultPrevented).toBe(true);
    expect(parent).not.toHaveBeenCalled(); // app shortcuts never see a modal's keys
    expect(onclose).toHaveBeenCalledTimes(1);
    await waitFor(() => instance === null);

    open();
    outer().dispatchEvent(new MouseEvent('click', { bubbles: true })); // target === the dialog = backdrop
    expect(onclose).toHaveBeenCalledTimes(1);
    await waitFor(() => instance === null);

    // A click inside the panel is not a backdrop click.
    open();
    (outer().querySelector('.fixture-body') as HTMLElement).click();
    expect(onclose).not.toHaveBeenCalled();
    // The exported method is the same single path.
    await expect(instance.requestClose()).resolves.toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
    // Settle-once: a second request does not call onclose again.
    await instance?.requestClose?.();
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('the native cancel event is owned and routed through requestClose', () => {
    open();
    const ev = new Event('cancel', { cancelable: true });
    outer().dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});

describe('Modal — dirty guard', () => {
  it('dirty: Esc opens "Discard unsaved changes?" with Keep editing focused', () => {
    open({ dirty: true });
    key(outer(), 'Escape');
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    const g = guard();
    expect(g).toBeTruthy();
    expect(g!.open).toBe(true);
    expect(document.activeElement).toBe(buttonIn(g, 'Keep editing'));
    expect(buttonIn(g, 'Discard')).toBeTruthy();
    // No onsave → no Save button.
    expect([...g!.querySelectorAll('button')].some((b) => b.textContent?.trim() === 'Save')).toBe(false);
    // The guard is an alert: on desktop that is a card.
    expect(g!.dataset.presentation).toBe('card');
  });

  it('dirtyMessage replaces the guard message; the default is unchanged', () => {
    open({ dirty: true, dirtyMessage: 'Setup is running. Cancel it?' });
    key(outer(), 'Escape');
    flushSync();
    const g = dialogs()[1];
    expect(g.querySelector('h2')!.textContent).toBe('Setup is running. Cancel it?');
    expect(g.textContent).not.toContain('Discard unsaved changes?');
    expect(g.getAttribute('aria-labelledby')).toBe(g.querySelector('h2')!.id);
    // Same buttons: Discard still closes, Keep editing is still focused.
    expect(document.activeElement).toBe(buttonIn(g, 'Keep editing'));
    buttonIn(g, 'Discard').click();
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('dirty + Discard closes; Keep editing keeps it open', async () => {
    open({ dirty: true });
    let result = instance.requestClose();
    flushSync();
    buttonIn(guard(), 'Keep editing').click();
    flushSync();
    await expect(result).resolves.toBe(false);
    expect(guard()).toBeUndefined();
    expect(outer().open).toBe(true);
    expect(onclose).not.toHaveBeenCalled();

    result = instance.requestClose();
    flushSync();
    buttonIn(guard(), 'Discard').click();
    await expect(result).resolves.toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('dirty + Save that rejects keeps it open and shows the error', async () => {
    let fail = true;
    const onsave = vi.fn(async () => {
      if (fail) throw new Error('Disk is read-only');
    });
    open({ dirty: true, onsave });
    let settled = false;
    const result = instance.requestClose().then((v: boolean) => { settled = true; return v; });
    flushSync();
    buttonIn(guard(), 'Save').click();
    await waitFor(() => onsave.mock.calls.length === 1);
    await settle();
    flushSync();
    expect(settled).toBe(false);
    expect(onclose).not.toHaveBeenCalled();
    expect(outer().open).toBe(true);
    expect(guard()?.textContent).toContain('Disk is read-only');
    // A retry that succeeds closes.
    fail = false;
    buttonIn(guard(), 'Save').click();
    await expect(result).resolves.toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});

describe('Modal — Tab trap', () => {
  const field = createRawSnippet(() => ({
    render: () =>
      '<div><input class="a" /><button type="button" class="b">B</button><button type="button" class="off" disabled>Off</button><span hidden><button type="button" class="gone">Gone</button></span></div>',
  }));

  it('wraps Tab from the last tabbable to the first and Shift+Tab back', () => {
    open({ children: field, size: 'md' });
    const items = [...outer().querySelectorAll<HTMLElement>('button, input')].filter(
      (el) => !el.matches(':disabled') && !el.closest('[hidden]'),
    );
    const first = items[0];
    const last = items[items.length - 1];
    last.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    last.dispatchEvent(fwd);
    expect(fwd.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);
    const back = new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true });
    first.dispatchEvent(back);
    expect(back.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(last);
  });

  it('leaves Tab between two tabbables to the browser', () => {
    open({ children: field });
    const a = outer().querySelector<HTMLElement>('.a')!;
    a.focus();
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    a.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
  });

  it('skips disabled and hidden controls when finding the ends', () => {
    open({ children: field });
    const items = [...outer().querySelectorAll<HTMLElement>('button, input')];
    expect(items.some((el) => el.classList.contains('off'))).toBe(true);
    const b = outer().querySelector<HTMLElement>('.b')!;
    b.focus();
    b.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));
    // .b is last among the enabled, visible ones (the close button is first).
    expect(document.activeElement).toBe(outer().querySelector('.close'));
  });

  it('wraps to and from a <summary> as the last control', () => {
    const details = createRawSnippet(() => ({
      render: () =>
        '<div><button type="button" class="b">B</button><details><summary class="sum">More</summary><p>Hidden text</p></details></div>',
    }));
    open({ children: details });
    const sum = outer().querySelector<HTMLElement>('.sum')!;
    const close = outer().querySelector<HTMLElement>('.close')!;
    sum.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    sum.dispatchEvent(fwd);
    expect(fwd.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(close);
    const back = new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true });
    close.dispatchEvent(back);
    expect(back.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(sum);
  });

  // R11: Settings pages hold <details> (Learn more, AI limits, receipts). A
  // closed one renders nothing but its <summary>, so a control inside it is
  // not a Tab stop; treating it as the last one left Tab from the summary to
  // the browser instead of wrapping.
  it('skips controls inside a closed <details>, but not its own <summary>', () => {
    const closed = createRawSnippet(() => ({
      render: () =>
        '<div><button type="button" class="b">B</button><details><summary class="sum">More</summary><button type="button" class="inner">Inner</button><details open><summary class="nested-sum">Nested</summary><input class="nested-in" /></details></details></div>',
    }));
    open({ children: closed });
    const sum = outer().querySelector<HTMLElement>('.sum')!;
    sum.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    sum.dispatchEvent(fwd);
    expect(fwd.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(outer().querySelector('.close'));
    // Shift+Tab from the first control wraps to the summary, not into the body.
    const close = outer().querySelector<HTMLElement>('.close')!;
    const back = new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true, bubbles: true, cancelable: true });
    close.dispatchEvent(back);
    expect(document.activeElement).toBe(sum);
  });

  it('counts controls inside an open <details>', () => {
    const opened = createRawSnippet(() => ({
      render: () =>
        '<div><details open><summary class="sum">More</summary><button type="button" class="inner">Inner</button></details></div>',
    }));
    open({ children: opened });
    const sum = outer().querySelector<HTMLElement>('.sum')!;
    sum.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    sum.dispatchEvent(fwd);
    expect(fwd.defaultPrevented).toBe(false); // .inner comes next: the browser's move
    const inner = outer().querySelector<HTMLElement>('.inner')!;
    inner.focus();
    const last = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    inner.dispatchEvent(last);
    expect(last.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(outer().querySelector('.close'));
  });

  it('a contenteditable counts as the last control; contenteditable="false" does not', () => {
    const editable = createRawSnippet(() => ({
      render: () =>
        '<div><div class="ce" contenteditable="true" tabindex="0">x</div><div class="no" contenteditable="false">y</div></div>',
    }));
    open({ children: editable });
    const ce = outer().querySelector<HTMLElement>('.ce')!;
    ce.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    ce.dispatchEvent(fwd);
    expect(fwd.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(outer().querySelector('.close'));
  });

  it('skips a tabindex="-1" element when finding the ends', () => {
    const skipped = createRawSnippet(() => ({
      render: () =>
        '<div><button type="button" class="b">B</button><button type="button" class="neg" tabindex="-1">Neg</button><div class="neg2" tabindex="-1">Neg2</div></div>',
    }));
    open({ children: skipped });
    const b = outer().querySelector<HTMLElement>('.b')!;
    b.focus();
    const fwd = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    b.dispatchEvent(fwd);
    // .b is the last tabbable (the negative-tabindex ones are not), so Tab wraps to Close.
    expect(fwd.defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(outer().querySelector('.close'));
  });

  it('the discard alert traps within itself', async () => {
    open({ dirty: true });
    void instance.requestClose();
    flushSync();
    const g = guard()!;
    const keep = buttonIn(g, 'Keep editing');
    const discard = buttonIn(g, 'Discard');
    expect(document.activeElement).toBe(keep);
    const ev = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    keep.dispatchEvent(ev); // Keep editing is the last control in the alert
    expect(ev.defaultPrevented).toBe(true);
    expect(g.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).toBe(discard);
    keep.focus();
  });
});

describe('Modal — presentation', () => {
  it('card on desktop even at 500px width', () => {
    isAndroid.set(false);
    androidLayoutMode.set('phone'); // a 500px desktop window classifies as phone width
    open({ phone: 'sheet' });
    expect(outer().dataset.presentation).toBe('card');
  });

  it('page/sheet/alert only when isAndroid && phone', () => {
    isAndroid.set(true);
    for (const phone of ['page', 'sheet', 'alert'] as const) {
      androidLayoutMode.set('phone');
      open({ phone });
      expect(outer().dataset.presentation).toBe(phone);
      androidLayoutMode.set('tablet');
      flushSync();
      expect(outer().dataset.presentation).toBe('card');
      dispose();
      flushSync();
    }
    androidLayoutMode.set('phone');
    open(); // default phone presentation is a page
    expect(outer().dataset.presentation).toBe('page');
  });

  it('phone page: ‹ parent label closes, primary sits in the header', () => {
    isAndroid.set(true);
    const onclick = vi.fn();
    open({ parentLabel: 'Notes', primary: { label: 'Done', onclick } });
    const head = outer().querySelector('header')!;
    buttonIn(head, 'Done').click();
    expect(onclick).toHaveBeenCalledTimes(1);
    buttonIn(head, '‹ Notes').click();
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('card: primary sits in the footer and honours disabled/busy', () => {
    const onclick = vi.fn();
    open({ primary: { label: 'Save', onclick, busy: true } });
    const foot = outer().querySelector('footer')!;
    const b = buttonIn(foot, 'Save');
    expect(b.disabled).toBe(true);
    expect(b.getAttribute('aria-busy')).toBe('true');
  });

  it('sheet: a drag down the grab handle past 80px closes through requestClose', () => {
    isAndroid.set(true);
    open({ phone: 'sheet', dirty: true });
    const handle = outer().querySelector('.grab') as HTMLElement;
    const ptr = (type: string, clientY: number) =>
      handle.dispatchEvent(new MouseEvent(type, { clientY, bubbles: true }));
    ptr('pointerdown', 100); ptr('pointermove', 150); ptr('pointerup', 150); // 50px: stays
    flushSync();
    expect(guard()).toBeUndefined();
    ptr('pointerdown', 100); ptr('pointermove', 200); ptr('pointerup', 200); // 100px: asks
    flushSync();
    expect(guard()).toBeTruthy(); // dirty → went through the guard, not straight to onclose
    expect(onclose).not.toHaveBeenCalled();
  });
});

describe('Modal — locked', () => {
  it('refuses Esc, the native cancel, the backdrop and the close button, with no guard', async () => {
    open({ locked: true, dirty: true });
    key(outer(), 'Escape');
    const ev = new Event('cancel', { cancelable: true });
    outer().dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    outer().click(); // backdrop: the click lands on the <dialog> itself
    const close = outer().querySelector<HTMLButtonElement>('button[aria-label="Close"]')!;
    expect(close.getAttribute('aria-disabled')).toBe('true');
    close.click();
    flushSync();
    await expect(instance.requestClose()).resolves.toBe(false);
    expect(onclose).not.toHaveBeenCalled();
    expect(guard()).toBeUndefined();
    expect(outer().open).toBe(true);
  });

  it('closeInto is refused while locked', () => {
    open({ locked: true });
    expect(instance.closeInto('note')).toBe(false);
    expect(onclose).not.toHaveBeenCalled();
  });
});

describe('Modal — onbeforeclose', () => {
  it('runs first on every close path; false keeps it open with no guard', async () => {
    const before = vi.fn(() => false);
    open({ onbeforeclose: before, dirty: true });
    key(outer(), 'Escape');
    outer().dispatchEvent(new Event('cancel', { cancelable: true }));
    outer().click(); // backdrop
    outer().querySelector<HTMLButtonElement>('button[aria-label="Close"]')!.click();
    flushSync();
    await expect(instance.requestClose()).resolves.toBe(false);
    expect(before).toHaveBeenCalledTimes(5);
    expect(onclose).not.toHaveBeenCalled();
    expect(guard()).toBeUndefined();
    expect(outer().open).toBe(true);
  });

  it('true lets the close go on, through the dirty guard', () => {
    const before = vi.fn(() => true);
    open({ onbeforeclose: before, dirty: true });
    key(outer(), 'Escape');
    flushSync();
    expect(before).toHaveBeenCalledTimes(1);
    expect(guard()).toBeTruthy();
    // A second request while the guard asks shares its answer, unasked.
    key(outer(), 'Escape');
    expect(before).toHaveBeenCalledTimes(1);
    buttonIn(guard(), 'Discard').click();
    flushSync();
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('is not asked by closeInto: the navigating action is the decision', () => {
    const before = vi.fn(() => false);
    open({ onbeforeclose: before });
    expect(instance.closeInto('note')).toBe(true);
    expect(before).not.toHaveBeenCalled();
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});

// Settings switches pages inside one modal; leaving a dirty page goes
// through the same guard as closing, but the modal stays open.
describe('Modal — guard Save that outlives its decision', () => {
  it('slow onsave → Keep editing mid-save → onsave resolves → the modal stays open', async () => {
    let finishSave!: () => void;
    const onsave = vi.fn(() => new Promise<void>((r) => { finishSave = r; }));
    open({ dirty: true, onsave });
    const closing = instance.requestClose();
    flushSync();
    buttonIn(guard(), 'Save').click();
    flushSync();
    expect(onsave).toHaveBeenCalledTimes(1);
    buttonIn(guard(), 'Keep editing').click();
    await expect(closing).resolves.toBe(false);
    finishSave();
    await settle();
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    expect(outer().open).toBe(true);
    expect(guard()).toBeUndefined();
  });

  it('a stale failure does not put an error on the next guard', async () => {
    let failSave!: (e: Error) => void;
    const onsave = vi.fn()
      .mockImplementationOnce(() => new Promise<void>((_, rej) => { failSave = rej; }))
      .mockImplementation(() => Promise.resolve());
    open({ dirty: true, onsave });
    void instance.requestClose();
    flushSync();
    buttonIn(guard(), 'Save').click();
    flushSync();
    buttonIn(guard(), 'Keep editing').click();
    await settle();
    void instance.requestClose(); // a new decision
    flushSync();
    failSave(new Error('late failure'));
    await settle();
    flushSync();
    expect(guard()!.textContent).not.toContain('late failure');
    expect(onclose).not.toHaveBeenCalled();
  });
});

describe('Modal — confirmLeave', () => {
  it('clean: resolves true with no guard', async () => {
    open();
    await expect(instance.confirmLeave()).resolves.toBe(true);
    expect(guard()).toBeUndefined();
    expect(onclose).not.toHaveBeenCalled();
  });

  it('dirty + Discard: resolves true and the modal stays open', async () => {
    open({ dirty: true });
    const p = instance.confirmLeave();
    flushSync();
    buttonIn(guard(), 'Discard').click();
    await expect(p).resolves.toBe(true);
    flushSync();
    expect(guard()).toBeUndefined();
    expect(outer().open).toBe(true);
    expect(onclose).not.toHaveBeenCalled();
  });

  it('dirty + Keep editing: resolves false', async () => {
    open({ dirty: true });
    const p = instance.confirmLeave();
    flushSync();
    buttonIn(guard(), 'Keep editing').click();
    await expect(p).resolves.toBe(false);
    expect(onclose).not.toHaveBeenCalled();
  });

  it('dirty + Save: runs onsave, resolves true, stays open', async () => {
    const onsave = vi.fn(() => Promise.resolve());
    open({ dirty: true, onsave });
    const p = instance.confirmLeave();
    flushSync();
    buttonIn(guard(), 'Save').click();
    await expect(p).resolves.toBe(true);
    expect(onsave).toHaveBeenCalledTimes(1);
    expect(onclose).not.toHaveBeenCalled();
  });

  it('a close request joining a pending leave turns it into a close', async () => {
    open({ dirty: true });
    const leave = instance.confirmLeave();
    const close = instance.requestClose();
    expect(close).toBe(leave);
    flushSync();
    buttonIn(guard(), 'Discard').click();
    await expect(close).resolves.toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});

describe('Modal — Android Back', () => {
  beforeEach(() => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    window.addEventListener('popstate', handlePopState);
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
  });
  afterEach(async () => {
    dispose();
    flushSync();
    await settle();
    window.removeEventListener('popstate', handlePopState);
  });

  it('phone: opening pushes {overlay}, depth+1', () => {
    navigateToPane('note');
    const before = depthFromHistoryState(history.state);
    open();
    expect(depthFromHistoryState(history.state)).toBe(before + 1);
    expect(overlayFromHistoryState(history.state)).toBeTruthy();
    expect((history.state as { pane: string }).pane).toBe('note');
  });

  it('desktop pushes nothing', () => {
    isAndroid.set(false);
    const state = history.state;
    open();
    expect(history.state).toBe(state);
  });

  it('tablet pushes an entry too (R7): Back closes the modal first', async () => {
    androidLayoutMode.set('tablet');
    // The tablet keeps no pane entries: the cold-start list's goes first.
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await settle();
    const before = depthFromHistoryState(history.state);
    open();
    expect(depthFromHistoryState(history.state)).toBe(before + 1);
    expect(overlayFromHistoryState(history.state)).toBeTruthy();
    history.back();
    await waitFor(() => onclose.mock.calls.length === 1);
    await settle();
    expect(depthFromHistoryState(history.state)).toBe(before);
    expect(overlayFromHistoryState(history.state)).toBeNull();
  });

  it('opened on tablet, rotated to phone: Back closes the modal, the pane is unchanged', async () => {
    androidLayoutMode.set('tablet');
    open();
    androidLayoutMode.set('phone');
    flushSync();
    expect(outer().dataset.presentation).toBe('page');
    history.back();
    await waitFor(() => onclose.mock.calls.length === 1);
    await settle();
    expect(get(activePane)).toBe('list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
  });

  it('closing from the UI and navigating in the same turn lands on the new pane', async () => {
    open(); // cold-start {list,1} → overlay at depth 2
    key(outer(), 'Escape'); // popOverlay: back in flight
    navigateToPane('note'); // e.g. App reacting to $selectedNote in the same turn
    expect(get(activePane)).toBe('note'); // shown at once, not after the back
    await waitFor(() => depthFromHistoryState(history.state) === 2 && overlayFromHistoryState(history.state) === null);
    await settle();
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('phone: popstate of the overlay entry closes the modal', async () => {
    open();
    history.back();
    await waitFor(() => onclose.mock.calls.length === 1);
    await settle();
    expect(depthFromHistoryState(history.state)).toBe(1);
  });

  it('phone: closing from the UI goes back exactly once', async () => {
    open();
    expect(depthFromHistoryState(history.state)).toBe(2);
    key(outer(), 'Escape');
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await settle();
    expect(depthFromHistoryState(history.state)).toBe(1);
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('phone: locked refuses Back → entry re-pushed, modal stays', async () => {
    open({ locked: true });
    const id = overlayFromHistoryState(history.state);
    const popped = new Promise<void>((resolve) => window.addEventListener('popstate', () => resolve(), { once: true }));
    history.back();
    await popped;
    await vi.waitFor(() => expect(overlayFromHistoryState(history.state)).toBe(id));
    expect(depthFromHistoryState(history.state)).toBe(2);
    expect(outer().open).toBe(true);
    expect(guard()).toBeUndefined();
    expect(onclose).not.toHaveBeenCalled();
  });

  it('phone: locked refuses the page ‹ Back button', () => {
    open({ locked: true });
    const back = outer().querySelector<HTMLButtonElement>('button.back')!;
    expect(back.getAttribute('aria-disabled')).toBe('true');
    back.click();
    flushSync();
    expect(onclose).not.toHaveBeenCalled();
    expect(outer().open).toBe(true);
  });

  it('phone: guard keeps it open → entry re-pushed', async () => {
    open({ dirty: true });
    const id = overlayFromHistoryState(history.state);
    history.back();
    await waitFor(() => guard() !== undefined);
    // The nested alert owns its own entry above the root.
    expect(depthFromHistoryState(history.state)).toBe(2);
    expect(guard()!.dataset.presentation).toBe('alert');
    buttonIn(guard(), 'Keep editing').click();
    await waitFor(() => overlayFromHistoryState(history.state) === id);
    await settle();
    expect(overlayFromHistoryState(history.state)).toBe(id);
    expect(depthFromHistoryState(history.state)).toBe(2);
    expect(outer().open).toBe(true);
    expect(onclose).not.toHaveBeenCalled();
  });

  it('phone: nested page → discard alert → Discard unwinds both entries', async () => {
    open({ dirty: true });
    key(outer(), 'Escape');
    flushSync();
    expect(depthFromHistoryState(history.state)).toBe(3); // page + its alert
    buttonIn(guard(), 'Discard').click();
    await waitFor(() => onclose.mock.calls.length === 1);
    await waitFor(() => depthFromHistoryState(history.state) === 1);
    await settle();
    expect(depthFromHistoryState(history.state)).toBe(1);
  });

  it('rotation to tablet with modal open: still open, Back still closes', async () => {
    open();
    expect(depthFromHistoryState(history.state)).toBe(2);
    androidLayoutMode.set('tablet');
    flushSync();
    expect(outer().open).toBe(true);
    expect(outer().dataset.presentation).toBe('card');
    history.back();
    await waitFor(() => onclose.mock.calls.length === 1);
    // Closed, not refused: nothing was pushed back. And the phone list
    // entry beneath goes too — on the tablet the next Back must background
    // the app, not pop a pane entry it cannot show (live pass 2026-10-21).
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await settle();
    expect(depthFromHistoryState(history.state)).toBe(0);
    expect(overlayFromHistoryState(history.state)).toBeNull();
  });

  it('rotation to tablet, then closing from the UI still removes the entry', async () => {
    open();
    androidLayoutMode.set('tablet');
    flushSync();
    key(outer(), 'Escape');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await settle();
    expect(overlayFromHistoryState(history.state)).toBeNull();
  });

  it('opened over a note, rotated to tablet and back: Back closes it, then Back reaches the list', async () => {
    navigateToPane('note'); // depth 2
    open(); // depth 3
    androidLayoutMode.set('tablet');
    flushSync();
    await settle();
    androidLayoutMode.set('phone');
    flushSync();
    await settle();
    expect(outer().open).toBe(true);
    history.back();
    await waitFor(() => onclose.mock.calls.length === 1);
    await settle();
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
    history.back();
    await waitFor(() => get(activePane) === 'list');
    expect(history.state).toEqual({ pane: 'list', depth: 1 });
  });

  it('closed on the tablet, then rotated to phone: the stack is rebuilt for the pane', async () => {
    navigateToPane('note'); // depth 2
    open(); // depth 3
    androidLayoutMode.set('tablet');
    flushSync();
    key(outer(), 'Escape');
    await waitFor(() => depthFromHistoryState(history.state) === 0);
    await settle();
    androidLayoutMode.set('phone');
    flushSync();
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(get(activePane)).toBe('note');
  });

  it('closeInto(pane) turns the entry into the destination without a back', () => {
    navigateToPane('list'); // no-op: the cold-start list (depth 1) is active
    open();
    const length = history.length;
    expect(instance.closeInto('note')).toBe(true);
    expect(onclose).toHaveBeenCalledTimes(1);
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
    expect(history.length).toBe(length);
  });

  it('closeInto is refused while the dirty guard is open', async () => {
    open({ dirty: true });
    key(outer(), 'Escape');
    flushSync();
    expect(depthFromHistoryState(history.state)).toBe(3); // page + alert
    const state = history.state;
    expect(instance.closeInto('note')).toBe(false);
    expect(onclose).not.toHaveBeenCalled();
    expect(history.state).toBe(state);
    expect(guard()).toBeTruthy();
    // Settling the guard first makes it possible again.
    buttonIn(guard(), 'Keep editing').click();
    await waitFor(() => depthFromHistoryState(history.state) === 2);
    await settle();
    expect(instance.closeInto('note')).toBe(true);
    expect(history.state).toEqual({ pane: 'note', depth: 2 });
  });
});
