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

    const details = target.querySelector('.test-result details');
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

    expect(target.querySelector('.test-result details')).toBeNull();
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
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
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
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
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
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
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
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
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

describe('LlmProviderSettings — Runs on (agent CLI on a server)', () => {
  const HOST = { target: 'me@vps.example', port: null, key_dir: 'k' };
  const PRESETS = [
    { id: 'claude', label: 'Claude Code', fidelity: 'structured', installed: true, resolved_path: '/x/claude', available: true, unavailable_reason: null, file_access: 'enabled', argv_prompt: false },
    { id: 'codex', label: 'Codex CLI', fidelity: 'structured', installed: false, resolved_path: null, available: true, unavailable_reason: null, file_access: 'enabled', argv_prompt: false },
    { id: 'gemini', label: 'Gemini CLI', fidelity: 'heuristic', installed: true, resolved_path: '/x/gemini', available: true, unavailable_reason: null, file_access: 'unknown', argv_prompt: true },
  ];
  const REMOTE_PRESETS = [{ id: 'claude', path: null }, { id: 'codex', path: '/usr/bin/codex' }];

  function mockIpc(platform: string, cfg: Record<string, unknown>) {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve(platform);
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...cfg });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
  }
  function radio(target: HTMLElement, label: string): HTMLInputElement {
    const l = [...target.querySelectorAll('label')].find((x) => (x.textContent ?? '').includes(label));
    if (!l) throw new Error(`no radio ${label}`);
    return l.querySelector('input[type="radio"]') as HTMLInputElement;
  }
  function savedPayload() {
    return invoke.mock.calls.filter((c) => c[0] === 'update_llm_settings').at(-1)![1].cfg;
  }
  const SERVER_CFG = { provider: 'agent_cli', agent_preset: 'codex', agent_host: HOST };

  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  it('on Android the Agent CLI option is offered, only as a server', async () => {
    mockIpc('android', SERVER_CFG);
    const { target, host } = render();
    await settle();
    expect(radio(target, 'Agent CLI')).toBeTruthy();
    expect(target.textContent).not.toContain('This computer');
    expect(target.textContent).toContain('A server');
    unmount(host);
  });

  it('a configured host loads as "A server" and saves agent_host', async () => {
    mockIpc('macos', SERVER_CFG);
    const { target, host } = render();
    await settle();
    expect(radio(target, 'A server').checked).toBe(true);
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
    await settle();
    expect(savedPayload().agent_host).toEqual(HOST);
    unmount(host);
  });

  it('choosing "This computer" saves agent_host: null, and "A server" again restores it', async () => {
    mockIpc('macos', SERVER_CFG);
    const { target, host } = render();
    await settle();
    radio(target, 'This computer').click();
    await settle();
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
    await settle();
    expect(savedPayload().agent_host).toBeNull();
    radio(target, 'A server').click();
    await settle();
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
    await settle();
    expect(savedPayload().agent_host).toEqual(HOST);
    unmount(host);
  });

  it('with a host, the preset list is claude/codex only, gated by the server, and has no Custom', async () => {
    mockIpc('macos', SERVER_CFG);
    const { target, host } = render();
    await settle();
    const select = target.querySelector('.agent-fields select') as HTMLSelectElement;
    const opts = [...select.querySelectorAll('option')].filter((o) => o.value && o.value !== 'null');
    expect(opts.map((o) => o.value)).toEqual(['claude', 'codex']);
    expect(opts.find((o) => o.value === 'claude')!.disabled).toBe(true);   // not installed on the server
    expect(opts.find((o) => o.value === 'codex')!.disabled).toBe(false);
    expect(select.textContent).not.toContain('Custom');
    unmount(host);
  });

  it('"A server" with no server set up cannot be saved (it would silently run locally)', async () => {
    mockIpc('macos', { provider: 'agent_cli', agent_preset: null, agent_host: null });
    const { target, host } = render();
    await settle();
    radio(target, 'A server').click();
    await settle();
    await expect(host.save()).rejects.toThrow('save: set up your server first, or choose This computer.');
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('update_llm_settings');
    expect(target.textContent).toContain('Set up your server above');
    unmount(host);
  });

  it('the file-access warning names the server', async () => {
    mockIpc('macos', SERVER_CFG);
    const { target, host } = render();
    await settle();
    expect(target.textContent).toContain('on vps.example');
    expect(target.textContent).not.toContain('your computer');
    unmount(host);
  });

  async function removeServer(target: HTMLElement) {
    button(target, 'Remove').click();
    await settle();
    [...target.querySelectorAll('dialog button')].find((b) => b.textContent?.trim() === 'Remove server')!
      .dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await settle();
  }

  it('Remove saves provider none and no host at once (account scope)', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    await removeServer(target);
    const payload = savedPayload();
    expect(payload.provider).toBe('none');
    expect(payload.agent_host).toBeNull();
    expect(payload.agent_preset).toBeNull();
    // 'none' on an account means "inherit the app's provider", not "off".
    expect(target.textContent).toContain("Server removed. This account now follows the app's AI provider");
    expect(target.textContent).not.toContain('vps.example');
    unmount(host);
  });

  it('the account-scope Remove confirm says the account falls back to the app provider', async () => {
    mockIpc('macos', SERVER_CFG);
    const { target, host } = render();
    await settle();
    button(target, 'Remove').click();
    await settle();
    const dialog = target.querySelector('dialog')!.textContent!;
    expect(dialog).toContain("app's AI provider");
    expect(dialog).not.toContain('switched off');
    unmount(host);
  });

  it('Remove persists only the server change, never other unsaved edits (e.g. the AI-data consent)', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG, data_allowed: false });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    // Tick consent but do NOT press Save.
    const consent = [...target.querySelectorAll('input[type=checkbox]')].find((i) =>
      i.parentElement?.textContent?.includes('Allow this account')) as HTMLInputElement;
    consent.click();
    await settle();
    await removeServer(target);
    expect(savedPayload().data_allowed).toBe(false);
    expect(savedPayload().provider).toBe('none');
    // The tick is still on screen, still unsaved: the page Save stays on.
    expect(consent.checked).toBe(true);
    expect(host.isDirty()).toBe(true);
    unmount(host);
  });

  it('a failed save after Remove says the server is gone and leaves Save to retry', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'update_llm_settings': return Promise.reject('disk full');
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    await removeServer(target);
    expect(target.textContent).toContain('The server was removed, but saving the change failed: disk full');
    expect(host.isDirty()).toBe(true); // the page Save stays on to retry
    unmount(host);
  });

  it('removing a server that was never saved does not touch the saved config', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        // Saved: a local codex. The server below was set up in this form but never saved.
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, provider: 'agent_cli', agent_preset: 'codex', agent_host: null });
        case 'ssh_fingerprint': return Promise.resolve({ algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'l' });
        case 'ai_host_setup': return Promise.resolve({ host: HOST, status: { version: 1, current: true, presets: REMOTE_PRESETS } });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    radio(target, 'A server').click();
    await settle();
    const field = (label: string) => [...target.querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith(label))!.querySelector('input') as HTMLInputElement;
    field('Server').value = 'vps.example'; field('Server').dispatchEvent(new Event('input', { bubbles: true }));
    field('Username').value = 'me'; field('Username').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Continue').click();
    await settle();
    field('Password').value = 'pw'; field('Password').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Trust and set up').click();
    await settle();
    await removeServer(target);
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('update_llm_settings');
    unmount(host);
  });

  async function changeServer(target: HTMLElement, to: { server: string; user: string }) {
    button(target, 'Change server').click();
    await settle();
    const field = (label: string) => [...target.querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith(label))!.querySelector('input') as HTMLInputElement;
    field('Server').value = to.server; field('Server').dispatchEvent(new Event('input', { bubbles: true }));
    field('Username').value = to.user; field('Username').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Continue').click();
    await settle();
    field('Password').value = 'pw'; field('Password').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Trust and set up').click();
    await settle();
  }
  const NEW_HOST = { target: 'jodd@box.example', port: null, key_dir: 'k2' };

  it('Change server moves the saved config to the new host at once, keeping the CLI', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ssh_fingerprint': return Promise.resolve({ algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'l' });
        case 'ai_host_setup': return Promise.resolve({ host: NEW_HOST, status: { version: 1, current: true, presets: REMOTE_PRESETS } });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    await changeServer(target, { server: 'box.example', user: 'jodd' });
    expect(invoke).toHaveBeenCalledWith('ai_host_forget', { host: HOST });
    const payload = savedPayload();
    expect(payload.agent_host).toEqual(NEW_HOST);
    expect(payload.provider).toBe('agent_cli');
    expect(payload.agent_preset).toBe('codex');
    expect(target.textContent).toContain('Server changed to box.example.');
    expect(host.isDirty()).toBe(false);
    unmount(host);
  });

  it('a failed save after Change server leaves the form dirty to retry', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ssh_fingerprint': return Promise.resolve({ algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'l' });
        case 'ai_host_setup': return Promise.resolve({ host: NEW_HOST, status: { version: 1, current: true, presets: REMOTE_PRESETS } });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'update_llm_settings': return Promise.reject('disk full');
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    await changeServer(target, { server: 'box.example', user: 'jodd' });
    expect(target.textContent).toContain('The server was changed, but saving the change failed: disk full');
    expect(host.isDirty()).toBe(true);
    unmount(host);
  });

  it('changing a server that was never saved does not touch the saved config', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        // Saved: a local codex. The form's server was set up here and never saved.
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, provider: 'agent_cli', agent_preset: 'codex', agent_host: null });
        case 'ssh_fingerprint': return Promise.resolve({ algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'l' });
        case 'ai_host_setup': return Promise.resolve({
          host: invoke.mock.calls.filter((c) => c[0] === 'ai_host_setup').length > 1 ? NEW_HOST : HOST,
          status: { version: 1, current: true, presets: REMOTE_PRESETS } });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    radio(target, 'A server').click();
    await settle();
    const field = (label: string) => [...target.querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith(label))!.querySelector('input') as HTMLInputElement;
    field('Server').value = 'vps.example'; field('Server').dispatchEvent(new Event('input', { bubbles: true }));
    field('Username').value = 'me'; field('Username').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Continue').click();
    await settle();
    field('Password').value = 'pw'; field('Password').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Trust and set up').click();
    await settle();
    await changeServer(target, { server: 'box.example', user: 'jodd' });
    expect(invoke).toHaveBeenCalledWith('ai_host_forget', { host: HOST });
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('update_llm_settings');
    unmount(host);
  });

  it('Remove saves provider none and no host at once (app scope)', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_app_llm_config': return Promise.resolve({
          cfg: { llm: { ...EMPTY_CFG, ...SERVER_CFG }, apply_to_accounts: false }, has_api_key: null });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.resolve(null);
        case 'set_app_llm_config': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const target = document.createElement('div');
    document.body.append(target);
    const host = mount(LlmProviderSettings, { target, props: { scope: 'app' } });
    await settle();
    await removeServer(target);
    const call = invoke.mock.calls.filter((c) => c[0] === 'set_app_llm_config').at(-1)!;
    expect(call[1].cfg.llm.provider).toBe('none');
    expect(call[1].cfg.llm.agent_host).toBeNull();
    expect(target.textContent).toContain('Server removed. Choose an AI provider.');
    unmount(host);
  });

  // Android never has a local preset to carry over, so a first-time setup
  // starts from agent_preset: null. Found live on 2026-10-14: setup succeeded,
  // the CLI list stayed on "Choose a CLI…" and Test was disabled with no reason.
  async function setUpServer(target: HTMLElement) {
    const field = (label: string) => [...target.querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith(label))!.querySelector('input') as HTMLInputElement;
    field('Server').value = 'vps.example'; field('Server').dispatchEvent(new Event('input', { bubbles: true }));
    field('Username').value = 'me'; field('Username').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Continue').click();
    await settle();
    field('Password').value = 'pw'; field('Password').dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    button(target, 'Trust and set up').click();
    await settle();
  }
  const BOTH = [{ id: 'claude', path: '/h/claude' }, { id: 'codex', path: '/h/codex' }];
  function firstTimeIpc(remote: { id: string; path: string | null }[]) {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('android');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, provider: 'http' });
        case 'ssh_fingerprint': return Promise.resolve({ algorithm: 'ED25519', fingerprint: 'SHA256:abc', known_hosts_line: 'l' });
        case 'ai_host_setup': return Promise.resolve({ host: HOST, status: { version: 2, current: true, presets: remote } });
        case 'ai_host_status': return Promise.resolve({ version: 2, current: true, presets: remote });
        case 'update_llm_settings': return Promise.resolve();
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
  }
  const presetSelect = (target: HTMLElement) => target.querySelector('.agent-fields select') as HTMLSelectElement;

  it('first-time setup with no preset picks a CLI the server has, preferring claude', async () => {
    firstTimeIpc(BOTH);
    const { target, host } = render();
    await settle();
    radio(target, 'Agent CLI').click();
    await settle();
    await setUpServer(target);
    expect(presetSelect(target).value).toBe('claude');
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
    await settle();
    expect(savedPayload().agent_preset).toBe('claude');
    expect(button(target, 'Test connection').disabled).toBe(false);
    unmount(host);
  });

  it('first-time setup picks the only CLI the server has', async () => {
    firstTimeIpc(REMOTE_PRESETS); // claude missing, codex installed
    const { target, host } = render();
    await settle();
    radio(target, 'Agent CLI').click();
    await settle();
    await setUpServer(target);
    expect(presetSelect(target).value).toBe('codex');
    unmount(host);
  });

  it('a server with neither CLI leaves the choice empty and says why Test is unavailable', async () => {
    firstTimeIpc([{ id: 'claude', path: null }, { id: 'codex', path: null }]);
    const { target, host } = render();
    await settle();
    radio(target, 'Agent CLI').click();
    await settle();
    await setUpServer(target);
    void host.save().catch(() => {}); // the Settings page's Save (section-form contract)
    await settle();
    expect(savedPayload().agent_preset).toBeNull();
    expect(button(target, 'Test connection').disabled).toBe(true);
    expect(target.textContent).toContain('Choose a CLI above and Save to test it.');
    unmount(host);
  });

  it('never replaces a CLI the user already chose', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('android');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG }); // codex
        case 'ai_host_status': return Promise.resolve({ version: 2, current: true, presets: BOTH });
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    expect(presetSelect(target).value).toBe('codex');
    unmount(host);
  });

  it('a failed Remove keeps the saved config untouched', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve(PRESETS);
        case 'get_llm_settings': return Promise.resolve({ ...EMPTY_CFG, ...SERVER_CFG });
        case 'ai_host_status': return Promise.resolve({ version: 1, current: true, presets: REMOTE_PRESETS });
        case 'ai_host_forget': return Promise.reject('nope');
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const { target, host } = render();
    await settle();
    await removeServer(target);
    expect(invoke.mock.calls.map((c) => c[0])).not.toContain('update_llm_settings');
    unmount(host);
  });
});

