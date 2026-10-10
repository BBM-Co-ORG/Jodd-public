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
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import LlmProviderSettings from './LlmProviderSettings.svelte';
import { AI_CONSENT_NEEDED } from '../aiConsent';

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


describe('App Settings API key actions', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'platform_name') return Promise.resolve('macos');
      if (cmd === 'list_agent_cli_presets') return Promise.resolve([]);
      if (cmd === 'get_app_llm_config') return Promise.resolve({
        cfg: { llm: { ...EMPTY_CFG, provider: 'http', http_base_url: 'https://example.test', http_model: 'test' }, apply_to_accounts: false },
        has_api_key: null,
      });
      if (cmd === 'check_app_llm_key') return Promise.reject('Access cancelled');
      if (cmd === 'set_app_llm_config' || cmd === 'delete_app_llm_key') return Promise.resolve();
      throw new Error(`unexpected command ${cmd}`);
    });
  });

  async function appForm() {
    const target = document.createElement('div');
    document.body.append(target);
    const host = mount(LlmProviderSettings, { target, props: { scope: 'app' } });
    await settle();
    return { target, host };
  }

  it('opens with unknown status without probing, and cancellation does not imply absence', async () => {
    const { target, host } = await appForm();
    expect(target.textContent).toContain('Not checked yet');
    expect(invoke.mock.calls.map(c => c[0])).not.toContain('check_app_llm_key');
    button(target, 'Check API key').click();
    await settle();
    expect(target.textContent).toContain('Not checked yet');
    expect(target.textContent).toContain('Access cancelled');
    await unmount(host);
  });

  it('saves provider fields with a null key and only writes a new key after Save', async () => {
    const { target, host } = await appForm();
    button(target, 'Save').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_app_llm_config', expect.objectContaining({ apiKey: null }));
    invoke.mockClear();
    button(target, 'Change API key').click();
    await settle();
    const input = target.querySelector('input[autocomplete="new-password"]') as HTMLInputElement;
    input.value = 'replacement';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Show new key').click();
    await settle();
    expect(input.type).toBe('text');
    expect(invoke).not.toHaveBeenCalled();
    button(target, 'Save').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_app_llm_config', expect.objectContaining({ apiKey: 'replacement' }));
    expect(target.textContent).toContain('Previously saved');
    expect(target.querySelector('input[autocomplete="new-password"]')).toBeNull();
    await unmount(host);
  });

  it('confirms deletion and keeps an unrelated model draft unsaved', async () => {
    const { target, host } = await appForm();
    const model = target.querySelector('input[placeholder="gpt-4o-mini"]') as HTMLInputElement;
    model.value = 'draft-model';
    model.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Delete API key').click();
    await settle();
    expect(invoke.mock.calls.map(c => c[0])).not.toContain('delete_app_llm_key');
    button(target.querySelector('dialog')!, 'Cancel').click();
    await settle();
    expect(invoke.mock.calls.map(c => c[0])).not.toContain('delete_app_llm_key');
    button(target, 'Delete API key').click();
    await settle();
    button(target.querySelector('dialog')!, 'Delete API key').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('delete_app_llm_key');
    expect(invoke.mock.calls.map(c => c[0])).not.toContain('set_app_llm_config');
    expect(model.value).toBe('draft-model');
    expect(target.textContent).toContain('Unsaved changes');
    expect(target.textContent).toContain('Last known to be absent');
    await unmount(host);
  });
});

describe('LlmProviderSettings — AI data permission mirror', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  it('an account with no recorded permission shows unticked and saves false', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve([]);
        case 'get_llm_settings': return Promise.resolve(EMPTY_CFG);
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    const box = target.querySelector('input[type="checkbox"]') as HTMLInputElement;
    expect(box.checked).toBe(false);
    button(target, 'Save').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith(
      'update_llm_settings',
      expect.objectContaining({ cfg: expect.objectContaining({ data_allowed: false }) }),
    );
    unmount(host);
  });

  it('the app scope stores no data permission (only accounts have one)', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve([]);
        case 'get_app_llm_config': return Promise.resolve({
          cfg: { llm: { ...EMPTY_CFG, provider: 'http', http_base_url: 'https://example.test', http_model: 'test' }, apply_to_accounts: true },
          has_api_key: true,
        });
        case 'set_app_llm_config': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const target = document.createElement('div');
    document.body.append(target);
    const host = mount(LlmProviderSettings, { target, props: { scope: 'app' } });
    await settle();
    button(target, 'Save').click();
    await settle();
    const call = invoke.mock.calls.find((c) => c[0] === 'set_app_llm_config');
    expect(call?.[1].cfg.llm.data_allowed).toBeNull();
    unmount(host);
  });

  it('a consent refusal from Test says how to allow it', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve([]);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, data_allowed: false });
        case 'test_llm_provider': return Promise.reject(`provider not configured: ${AI_CONSENT_NEEDED}`);
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    button(target, 'Test connection').click();
    await settle();
    expect(target.querySelector('.test-result')?.textContent)
      .toContain('Tick “Allow this account’s data in Jodd-managed AI” above and Save, then test again.');
    unmount(host);
  });
});
