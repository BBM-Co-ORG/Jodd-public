// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

vi.mock('../stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async () => {
  const { writable } = await import('svelte/store');
  return { androidLayoutMode: writable('phone') };
});

import WhatsNew from './WhatsNew.svelte';

const versions = [
  { version: '1.2.3', date: '2026-10-01', sections: { Added: ['Thing one', 'Thing two'], Fixed: ['A bug'] } },
];

let host: HTMLElement;
let component: ReturnType<typeof mount>;
// A plain getter/setter pair stands in for the parent's bind:open.
let state: { open: boolean; versions: typeof versions };

function render(props: { open: boolean; versions: typeof versions }) {
  state = props;
  component = mount(WhatsNew, {
    target: host,
    props: {
      get open() { return state.open; },
      set open(v: boolean) { state.open = v; },
      get versions() { return state.versions; },
    },
  });
  flushSync();
}

beforeEach(() => {
  host = document.createElement('div');
  document.body.append(host);
});
afterEach(async () => {
  await unmount(component);
  host.remove();
});

const dialog = () => host.querySelector('dialog');

it('renders nothing while closed', () => {
  render({ open: false, versions });
  expect(dialog()).toBeNull();
});

it("opens as a Modal titled What's New with every entry", () => {
  render({ open: true, versions });
  expect(dialog()!.hasAttribute('data-presentation')).toBe(true);
  expect(host.querySelector('h2')!.textContent).toBe("What's New");
  expect(host.textContent).toContain('1.2.3');
  expect(host.textContent).toContain('Thing two');
  expect(host.textContent).toContain('A bug');
});

it('says so when there are no release notes', () => {
  render({ open: true, versions: [] });
  expect(host.textContent).toContain('No release notes for this version.');
});

it('Esc closes by clearing the bindable open prop', async () => {
  render({ open: true, versions });
  dialog()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await tick();
  flushSync();
  expect(state.open).toBe(false);
});

it('keeps exactly one close control: the Modal header, not a second Close button', () => {
  render({ open: true, versions });
  const closers = [...host.querySelectorAll('button')].filter(
    (b) => b.textContent?.trim() === 'Close' || b.getAttribute('aria-label') === 'Close',
  );
  expect(closers).toHaveLength(1);
});

// Live pass 2026-10-21 (S23 FE): every changelog bullet opens with a
// **bold** lead sentence, and What's New showed the asterisks literally.
it('renders **bold** as <strong> and everything else as text', () => {
  render({
    open: true,
    versions: [
      {
        version: '9.9.9',
        date: null,
        sections: { Fixed: ['**Back works again.** Even with <b>tags</b> & a stray ** in the text.'] },
      },
    ] as unknown as typeof versions,
  });
  const li = document.querySelector('.wn-version li')!;
  const strong = li.querySelector('strong');
  expect(strong?.textContent).toBe('Back works again.');
  // No markup from the changelog reaches the DOM: <b> stays text.
  expect(li.querySelector('b')).toBeNull();
  expect(li.textContent).toBe('Back works again. Even with <b>tags</b> & a stray ** in the text.');
  expect(li.querySelectorAll('strong')).toHaveLength(1);
});

// Tab S7 (2026-10-21): inline code showed its backticks. It renders as
// <code>, as text — no {@html} on changelog text.
it('renders `code` as <code>, escaped', () => {
  const entry = { version: '1.2.4', date: null, sections: { Fixed: ['Type `**` or `<b>x</b>` here'] } };
  render({ open: true, versions: [entry] as unknown as typeof versions });
  const codes = [...host.querySelectorAll('li code')].map((c) => c.textContent);
  expect(codes).toEqual(['**', '<b>x</b>']);
  expect(host.querySelector('li b')).toBeNull();
  expect(host.querySelector('li')!.textContent).toBe('Type ** or <b>x</b> here');
});