describe('LlmProviderSettings — section-form contract (isDirty / save)', () => {
  let failSave: boolean;
  beforeEach(() => {
    failSave = false;
    invoke.mockReset();
    document.body.innerHTML = '';
    invoke.mockImplementation((cmd: string) => {
      if (cmd === 'platform_name') return Promise.resolve('macos');
      if (cmd === 'list_agent_cli_presets') return Promise.resolve([]);
      if (cmd === 'get_app_llm_config') return Promise.resolve({
        cfg: { llm: { ...EMPTY_CFG, provider: 'http', http_base_url: 'https://example.test', http_model: 'test', disable_thinking: false, agent_host: null }, apply_to_accounts: false },
        has_api_key: null,
      });
      if (cmd === 'set_app_llm_config') return failSave ? Promise.reject('disk full') : Promise.resolve();
      if (cmd === 'test_llm_provider') return new Promise(() => {}); // never settles: a test in progress
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
  async function editModel(target: HTMLElement, value: string) {
    const model = target.querySelector('input[placeholder="gpt-4o-mini"]') as HTMLInputElement;
    model.value = value;
    model.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
  }

  it('isDirty() is true after editing the model; save() calls set_app_llm_config and clears it', async () => {
    const { target, host } = await appForm();
    expect(host.isDirty()).toBe(false);
    await editModel(target, 'edited-model');
    expect(host.isDirty()).toBe(true);
    await host.save();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_app_llm_config', expect.objectContaining({
      cfg: expect.objectContaining({ llm: expect.objectContaining({ http_model: 'edited-model' }) }),
    }));
    expect(host.isDirty()).toBe(false);
    await unmount(host);
  });

  it('save() throws a readable message when the write fails, and stays dirty', async () => {
    const { target, host } = await appForm();
    await editModel(target, 'edited-model');
    failSave = true;
    await expect(host.save()).rejects.toThrow('save: disk full');
    await settle();
    // One surface per failure: the Settings page shows it above this section.
    expect(target.textContent).not.toContain('disk full');
    expect(host.isDirty()).toBe(true);
    await unmount(host);
  });

  it('a re-entrant save() returns the same promise and writes once', async () => {
    const { target, host } = await appForm();
    await editModel(target, 'edited-model');
    const p1 = host.save(); const p2 = host.save();
    expect(p2).toBe(p1);
    await Promise.all([p1, p2]);
    expect(invoke.mock.calls.filter((c) => c[0] === 'set_app_llm_config')).toHaveLength(1);
    await unmount(host);
  });

  it('save() refuses a whitespace-only new key with a message instead of skipping the check', async () => {
    const { target, host } = await appForm();
    button(target, 'Change API key').click();
    await settle();
    const input = target.querySelector('input[autocomplete="new-password"]') as HTMLInputElement;
    input.value = '   ';
    input.dispatchEvent(new Event('input', { bubbles: true }));
    await settle();
    await expect(host.save()).rejects.toThrow('Enter a valid API key.');
    expect(invoke.mock.calls.filter((c) => c[0] === 'set_app_llm_config')).toHaveLength(0);
    // The Settings page shows this message above the section (settings.test.ts).
    await unmount(host);
  });

  it('save() throws while a connection test is in progress', async () => {
    const { target, host } = await appForm();
    button(target, 'Test connection').click();
    await settle();
    await expect(host.save()).rejects.toThrow('Wait for the connection test to finish, then save.');
    expect(invoke.mock.calls.filter((c) => c[0] === 'set_app_llm_config')).toHaveLength(0);
    await unmount(host);
  });
});


describe('LlmProviderSettings — what "Use app default" inherits (account scope)', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  const CODEX_ON_SERVER = {
    ...EMPTY_CFG,
    provider: 'agent_cli',
    agent_preset: 'codex',
    agent_host: { target: 'jodd@jodd-zf', port: null, key_dir: '/k' },
    disable_thinking: false,
  };

  function mockWith(
    app: (() => unknown) | { llm: unknown; apply_to_accounts: boolean },
    own = EMPTY_CFG,
    dataAllowed = true,
  ) {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve([{ id: 'codex', label: 'Codex CLI' }]);
        case 'get_llm_settings': return Promise.resolve({ ...own, data_allowed: dataAllowed });
        case 'get_app_llm_config':
          return typeof app === 'function'
            ? Promise.reject(app())
            : Promise.resolve({ cfg: app, has_api_key: null });
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
  }

  const inheritLabel = (t: HTMLElement) =>
    [...t.querySelectorAll('label')].find((l) => (l.textContent ?? '').includes('Use app default'))!;

  it('names the app provider beside the option, and says where it comes from', async () => {
    mockWith({ llm: CODEX_ON_SERVER, apply_to_accounts: true });
    const { target, host } = render();
    await settle();
    expect(inheritLabel(target).textContent).toContain('Codex CLI on jodd-zf');
    expect(target.textContent).toContain('Settings › AI');
    expect(target.textContent).not.toContain('uses no AI');
    await unmount(host);
  });

  it('separates the option and the provider name with spaces on both sides', async () => {
    // Live check 2026-10-25: it rendered "Use app default· Codex". A space
    // at the start of the name's <span> is dropped when the page renders.
    mockWith({ llm: CODEX_ON_SERVER, apply_to_accounts: true });
    const { target, host } = render();
    await settle();
    expect(inheritLabel(target).textContent!.replace(/\s+/g, ' ').trim())
      .toBe('Use app default · Codex CLI on jodd-zf');
    await unmount(host);
  });

  it('with the AI data permission off, says nothing is sent, and updates when it is ticked', async () => {
    // Live check 2026-10-25: Outlook (permission off) read "From Settings › AI"
    // as if it used Codex, though the backend refuses every AI call for it.
    mockWith({ llm: CODEX_ON_SERVER, apply_to_accounts: true }, EMPTY_CFG, false);
    const { target, host } = render();
    await settle();
    const warn = target.querySelector('.inherit.warn');
    expect(warn?.textContent).toContain('nothing from this account is sent');
    expect(target.textContent).not.toContain('From Settings › AI. Change it there');
    const box = target.querySelector('input[type="checkbox"]') as HTMLInputElement;
    box.click();
    await settle();
    expect(target.querySelector('.inherit.warn')).toBeNull();
    expect(target.textContent).toContain('From Settings › AI. Change it there');
    await unmount(host);
  });

  it('when the app does not share its provider, says the account uses no AI', async () => {
    mockWith({ llm: CODEX_ON_SERVER, apply_to_accounts: false });
    const { target, host } = render();
    await settle();
    expect(inheritLabel(target).textContent).not.toContain('Codex');
    expect(target.querySelector('.inherit.warn')?.textContent).toContain('uses no AI');
    await unmount(host);
  });

  it('when the app has no provider, says the account uses no AI', async () => {
    mockWith({ llm: EMPTY_CFG, apply_to_accounts: true });
    const { target, host } = render();
    await settle();
    expect(target.querySelector('.inherit.warn')?.textContent).toContain('uses no AI');
    await unmount(host);
  });

  it('with its own provider, says it overrides the app one and that Ask Jodd still uses the app one', async () => {
    mockWith({ llm: CODEX_ON_SERVER, apply_to_accounts: true }, { ...EMPTY_CFG, provider: 'disabled' });
    const { target, host } = render();
    await settle();
    expect(target.textContent).toContain('Ask Jodd always uses Settings › AI');
    expect(target.querySelector('.inherit')).toBeNull();
    await unmount(host);
  });

  it('a failed app-config read leaves the form working and claims nothing', async () => {
    mockWith(() => 'boom');
    const { target, host } = render();
    await settle();
    expect(target.textContent).not.toContain('load:');
    expect(inheritLabel(target).textContent?.trim()).toMatch(/^Use app default$/);
    expect(target.querySelector('.inherit.warn')).toBeNull();
    await unmount(host);
  });

  it('the app scope never asks for itself', async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'platform_name': return Promise.resolve('macos');
        case 'list_agent_cli_presets': return Promise.resolve([]);
        case 'get_app_llm_config': return Promise.resolve({ cfg: { llm: EMPTY_CFG, apply_to_accounts: true }, has_api_key: null });
        default: throw new Error(`unexpected command ${cmd}`);
      }
    });
    const target = document.createElement('div');
    document.body.append(target);
    const host = mount(LlmProviderSettings, { target, props: { scope: 'app' } });
    await settle();
    expect(invoke.mock.calls.filter((c) => c[0] === 'get_app_llm_config')).toHaveLength(1);
    expect(target.textContent).toContain('accounts set to “Use app default”');
    await unmount(host);
  });
});
