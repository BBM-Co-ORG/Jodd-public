// @vitest-environment jsdom
//
// The Account settings window has two Save buttons: the LLM section's own
// (inside LlmProviderSettings, writes `update_llm_settings`) and the window's
// bottom Save (labels / vault name). Observed live on 2026-10-09: tick
// "Allow this account's data in Jodd-managed AI", see the LLM section say
// "Unsaved changes", press the bottom Save — the window closes and the
// checkbox change is gone, accounts.json untouched. The bottom Save is the
// one a user reaches for to "save the window", so it must not drop an edit
// the window is still showing.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import AccountSettings from './AccountSettings.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

const LLM_CFG = {
  data_allowed: true,
  provider: 'none',
  http_base_url: null,
  http_model: null,
  http_api_key_keychain: null,
  agent_preset: null,
  agent_custom: null,
  disable_thinking: false,
};

function backend(overrides: Record<string, (args: any) => unknown> = {}) {
  invoke.mockImplementation((cmd: string, args: any) => {
    if (overrides[cmd]) return Promise.resolve().then(() => overrides[cmd](args));
    switch (cmd) {
      case 'platform_name':
        return Promise.resolve('macos');
      case 'list_agent_cli_presets':
        return Promise.resolve([]);
      case 'get_llm_settings':
        return Promise.resolve({ ...LLM_CFG });
      case 'get_account_settings':
        return Promise.resolve({ notes_label: 'Notes', meta_label: 'Notes-Meta' });
      case 'update_account_settings':
        return Promise.resolve({ notes_label: args.notesLabel, meta_label: args.metaLabel });
      case 'update_llm_settings':
        return Promise.resolve(null);
      case 'rename_local_account':
        return Promise.resolve({ id: args.accountId, email: args.name, added_at: '', backend_kind: 'local_fs', root_dir: null });
      default:
        return Promise.reject(new Error(`unexpected command ${cmd}`));
    }
  });
}

function render(backendKind: string) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const onClose = vi.fn();
  const host = mount(AccountSettings, {
    target,
    props: { accountId: `${backendKind}:a@x`, accountEmail: 'a@x', backendKind, onClose },
  });
  flushSync();
  return { target, host, onClose };
}

async function settle() {
  for (let i = 0; i < 10; i++) await tick();
  flushSync();
}

function dataAllowedBox(target: HTMLElement): HTMLInputElement {
  const box = target.querySelector('.llm-settings input[type="checkbox"]');
  if (!box) throw new Error(`no data-allowed checkbox: ${target.innerHTML}`);
  return box as HTMLInputElement;
}

function bottomSave(target: HTMLElement): HTMLButtonElement {
  const b = target.querySelector('.actions .save');
  if (!b) throw new Error(`no bottom Save: ${target.innerHTML}`);
  return b as HTMLButtonElement;
}

function llmWrites() {
  return invoke.mock.calls.filter(([cmd]) => cmd === 'update_llm_settings');
}

describe('AccountSettings — bottom Save and the LLM section', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
  });

  for (const kind of ['gmail', 'local_fs']) {
    it(`${kind}: bottom Save persists an unsaved data-allowed change`, async () => {
      backend();
      const { target, host, onClose } = render(kind);
      await settle();

      dataAllowedBox(target).click();
      flushSync();
      expect(target.textContent).toContain('Unsaved changes');

      bottomSave(target).click();
      await settle();

      const writes = llmWrites();
      expect(writes).toHaveLength(1);
      expect(writes[0][1]).toMatchObject({ accountId: `${kind}:a@x`, cfg: { data_allowed: false } });
      expect(onClose).toHaveBeenCalledTimes(1);

      unmount(host);
    });
  }

  // Since #170 the permission is opt-in: an account with no stored value
  // shows the box unticked, so ticking it is the edit people actually make.
  it('opt-in: ticking the box on an account with no stored value is persisted', async () => {
    const { data_allowed: _, ...unset } = LLM_CFG;
    backend({ get_llm_settings: () => ({ ...unset }) });
    const { target, host, onClose } = render('gmail');
    await settle();
    expect(dataAllowedBox(target).checked).toBe(false);

    dataAllowedBox(target).click();
    flushSync();
    bottomSave(target).click();
    await settle();

    const writes = llmWrites();
    expect(writes).toHaveLength(1);
    expect(writes[0][1]).toMatchObject({ cfg: { data_allowed: true } });
    expect(onClose).toHaveBeenCalledTimes(1);

    unmount(host);
  });

  it('clean LLM section: bottom Save does not write LLM settings', async () => {
    backend();
    const { target, host, onClose } = render('gmail');
    await settle();

    bottomSave(target).click();
    await settle();

    expect(llmWrites()).toHaveLength(0);
    expect(onClose).toHaveBeenCalledTimes(1);

    unmount(host);
  });

  it('LLM write fails: window stays open and the edit stays on screen', async () => {
    backend({
      update_llm_settings: () => {
        throw new Error('disk full');
      },
    });
    const { target, host, onClose } = render('gmail');
    await settle();

    dataAllowedBox(target).click();
    flushSync();
    bottomSave(target).click();
    await settle();

    expect(onClose).not.toHaveBeenCalled();
    expect(dataAllowedBox(target).checked).toBe(false);
    expect(target.textContent).toContain('disk full');

    unmount(host);
  });
});
