// @vitest-environment jsdom
import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import AppSettings from './AppSettings.svelte';
import { appSettingsOpen } from '../stores/ui';

const invoke = vi.fn((cmd: string) => {
  switch (cmd) {
    case 'platform_name': return Promise.resolve('macos');
    case 'get_oauth_settings': return Promise.resolve({ client_id: 'existing-google-id' });
    case 'get_ms_oauth_config': return Promise.resolve({ client_id: '', credentials_available: false });
    case 'get_log_settings': return Promise.resolve({ file_logging_enabled: true, log_file_path: '/tmp/test.log', log_file_size_bytes: 0 });
    case 'list_agent_cli_presets': return Promise.resolve([]);
    case 'get_app_llm_config': return Promise.resolve({ cfg: { llm: { provider: 'none' }, apply_to_accounts: false }, has_api_key: null });
    case 'get_ai_limits': return Promise.resolve({});
    case 'list_ai_receipts': return Promise.resolve([]);
    case 'get_ai_receipt_retention': return Promise.resolve(30);
    default: throw new Error(`Unexpected Settings command: ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: [string]) => invoke(...args) }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn(), revealItemInDir: vi.fn() }));
let component: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (component) await unmount(component);
  appSettingsOpen.set(false);
  document.body.innerHTML = '';
  invoke.mockClear();
});
async function settle() { for (let i = 0; i < 12; i++) await tick(); flushSync(); }
it('opens and reopens with metadata commands only; unchanged Save does not touch OAuth credentials', async () => {
  const target = document.createElement('div');
  document.body.append(target);
  appSettingsOpen.set(true);
  component = mount(AppSettings, { target });
  await settle();
  appSettingsOpen.set(false);
  await settle();
  appSettingsOpen.set(true);
  await settle();
  expect(invoke.mock.calls.filter(c => c[0] === 'get_oauth_settings')).toHaveLength(2);
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('get_oauth_config');
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('check_app_llm_key');
  const save = [...target.querySelectorAll('button')].find(b => b.textContent?.trim() === 'Save' && !b.closest('.llm-settings'));
  expect(save).toBeDefined();
  save!.click();
  await settle();
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('save_oauth_config');
});
