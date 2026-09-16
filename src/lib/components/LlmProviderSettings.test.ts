// @vitest-environment jsdom
//
// Regression guard for the failed-test-result template branches.
//
// `testConnection()`'s result renders two different ways depending on
// whether `cause` is set: when a failure is RECOGNISED, the raw `error` /
// `raw_head` go inside a <details> disclosure, alongside the named cause and
// action. When NOTHING recognises the failure, the raw evidence is all the
// user has, and it must render EXPANDED — no disclosure to hide it behind.
// A flipped {#if} would collapse an unexplained error behind a click and
// look perfectly fine on screen; this test pins both branches so that bug
// cannot land silently.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import LlmProviderSettings from './LlmProviderSettings.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

const EMPTY_CFG = {
  provider: 'none',
  http_base_url: null,
  http_model: null,
  http_api_key_keychain: null,
  agent_preset: null,
  agent_custom: null,
};

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(LlmProviderSettings, { target, props: { accountId: 'acct@x' } });
  flushSync();
  return { target, host };
}

function button(target: HTMLElement, label: string): HTMLButtonElement {
  const found = [...target.querySelectorAll('button')].find((b) =>
    (b.textContent ?? '').includes(label),
  );
  if (!found) throw new Error(`no button matching ${label}: ${target.innerHTML}`);
  return found as HTMLButtonElement;
}

// Drains the microtask queue enough for onMount's load() and testConnection's
// awaited invoke to settle, mirroring ReindexRecoveryBanner.test.ts's pattern.
async function settle() {
  for (let i = 0; i < 10; i++) await tick();
  flushSync();
}

describe('LlmProviderSettings — failed test-result rendering', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  it('cause set: raw evidence is inside a <details> disclosure', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name':
          return Promise.resolve('macos');
        case 'list_agent_cli_presets':
          return Promise.resolve([]);
        case 'get_llm_settings':
          return Promise.resolve(EMPTY_CFG);
        case 'test_llm_provider':
          return Promise.resolve({
            ok: false,
            elapsed_ms: 42,
            error: 'exit status: 1: structured_output_retry_exhausted',
            raw_head: 'raw evidence marker',
            cause: 'the model kept answering in the wrong shape',
            action: 'try a different CLI',
          });
        default:
          throw new Error(`unexpected command ${cmd}`);
      }
    });

    const { target, host } = render();
    await settle();

    button(target, 'Test connection').click();
    await settle();

    const details = target.querySelector('details');
    expect(details).not.toBeNull();
    expect(details!.textContent).toContain('raw evidence marker');
    expect(details!.textContent).toContain('structured_output_retry_exhausted');
    expect(target.querySelector('.cause')?.textContent).toContain('the model kept answering');

    unmount(host);
  });

  it('cause null: no <details>, raw evidence renders expanded', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name':
          return Promise.resolve('macos');
        case 'list_agent_cli_presets':
          return Promise.resolve([]);
        case 'get_llm_settings':
          return Promise.resolve(EMPTY_CFG);
        case 'test_llm_provider':
          return Promise.resolve({
            ok: false,
            elapsed_ms: 17,
            error: 'some error nobody has ever seen',
            raw_head: 'unexplained raw evidence',
            cause: null,
            action: null,
          });
        default:
          throw new Error(`unexpected command ${cmd}`);
      }
    });

    const { target, host } = render();
    await settle();

    button(target, 'Test connection').click();
    await settle();

    expect(target.querySelector('details')).toBeNull();
    expect(target.textContent).toContain('unexplained raw evidence');
    expect(target.textContent).toContain('some error nobody has ever seen');

    unmount(host);
  });
});
