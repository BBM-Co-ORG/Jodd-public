// @vitest-environment jsdom
import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import AppSettings from './AppSettings.svelte';
import { appSettingsOpen } from '../stores/ui';

let askHistorySet: () => Promise<unknown> = () => Promise.resolve(null);
let askHistoryGet: () => Promise<boolean | null> = () => Promise.resolve(true);
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
    case 'agent_workspace_status': return Promise.resolve({ account_id: null, folder: 'Notes/__Agent__', eligible: [], hidden: {}, scope_path: '/tmp/s.json', error: null });
    case 'get_ask_history_enabled': return askHistoryGet();
    case 'set_ask_history_enabled': return askHistorySet();
    case 'clear_ask_history': return Promise.resolve(null);
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
  askHistorySet = () => Promise.resolve(null);
  askHistoryGet = () => Promise.resolve(true);
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

async function openSettings() {
  const target = document.createElement('div');
  document.body.append(target);
  appSettingsOpen.set(true);
  component = mount(AppSettings, { target });
  await settle();
  return target;
}
function askHistoryBox(target: HTMLElement) {
  return [...target.querySelectorAll('label')].find(l => l.textContent?.includes('Keep Ask Jodd history'))!
    .querySelector('input') as HTMLInputElement;
}
const btnText = (t: HTMLElement, s: string) => [...t.querySelectorAll('button')].find(b => b.textContent?.trim() === s) as HTMLButtonElement | undefined;

it('turns Ask history off, then deletes existing history only when asked', async () => {
  const target = await openSettings();
  const box = askHistoryBox(target);
  expect(box.checked).toBe(true);
  box.click();
  await settle();
  expect(invoke).toHaveBeenCalledWith('set_ask_history_enabled', { enabled: false });
  expect(target.textContent).toContain('Also delete existing history?');
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('clear_ask_history');
  btnText(target, 'Delete history')!.click();
  await settle();
  expect(invoke.mock.calls.map(c => c[0])).toContain('clear_ask_history');
  expect(target.textContent).not.toContain('Also delete existing history?');
});

it('Keep history closes the prompt without deleting', async () => {
  const target = await openSettings();
  askHistoryBox(target).click();
  await settle();
  btnText(target, 'Keep history')!.click();
  await settle();
  expect(target.textContent).not.toContain('Also delete existing history?');
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('clear_ask_history');
});

it('rolls the Ask history switch back when saving fails', async () => {
  askHistorySet = () => Promise.reject('denied');
  const target = await openSettings();
  const box = askHistoryBox(target);
  box.click();
  await settle();
  expect(askHistoryBox(target).checked).toBe(true);
  expect(target.textContent).toContain('denied');
  expect(target.textContent).not.toContain('Also delete existing history?');
});

it('re-checking the box while the switch-off is still saving takes the Delete offer away at once', async () => {
  const target = await openSettings();
  askHistoryBox(target).click();
  await settle();
  expect(target.textContent).toContain('Also delete existing history?');
  let resolveSet!: (v: unknown) => void;
  askHistorySet = () => new Promise((r) => { resolveSet = r; });
  askHistoryBox(target).click(); // back on; set_ask_history_enabled(true) now pending
  flushSync();
  expect(askHistoryBox(target).checked).toBe(true);
  expect(target.textContent).not.toContain('Also delete existing history?');
  expect(btnText(target, 'Delete history')).toBeUndefined();
  resolveSet(null);
  await settle();
  expect(target.textContent).not.toContain('Also delete existing history?');
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('clear_ask_history');
});

it('reopening App Settings drops a Delete offer left from the last visit', async () => {
  const target = await openSettings();
  askHistoryBox(target).click();
  await settle();
  expect(target.textContent).toContain('Also delete existing history?');
  askHistoryGet = () => Promise.resolve(false); // still off, so only the reset can hide it
  appSettingsOpen.set(false);
  await settle();
  appSettingsOpen.set(true);
  await settle();
  expect(target.textContent).not.toContain('Also delete existing history?');
});
