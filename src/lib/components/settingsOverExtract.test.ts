// @vitest-environment jsdom
//
// R16: Extract used to hide itself (`suspended`) while the old overlay
// Settings was open, because the top layer would have covered that overlay.
// Settings is a Modal now, so it stacks ON TOP of Extract like any later
// Modal, and owns the next Android Back: Back closes Settings first (page by
// page on a phone), then Extract.
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get, type Writable } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...a: unknown[]) => invoke(...a),
  Channel: class { onmessage: (m: unknown) => void = () => {}; },
}));
vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<Record<string, unknown>>();
  return { ...orig, isAndroid: writable(true), isMacos: writable(false), supportsIcloud: writable(false) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<Record<string, unknown>>();
  return { ...orig, androidLayoutMode: writable('phone') };
});

import LessonExtractModal from './LessonExtractModal.svelte';
import Settings from './Settings.svelte';
import { settingsRoute } from '../stores/ui';
import { currentAccount, selectedFolder } from '../stores/notes';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import { handlePopState, initPhoneNavHistory, depthFromHistoryState } from '../stores/phoneNav';

const androidLayoutMode = layoutRO as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

async function settle(): Promise<void> {
  for (let i = 0; i < 12; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}
/** Until `pred` holds: each round lets one macrotask (a history popstate) land. */
async function until(pred: () => boolean): Promise<void> {
  for (let i = 0; i < 50 && !pred(); i++) {
    await new Promise((r) => setTimeout(r, 0));
    await settle();
  }
  expect(pred()).toBe(true);
}

const titles = () =>
  [...document.querySelectorAll('dialog')].filter((d) => d.open).map((d) => d.querySelector('h2')?.textContent);

describe('Settings opened from Extract (R16)', () => {
  let host: HTMLElement;
  let extract: ReturnType<typeof mount>;
  let settings: ReturnType<typeof mount>;
  let extractOpen: boolean;

  beforeEach(async () => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_stt_config') {
        return Promise.resolve({ cfg: { preset: 'custom', base_url: '', model: '', language: null, max_upload_mb: 25, confirmed_host: null }, has_api_key: null });
      }
      return Promise.resolve([]);
    });
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    window.addEventListener('popstate', handlePopState);
    settingsRoute.set(null);
    currentAccount.set('gmail:a@b.com');
    selectedFolder.set('Notes');
    host = document.createElement('div');
    document.body.append(host);
    extractOpen = true;
  });

  afterEach(async () => {
    unmount(settings);
    if (extract) unmount(extract);
    settingsRoute.set(null);
    await settle();
    await new Promise((r) => setTimeout(r, 0));
    window.removeEventListener('popstate', handlePopState);
    host.remove();
    vi.restoreAllMocks();
  });

  async function mountBoth() {
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    extract = mount(LessonExtractModal, {
      target: host,
      props: {
        get open() { return extractOpen; },
        set open(v: boolean) { extractOpen = v; },
      },
    });
    // App.svelte mounts Settings after Extract; it renders nothing until opened.
    settings = mount(Settings, { target: host });
    await settle();
  }

  it('phone: Settings stacks on Extract, and Back closes Settings first, then Extract', async () => {
    androidLayoutMode.set('phone');
    await mountBoth();
    expect(titles()).toHaveLength(1);
    const extractTitle = titles()[0];

    settingsRoute.set('speech'); // Extract's "set it up in Settings" link
    await settle();
    // Extract stays open (no longer suspended); Settings' pages are above it.
    expect(titles()).toEqual([extractTitle, 'Settings', 'Speech to text']);

    history.back();
    await until(() => get(settingsRoute) === '');
    expect(titles()).toEqual([extractTitle, 'Settings']);

    history.back();
    await until(() => get(settingsRoute) === null);
    expect(titles()).toEqual([extractTitle]);
    expect(extractOpen).toBe(true);

    history.back();
    await until(() => extractOpen === false);
  });

  it('tablet: one Settings card over Extract; Back closes it, then Extract', async () => {
    androidLayoutMode.set('tablet');
    // The tablet keeps no phone pane entries: let the rotation's one-jump
    // unwind of the last test's stack land before the modals push theirs.
    await until(() => depthFromHistoryState(history.state) === 0);
    await mountBoth();
    const extractTitle = titles()[0];

    settingsRoute.set('speech');
    await settle();
    expect(titles()).toEqual([extractTitle, 'Settings']);

    history.back();
    await until(() => get(settingsRoute) === null);
    expect(titles()).toEqual([extractTitle]);
    expect(extractOpen).toBe(true);

    history.back();
    await until(() => extractOpen === false);
  });
});
