// @vitest-environment jsdom
//
// Settings' Copy buttons with the Tauri webview's clipboard refusing
// (`navigator.clipboard.writeText` rejects there — live run, AccountPage).
// Each goes through `copyText`, so the fallback still copies; when both paths
// fail the page says so and never shows "Copied".
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '../__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { stubWebviewClipboard, type ClipboardStub } from '../__fixtures__/clipboardStub';

const LOG_PATH = '/data/user/0/app.jodd/files/jodd.log';
const CENSUS = 'census: 3 notes, 2 writable';

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'get_log_settings':
      return { file_logging_enabled: true, log_file_path: LOG_PATH, log_file_size_bytes: 10 };
    case 'get_oauth_settings': return { client_id: '' };
    case 'get_ms_oauth_config': return { client_id: '', credentials_available: false };
    case 'icloud_census': return CENSUS;
    default: return [];
  }
});
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, args?: unknown) => invoke(cmd, args),
  Channel: class { onmessage: unknown = null; },
}));
vi.mock('@tauri-apps/api/event', () => ({ listen: async () => () => {}, emit: async () => {} }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn(), revealItemInDir: vi.fn() }));
vi.mock('../../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../../stores/platform')>();
  return { ...orig, isAndroid: writable(false), isMacos: writable(true) };
});

import SharePage from './SharePage.svelte';
import AdvancedPage from './AdvancedPage.svelte';
import AccountPage from './AccountPage.svelte';
import { isAndroid as isAndroidReadonly } from '../../stores/platform';
import { BOOKMARKLET } from '../../capture';
import { accounts } from '../../stores/notes';
import { dupStats } from '../../stores/dupStats';
import type { Writable } from 'svelte/store';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;
let clip: ClipboardStub | undefined;

async function settle(rounds = 6) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}
const button = (label: string) => {
  const b = Array.from(host.querySelectorAll('button')).find((x) => x.textContent!.trim() === label);
  expect(b, `button "${label}"`).toBeTruthy();
  return b!;
};

beforeEach(() => {
  invoke.mockClear();
  isAndroid.set(false);
  dupStats.set({});
  host = document.createElement('div');
  document.body.appendChild(host);
});
afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  clip?.restore();
  clip = undefined;
  host.remove();
});

describe('Settings › Share to Jodd — Copy bookmarklet', () => {
  beforeEach(async () => {
    comp = mount(SharePage, { target: host });
    await settle();
  });

  it('navigator.clipboard rejecting still copies, and confirms', async () => {
    clip = stubWebviewClipboard('rejects');
    button('Copy bookmarklet').click();
    await vi.waitFor(() => expect(button('Copied')).toBeTruthy());
    expect(clip.copied).toEqual([BOOKMARKLET]);
    expect(host.querySelector('[role="alert"]')).toBeNull();
  });

  it('both paths failing says so, and never shows Copied', async () => {
    clip = stubWebviewClipboard('fails');
    button('Copy bookmarklet').click();
    await vi.waitFor(() => expect(host.querySelector('[role="alert"]')).not.toBeNull());
    expect(host.querySelector('[role="alert"]')!.textContent).toContain('Could not copy');
    expect(host.textContent).not.toContain('Copied');
  });
});

describe('Settings › Advanced — Copy log path (Android)', () => {
  beforeEach(async () => {
    isAndroid.set(true);
    comp = mount(AdvancedPage, { target: host, props: {} });
    await vi.waitFor(() => expect(host.querySelector('.log-path')?.textContent).toBe(LOG_PATH));
    await settle();
  });

  it('navigator.clipboard rejecting still copies the path, with no error', async () => {
    clip = stubWebviewClipboard('rejects');
    button('Copy log path').click();
    await vi.waitFor(() => expect(clip!.copied).toEqual([LOG_PATH]));
    await settle();
    expect(host.querySelector('p.error')).toBeNull();
  });

  it('both paths failing shows an error', async () => {
    clip = stubWebviewClipboard('fails');
    button('Copy log path').click();
    await vi.waitFor(() =>
      expect(host.querySelector('p.error')?.textContent).toContain('Could not copy the log path'),
    );
  });
});

describe('Settings › Accounts › iCloud — Copy report (census)', () => {
  const ICLOUD = {
    id: 'icloud:a@b.com', email: 'a@b.com', added_at: '2026-01-01T00:00:00Z',
    status: 'active', backend_kind: 'icloud',
  };
  beforeEach(async () => {
    accounts.set([ICLOUD as never]);
    comp = mount(AccountPage, {
      target: host,
      props: { accountId: ICLOUD.id, leave: async () => true },
    });
    await vi.waitFor(() => expect(host.querySelector('.loading')).toBeNull());
    await settle();
    button('Check what Jodd can write here').click();
    await vi.waitFor(() => expect(host.querySelector('pre.census-out')?.textContent).toBe(CENSUS));
    await settle();
  });

  it('navigator.clipboard rejecting still copies the report, and confirms', async () => {
    clip = stubWebviewClipboard('rejects');
    button('Copy report').click();
    await vi.waitFor(() => expect(button('Copied')).toBeTruthy());
    expect(clip.copied).toEqual([CENSUS]);
    expect(host.querySelector('[role="alert"]')).toBeNull();
  });

  it('both paths failing says so, and never shows Copied', async () => {
    clip = stubWebviewClipboard('fails');
    button('Copy report').click();
    await vi.waitFor(() => expect(host.querySelector('[role="alert"]')).not.toBeNull());
    expect(host.querySelector('[role="alert"]')!.textContent).toContain('Could not copy');
    expect(button('Copy report')).toBeTruthy();
    expect(host.textContent).not.toContain('Copied');
  });
});
