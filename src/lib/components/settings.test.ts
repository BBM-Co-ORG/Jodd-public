// @vitest-environment jsdom
//
// Settings (spec 2026-10-20 §4.3): one sectioned surface with one save model.
// Also holds the assertions moved from the deleted AppSettings.test.ts and
// AccountSettings.test.ts, and the hidden-folder cases moved from
// AgentWorkspaceSettings.test.ts (that block now lives in Privacy).
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import { get, type Writable } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn(), revealItemInDir: vi.fn() }));
vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<Record<string, unknown>>();
  return { ...orig, isAndroid: writable(false), isMacos: writable(true), supportsIcloud: writable(true) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<Record<string, unknown>>();
  return { ...orig, androidLayoutMode: writable('phone') };
});

import Settings from './Settings.svelte';
import AuthScreen from './AuthScreen.svelte';
import { settingsRoute, localFolderBusy, addAccountRequest } from '../stores/ui';
import { accounts, currentAccount, isAuthenticated } from '../stores/notes';
import { dupStats } from '../stores/dupStats';
import { isAndroid as isAndroidRO, supportsIcloud as supportsIcloudRO } from '../stores/platform';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import { handlePopState, initPhoneNavHistory } from '../stores/phoneNav';
import type { Account } from '../types';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const supportsIcloud = supportsIcloudRO as unknown as Writable<boolean>;
const androidLayoutMode = layoutRO as unknown as Writable<'phone' | 'tablet' | 'tabletWide'>;

// ─── Backend ───────────────────────────────────────────────────────────────

const APP_LLM = {
  provider: 'none', http_base_url: null, http_model: null, http_api_key_keychain: null,
  agent_preset: null, agent_custom: null, agent_host: null, disable_thinking: false,
};
const ACCOUNT_LLM = { ...APP_LLM, data_allowed: true };
const LIMITS = { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
const GMAIL: Account = { id: 'gmail:a@x.com', email: 'a@x.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail' };
const LOCAL: Account = { id: 'local_fs:a@x', email: 'a@x', added_at: '2026-01-01T00:00:00Z', backend_kind: 'local_fs', root_dir: null };
const OUTLOOK_DRAINING: Account = { id: 'microsoft:b@y.com', email: 'b@y.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'microsoft', status: 'draining' };

type Handler = (args: any) => unknown;
let overrides: Record<string, Handler> = {};

function backend(cmd: string, args: any): unknown {
  if (overrides[cmd]) return overrides[cmd](args);
  switch (cmd) {
    case 'platform_name': return 'macos';
    case 'get_oauth_settings': return { client_id: 'existing-google-id' };
    case 'get_ms_oauth_config': return { client_id: '', credentials_available: false };
    case 'get_oauth_config': return { credentials_available: false };
    case 'get_log_settings': return { file_logging_enabled: true, log_file_path: '/tmp/test.log', log_file_size_bytes: 0 };
    case 'list_agent_cli_presets': return [];
    case 'get_app_llm_config': return { cfg: { llm: { ...APP_LLM }, apply_to_accounts: false }, has_api_key: null };
    case 'set_app_llm_config': return null;
    case 'set_stt_config': return null;
    case 'get_stt_config': return { cfg: { preset: 'custom', base_url: '', model: '', language: null, max_upload_mb: 25, confirmed_host: null }, has_api_key: null };
    case 'get_ai_limits': return { ...LIMITS };
    case 'set_ai_limits': return null;
    case 'list_ai_receipts': return [];
    case 'get_ai_receipt_retention': return 30;
    case 'agent_workspace_status': return { account_id: null, folder: 'Notes/__Agent__', eligible: [], hidden: {}, scope_path: '/tmp/s.json', error: null };
    case 'list_folders': return ['Notes', 'Notes/__Agent__', 'Notes/__Agent__/Projects', 'Notes/Personal', 'Notes/Work'];
    case 'set_folder_hidden_from_agents': return null;
    case 'get_ask_history_enabled': return true;
    case 'set_ask_history_enabled': return null;
    case 'clear_ask_history': return null;
    case 'clear_oauth_config': return null;
    case 'save_oauth_config': return null;
    case 'save_ms_oauth_config': return null;
    case 'get_llm_settings': return { ...ACCOUNT_LLM };
    case 'update_llm_settings': return null;
    case 'get_account_settings': return { notes_label: 'Notes', meta_label: 'Notes-Meta' };
    case 'update_account_settings': return { notes_label: args.notesLabel, meta_label: args.metaLabel };
    case 'rename_local_account': return { id: args.accountId, email: args.name, added_at: '', backend_kind: 'local_fs', root_dir: null };
    case 'get_dup_stats': return { collapsed: args.accountId === GMAIL.id ? 3 : 0, uuids_affected: 1 };
    case 'count_pending_pushes': return { notes: 0, deletes: 0, pins: 0, folders: 0, blocked: 0 };
    case 'remove_account': return 'removed';
    case 'set_account_status': return null;
    default: throw new Error(`Unexpected Settings command: ${cmd}`);
  }
}

const occurrences = (text: string, needle: string) => text.split(needle).length - 1;
const commands = () => invoke.mock.calls.map((c) => c[0] as string).filter((c) => c !== 'platform_name');
const calls = (cmd: string) => invoke.mock.calls.filter((c) => c[0] === cmd);

// ─── Harness ───────────────────────────────────────────────────────────────

let target: HTMLElement;
let app: ReturnType<typeof mount> | undefined;

/** Deterministic: ticks and microtasks only (R8), plus one macrotask turn for popstate. */
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

async function open(route: string) {
  settingsRoute.set(route);
  app = mount(Settings, { target });
  await settle();
}

const dialogs = () => [...document.querySelectorAll('dialog')].filter((d) => d.open) as HTMLDialogElement[];
const top = () => dialogs().at(-1)!;
const settingsDialog = () => [...document.querySelectorAll('dialog')].find((d) => d.querySelector('h2')?.textContent === 'Settings') as HTMLDialogElement;
const guard = () => dialogs().find((d) => d.textContent?.includes('Discard unsaved changes?'));
function button(root: ParentNode, text: string): HTMLButtonElement {
  const b = [...root.querySelectorAll('button')].find((x) => x.textContent?.trim() === text);
  if (!b) throw new Error(`no button "${text}"`);
  return b as HTMLButtonElement;
}
const maybeButton = (root: ParentNode, text: string) =>
  [...root.querySelectorAll('button')].find((x) => x.textContent?.trim() === text) as HTMLButtonElement | undefined;
const pageSave = () => settingsDialog().querySelector<HTMLButtonElement>('footer .btn.primary')!;
const pageTitle = () => settingsDialog().querySelector('.page-title')?.textContent;
function key(el: Element, value: string) {
  el.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true }));
}
function radio(root: ParentNode, label: string): HTMLInputElement {
  const l = [...root.querySelectorAll('label')].find((x) => x.textContent?.includes(label));
  if (!l) throw new Error(`no radio "${label}"`);
  return l.querySelector('input') as HTMLInputElement;
}
function type(input: HTMLInputElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

beforeEach(() => {
  overrides = {};
  invoke.mockReset();
  invoke.mockImplementation((cmd: string, args: any) => {
    try { return Promise.resolve(backend(cmd, args)); } catch (e) { return Promise.reject(e); }
  });
  isAndroid.set(false);
  supportsIcloud.set(true);
  androidLayoutMode.set('phone');
  accounts.set([GMAIL, LOCAL]);
  currentAccount.set(GMAIL.id);
  isAuthenticated.set(true);
  dupStats.set({});
  localFolderBusy.set(false);
  addAccountRequest.set(null);
  window.addEventListener('popstate', handlePopState);
  target = document.createElement('div');
  document.body.append(target);
});

afterEach(async () => {
  if (app) await unmount(app);
  app = undefined;
  settingsRoute.set(null);
  await settle();
  await new Promise((r) => setTimeout(r, 0));
  window.removeEventListener('popstate', handlePopState);
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

// ─── The brief's cases ─────────────────────────────────────────────────────

describe('Settings — shell and save model', () => {
  it('AI: the provider description names every account feature, not just Extract and auto-link', async () => {
    // Live check 2026-10-25: the section still said accounts "can adopt it
    // for Extract and auto-link"; it also serves Summarize, Organize, Studio
    // and ingest — every account feature set to "Use app default".
    await open('ai');
    // Collapsed as the page shows it: the source wraps lines inside the text.
    const text = settingsDialog().textContent!.replace(/\s+/g, ' ');
    expect(text).not.toContain('Extract and auto-link');
    expect(text).toContain('behind any account set to “Use app default”');
  });

  it('opens at the route section', async () => {
    await open('ai');
    expect(settingsDialog().open).toBe(true);
    expect(pageTitle()).toBe('AI');
    expect(settingsDialog().textContent).toContain('LLM provider');
    expect(settingsDialog().querySelector('[aria-current="page"]')!.textContent?.trim()).toBe('AI');
    // The save model is stated once, at the top of the section list: one help
    // line, the rest under Learn more.
    const help = [...settingsDialog().querySelectorAll('.nav > .help')];
    expect(help).toHaveLength(1);
    expect(help[0].textContent).toContain('Pages with text fields have one Save button.');
    const more = settingsDialog().querySelector('.nav > details.learn-more')!;
    expect(more.textContent).toContain("You can't leave such a page with unsaved changes without choosing Save or Discard.");
  });

  it('is closed while the route is null, and closing sets it back to null', async () => {
    settingsRoute.set(null);
    app = mount(Settings, { target });
    await settle();
    expect(document.querySelector('dialog')).toBeNull();
    settingsRoute.set('general');
    await settle();
    expect(pageTitle()).toBe('General');
    settingsDialog().querySelector<HTMLButtonElement>('.close')!.click();
    await settle();
    expect(get(settingsRoute)).toBeNull();
    expect(document.querySelector('dialog')).toBeNull();
  });

  it('one Save on the AI page saves LLM and limits in order', async () => {
    await open('ai');
    expect(pageSave().disabled).toBe(true); // nothing to save yet
    radio(settingsDialog(), 'Custom endpoint').click();
    flushSync();
    settingsDialog().querySelector<HTMLInputElement>('.ai-limits input[type=checkbox]')!.click();
    flushSync();
    expect(settingsDialog().textContent).toContain('Unsaved changes');
    expect(pageSave().disabled).toBe(false);
    pageSave().click();
    await settle();
    const order = commands().filter((c) => c === 'set_app_llm_config' || c === 'set_ai_limits');
    expect(order).toEqual(['set_app_llm_config', 'set_ai_limits']);
    expect(calls('set_ai_limits')[0][1]).toEqual({ settings: { ...LIMITS, automatic_enrichment: true } });
    expect(settingsDialog().textContent).not.toContain('Unsaved changes');
    expect(pageSave().disabled).toBe(true);
  });

  it('closing with unsaved LLM edits asks before discarding', async () => {
    await open('ai');
    radio(settingsDialog(), 'Custom endpoint').click();
    flushSync();
    key(settingsDialog(), 'Escape');
    flushSync();
    expect(guard()).toBeTruthy();
    button(guard()!, 'Keep editing').click();
    await settle();
    expect(get(settingsRoute)).toBe('ai');
    expect(radio(settingsDialog(), 'Custom endpoint').checked).toBe(true);
    settingsDialog().querySelector<HTMLButtonElement>('.close')!.click();
    flushSync();
    button(guard()!, 'Discard').click();
    await settle();
    expect(get(settingsRoute)).toBeNull();
    expect(calls('set_app_llm_config')).toHaveLength(0);
  });

  it("the guard's Save saves, then closes", async () => {
    await open('ai');
    radio(settingsDialog(), 'Custom endpoint').click();
    flushSync();
    key(settingsDialog(), 'Escape');
    flushSync();
    button(guard()!, 'Save').click();
    await settle();
    expect(calls('set_app_llm_config')).toHaveLength(1);
    expect(get(settingsRoute)).toBeNull();
  });

  it('first failing section shows its error and stops', async () => {
    overrides.set_app_llm_config = () => { throw 'disk full'; };
    await open('ai');
    radio(settingsDialog(), 'Custom endpoint').click();
    settingsDialog().querySelector<HTMLInputElement>('.ai-limits input[type=checkbox]')!.click();
    flushSync();
    pageSave().click();
    await settle();
    expect(calls('set_ai_limits')).toHaveLength(0);
    const alert = settingsDialog().querySelector('.save-error[role=alert]')!;
    expect(alert.textContent).toBe('save: disk full');
    // One surface per failure: the page's line, not the section's too.
    expect(occurrences(settingsDialog().textContent!, 'disk full')).toBe(1);
    // Inline above the section that failed: the LLM form follows it.
    expect(alert.compareDocumentPosition(settingsDialog().querySelector('.llm-settings')!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(get(settingsRoute)).toBe('ai');
    expect(pageSave().disabled).toBe(false);
  });

  it('a section refusal (whitespace-only key) is shown above that section, nothing written', async () => {
    overrides.get_app_llm_config = () => ({
      cfg: { llm: { ...APP_LLM, provider: 'http', http_base_url: 'https://example.test', http_model: 'm' }, apply_to_accounts: false },
      has_api_key: null,
    });
    await open('ai');
    button(settingsDialog(), 'Change API key').click();
    flushSync();
    type(settingsDialog().querySelector<HTMLInputElement>('input[autocomplete="new-password"]')!, '   ');
    pageSave().click();
    await settle();
    expect(settingsDialog().querySelector('.save-error')!.textContent).toBe('Enter a valid API key.');
    expect(calls('set_app_llm_config')).toHaveLength(0);
  });

  it('switching sections while dirty goes through the same guard', async () => {
    await open('ai');
    radio(settingsDialog(), 'Custom endpoint').click();
    flushSync();
    button(settingsDialog(), 'General').click();
    await settle();
    expect(guard()).toBeTruthy();
    button(guard()!, 'Keep editing').click();
    await settle();
    expect(pageTitle()).toBe('AI');
    button(settingsDialog(), 'General').click();
    await settle();
    button(guard()!, 'Discard').click();
    await settle();
    expect(get(settingsRoute)).toBe('general');
    expect(pageTitle()).toBe('General');
    expect(settingsDialog().open).toBe(true);
    expect(calls('set_app_llm_config')).toHaveLength(0);
  });

  it('a clean page switches without asking', async () => {
    await open('ai');
    button(settingsDialog(), 'Privacy').click();
    await settle();
    expect(guard()).toBeUndefined();
    expect(pageTitle()).toBe('Privacy');
  });

  it('Remove credentials uses ConfirmDialog, never window.confirm', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm').mockReturnValue(true);
    await open('advanced');
    button(settingsDialog(), 'Remove credentials').click();
    await settle();
    const dlg = dialogs().find((d) => d.textContent?.includes('Remove saved credentials?'))!;
    expect(dlg).toBeTruthy();
    expect(calls('clear_oauth_config')).toHaveLength(0);
    button(dlg, 'Remove credentials').click();
    await settle();
    expect(calls('clear_oauth_config')).toHaveLength(1);
    expect(confirmSpy).not.toHaveBeenCalled();
    const clientId = [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.includes('Client ID'))!.querySelector('input')!;
    expect(clientId.value).toBe('');
  });

  it('Remove credentials: Cancel removes nothing', async () => {
    await open('advanced');
    button(settingsDialog(), 'Remove credentials').click();
    await settle();
    const dlg = dialogs().find((d) => d.textContent?.includes('Remove saved credentials?'))!;
    button(dlg, 'Cancel').click();
    await settle();
    expect(calls('clear_oauth_config')).toHaveLength(0);
  });

  it('Privacy shows hidden folders on Android', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('tablet');
    accounts.set([GMAIL]);
    await open('privacy');
    expect(pageTitle()).toBe('Privacy');
    const details = settingsDialog().querySelector<HTMLDetailsElement>('details:not(.learn-more)')!;
    expect(details.querySelector('summary')!.textContent).toContain('a@x.com');
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    await settle();
    const labels = [...details.querySelectorAll('label.folder')].map((l) => l.textContent?.trim());
    expect(labels).toEqual(['Personal', 'Work']);
    // The agent workspace's enable step stays desktop-only, under AI.
    button(settingsDialog(), 'AI').click();
    await settle();
    expect(settingsDialog().textContent).not.toContain('Agent workspace');
  });

  it('Agent workspace enable stays on the AI page on desktop', async () => {
    await open('ai');
    expect(settingsDialog().textContent).toContain('Agent workspace');
  });

  it('Microsoft OAuth hidden on Android', async () => {
    await open('advanced');
    expect(settingsDialog().textContent).toContain('Microsoft OAuth Client');
    await unmount(app!);
    app = undefined;
    settingsRoute.set(null);
    await settle();
    isAndroid.set(true);
    androidLayoutMode.set('tablet');
    await open('advanced');
    expect(settingsDialog().textContent).toContain('Google OAuth Credentials');
    expect(settingsDialog().textContent).not.toContain('Microsoft OAuth Client');
  });

  it('phone: section opens as a page with ‹ Settings and Save', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    await open('ai');
    const open2 = dialogs();
    expect(open2).toHaveLength(2);
    const [root, page] = open2;
    expect(root.dataset.presentation).toBe('page');
    expect(root.querySelector('h2')!.textContent).toBe('Settings');
    expect(root.textContent).toContain('Pages with text fields have one Save button.');
    expect(page.dataset.presentation).toBe('page');
    expect(page.querySelector('h2')!.textContent).toBe('AI');
    expect(page.querySelector('.back')!.textContent?.trim()).toBe('‹ Settings');
    const save = page.querySelector<HTMLButtonElement>('header .btn.primary')!;
    expect(save.textContent).toBe('Save');
    expect(save.disabled).toBe(true);
    radio(page, 'Custom endpoint').click();
    flushSync();
    expect(save.disabled).toBe(false);
    save.click();
    await settle();
    expect(calls('set_app_llm_config')).toHaveLength(1);
    // ‹ Settings returns to the section list, which stays open.
    page.querySelector<HTMLButtonElement>('.back')!.click();
    await settle();
    expect(get(settingsRoute)).toBe('');
    expect(dialogs()).toHaveLength(1);
  });

  it('phone: Android Back walks account page → Accounts → list → closed', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    await open('accounts/' + GMAIL.id);
    expect(dialogs().map((d) => d.querySelector('h2')!.textContent)).toEqual(['Settings', 'Accounts', 'Account settings']);
    expect(top().querySelector('.back')!.textContent?.trim()).toBe('‹ Accounts');
    history.back();
    await until(() => get(settingsRoute) === 'accounts');
    expect(dialogs()).toHaveLength(2);
    history.back();
    await until(() => get(settingsRoute) === '');
    expect(dialogs()).toHaveLength(1);
    history.back();
    await until(() => get(settingsRoute) === null);
    expect(dialogs()).toHaveLength(0);
  });

  it('phone: Back on a dirty page asks first', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    await open('ai');
    radio(top(), 'Custom endpoint').click();
    flushSync();
    history.back();
    await until(() => !!guard());
    button(guard()!, 'Keep editing').click();
    await settle();
    expect(get(settingsRoute)).toBe('ai');
  });

  it('AuthScreen opens advanced/oauth', async () => {
    const host = document.createElement('div');
    document.body.append(host);
    const auth = mount(AuthScreen, { target: host });
    await settle();
    button(host, 'Configure first').click();
    expect(get(settingsRoute)).toBe('advanced/oauth');
    app = mount(Settings, { target });
    await settle();
    await until(() => pageTitle() === 'Advanced');
    const clientId = [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.includes('Client ID'))!.querySelector('input')!;
    await until(() => document.activeElement === clientId);
    await unmount(auth);
  });

  describe('Speech page: the host confirmation stacks above the guard', () => {
    const baseUrl = () => [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.trim().startsWith('Base URL'))!.querySelector('input')!;
    const hostConfirm = () => dialogs().find((d) => d.querySelector('h2')?.textContent === 'Confirm where audio is sent');

    it('new host → Esc → guard Save → Confirm saves and closes, as the guard intended', async () => {
      await open('speech');
      type(baseUrl(), 'https://api.groq.com/openai/v1');
      key(settingsDialog(), 'Escape');
      flushSync();
      button(guard()!, 'Save').click();
      await settle();
      const confirm = hostConfirm()!;
      expect(confirm).toBeTruthy();
      expect(confirm.textContent).toContain('Audio from video links will be sent to api.groq.com to turn speech into text.');
      expect(calls('set_stt_config')).toHaveLength(0);
      button(confirm, 'Confirm').click();
      await settle();
      expect(calls('set_stt_config')).toHaveLength(1);
      expect(calls('set_stt_config')[0][1]).toMatchObject({ confirm: 'https://api.groq.com' });
      expect(get(settingsRoute)).toBeNull();
    });

    it('Cancel on the host confirmation keeps Settings open with the edit', async () => {
      await open('speech');
      type(baseUrl(), 'https://api.groq.com/openai/v1');
      key(settingsDialog(), 'Escape');
      flushSync();
      button(guard()!, 'Save').click();
      await settle();
      button(hostConfirm()!, 'Cancel').click();
      await settle();
      expect(hostConfirm()).toBeUndefined();
      expect(guard()!.textContent).toContain('Could not save: Speech-to-text host not confirmed.');
      button(guard()!, 'Keep editing').click();
      await settle();
      expect(get(settingsRoute)).toBe('speech');
      expect(baseUrl().value).toBe('https://api.groq.com/openai/v1');
      expect(calls('set_stt_config')).toHaveLength(0);
    });

    it('the page Save asks the same way', async () => {
      await open('speech');
      type(baseUrl(), 'https://api.groq.com/openai/v1');
      pageSave().click();
      await settle();
      button(hostConfirm()!, 'Confirm').click();
      await settle();
      expect(calls('set_stt_config')[0][1]).toMatchObject({ confirm: 'https://api.groq.com' });
      expect(get(settingsRoute)).toBe('speech');
    });
  });

  it('Cmd+Enter saves the page', async () => {
    await open('ai');
    radio(settingsDialog(), 'Custom endpoint').click();
    flushSync();
    const field = settingsDialog().querySelector('.page')!;
    field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', metaKey: true, bubbles: true, cancelable: true }));
    await settle();
    expect(calls('set_app_llm_config')).toHaveLength(1);
  });
});

// ─── Accounts ──────────────────────────────────────────────────────────────

describe('Settings › Accounts', () => {
  it('lists every account with its backend, status dot and duplicate count', async () => {
    accounts.set([GMAIL, LOCAL, OUTLOOK_DRAINING]);
    await open('accounts');
    await settle();
    const rows = [...settingsDialog().querySelectorAll('button.row')];
    expect(rows.map((r) => r.getAttribute('data-account-id'))).toEqual([GMAIL.id, LOCAL.id, OUTLOOK_DRAINING.id]);
    expect(rows[0].textContent).toContain('a@x.com');
    expect(rows[0].textContent).toContain('Gmail');
    expect(rows[0].textContent).toContain('3 duplicates');
    expect(rows[1].textContent).toContain('localfs:a@x');
    expect(rows[1].textContent).not.toContain('duplicates');
    expect(rows[2].querySelector('.dot')!.getAttribute('aria-label')).toBe('Finishing sync');
    expect(rows[0].querySelector('.dot')!.getAttribute('aria-label')).toBe('Active');
  });

  it('the duplicate count is the Sidebar pill’s source (get_dup_stats → dupStats)', async () => {
    accounts.set([GMAIL]);
    await open('accounts');
    expect(calls('get_dup_stats').map((c) => c[1])).toEqual([{ accountId: GMAIL.id }]);
    expect(get(dupStats)[GMAIL.id]).toEqual({ collapsed: 3, uuids_affected: 1 });
  });

  it('a row drills into that account’s page', async () => {
    await open('accounts');
    settingsDialog().querySelector<HTMLButtonElement>(`button.row[data-account-id="${GMAIL.id}"]`)!.click();
    await settle();
    expect(get(settingsRoute)).toBe('accounts/' + GMAIL.id);
    expect(pageTitle()).toBe('Account settings');
    expect(settingsDialog().querySelector<HTMLInputElement>('input[placeholder="Notes"]')!.value).toBe('Notes');
    button(settingsDialog(), '‹ Accounts').click();
    await settle();
    expect(get(settingsRoute)).toBe('accounts');
  });

  it('Add account offers the Sidebar’s kinds on desktop', async () => {
    await open('accounts');
    const labels = [...settingsDialog().querySelectorAll('.add-btn')].map((b) => b.textContent?.trim());
    expect(labels).toEqual([
      'Add Gmail account', 'Add Microsoft account', 'Add iCloud account', 'Add Local Folder',
      'Add SSH Server', 'Add SSH Server (advanced)',
    ]);
  });

  it('Add account on Android: no iCloud, no advanced SSH (as the Sidebar)', async () => {
    isAndroid.set(true);
    supportsIcloud.set(false);
    androidLayoutMode.set('tablet');
    await open('accounts');
    const labels = [...settingsDialog().querySelectorAll('.add-btn')].map((b) => b.textContent?.trim());
    expect(labels).toEqual(['Add Gmail account', 'Add Microsoft account', 'Add Local Folder', 'Add SSH Server']);
  });

  it('signed out (Settings from the sign-in screen): no add kinds that need the add host', async () => {
    isAuthenticated.set(false);
    await open('accounts');
    const labels = [...settingsDialog().querySelectorAll('.add-btn')].map((b) => b.textContent?.trim());
    expect(labels).toEqual(['Add Gmail account', 'Add Microsoft account', 'Add iCloud account']);
  });

  it('Add Local Folder asks the add host and is disabled while it is busy', async () => {
    await open('accounts');
    button(settingsDialog(), 'Add Local Folder').click();
    expect(get(addAccountRequest)?.kind).toBe('localfs');
    localFolderBusy.set(true);
    flushSync();
    expect(button(settingsDialog(), 'Add Local Folder').disabled).toBe(true);
  });

  it('Remove uses the shared confirm and returns to the list', async () => {
    const confirmSpy = vi.spyOn(window, 'confirm');
    await open('accounts/' + GMAIL.id);
    button(settingsDialog(), 'Remove account').click();
    await settle();
    const dlg = top();
    expect(dlg).not.toBe(settingsDialog());
    expect(dlg.textContent).toContain('a@x.com');
    // Destructive: labelled with its verb and danger-styled, never "OK".
    const remove = button(dlg, 'Remove');
    expect(remove.classList.contains('danger')).toBe(true);
    expect(remove.classList.contains('primary')).toBe(false);
    remove.click();
    await settle();
    expect(calls('remove_account')).toHaveLength(1);
    expect(get(accounts).map((a) => a.id)).toEqual([LOCAL.id]);
    expect(get(settingsRoute)).toBe('accounts');
    expect(confirmSpy).not.toHaveBeenCalled();
  });

  it('Remove of the last account closes Settings and drops its duplicate count', async () => {
    accounts.set([GMAIL]);
    dupStats.set({ [GMAIL.id]: { collapsed: 3, uuids_affected: 1 } });
    await open('accounts/' + GMAIL.id);
    button(settingsDialog(), 'Remove account').click();
    await settle();
    button(top(), 'Remove').click();
    await settle();
    expect(calls('remove_account')).toHaveLength(1);
    expect(get(settingsRoute)).toBeNull();
    expect(get(dupStats)[GMAIL.id]).toBeUndefined();
  });

  it('dirty labels → Deactivate → the guard asks; Discard goes to the list', async () => {
    await open('accounts/' + GMAIL.id);
    type(settingsDialog().querySelector<HTMLInputElement>('input[placeholder="Notes"]')!, 'Notes2');
    button(settingsDialog(), 'Deactivate this account').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_account_status', { accountId: GMAIL.id, status: 'draining' });
    expect(guard()).toBeTruthy();
    expect(get(settingsRoute)).toBe('accounts/' + GMAIL.id);
    button(guard()!, 'Discard').click();
    await settle();
    expect(get(settingsRoute)).toBe('accounts');
    expect(calls('update_account_settings')).toHaveLength(0);
  });

  it('dirty LLM → Deactivate → Keep editing stays on the page with the edit', async () => {
    await open('accounts/' + GMAIL.id);
    const box = settingsDialog().querySelector<HTMLInputElement>('.llm-settings input[type="checkbox"]')!;
    box.click();
    flushSync();
    button(settingsDialog(), 'Deactivate this account').click();
    await settle();
    button(guard()!, 'Keep editing').click();
    await settle();
    expect(get(settingsRoute)).toBe('accounts/' + GMAIL.id);
    expect(settingsDialog().querySelector<HTMLInputElement>('.llm-settings input[type="checkbox"]')!.checked).toBe(false);
    expect(settingsDialog().textContent).toContain('Unsaved changes');
  });

  it('phone: dirty account page → Deactivate → the account page asks first', async () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    history.replaceState(null, '', location.href); // a fresh page: no stack for init to unwind
    initPhoneNavHistory();
    await open('accounts/' + GMAIL.id);
    type(top().querySelector<HTMLInputElement>('input[placeholder="Notes"]')!, 'Notes2');
    button(top(), 'Deactivate this account').click();
    await until(() => !!guard());
    button(guard()!, 'Discard').click();
    await until(() => get(settingsRoute) === 'accounts');
    expect(dialogs().map((d) => d.querySelector('h2')!.textContent)).toEqual(['Settings', 'Accounts']);
  });

  it('account actions wait while the page Save runs', async () => {
    let finish!: (v: unknown) => void;
    overrides.update_account_settings = (args) => new Promise((r) => { finish = () => r({ notes_label: args.notesLabel, meta_label: args.metaLabel }); });
    await open('accounts/' + GMAIL.id);
    type(settingsDialog().querySelector<HTMLInputElement>('input[placeholder="Notes"]')!, 'Notes2');
    pageSave().click();
    await settle();
    expect(button(settingsDialog(), 'Deactivate this account').disabled).toBe(true);
    expect(button(settingsDialog(), 'Remove account').disabled).toBe(true);
    finish(null);
    await settle();
    expect(button(settingsDialog(), 'Deactivate this account').disabled).toBe(false);
    expect(button(settingsDialog(), 'Remove account').disabled).toBe(false);
  });

  it('Deactivate flips the account and returns to the list', async () => {
    await open('accounts/' + GMAIL.id);
    button(settingsDialog(), 'Deactivate this account').click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_account_status', { accountId: GMAIL.id, status: 'draining' });
    expect(get(settingsRoute)).toBe('accounts');
  });
});

// ─── Moved from AccountSettings.test.ts ────────────────────────────────────
//
// The Account settings window had two Save buttons, and closing over an
// unsaved LLM edit silently lost it (2026-10-09). Now the page has one Save
// that covers both forms, and leaving it dirty asks first.

describe('Settings › account page — one Save covers labels and the LLM section', () => {
  const dataAllowedBox = () => settingsDialog().querySelector<HTMLInputElement>('.llm-settings input[type="checkbox"]')!;
  const llmWrites = () => calls('update_llm_settings');

  for (const acct of [GMAIL, LOCAL]) {
    it(`${acct.backend_kind}: page Save persists an unsaved data-allowed change`, async () => {
      await open('accounts/' + acct.id);
      dataAllowedBox().click();
      flushSync();
      expect(settingsDialog().textContent).toContain('Unsaved changes');
      pageSave().click();
      await settle();
      const writes = llmWrites();
      expect(writes).toHaveLength(1);
      expect(writes[0][1]).toMatchObject({ accountId: acct.id, cfg: { data_allowed: false } });
      expect(settingsDialog().textContent).not.toContain('Unsaved changes');
    });
  }

  // Since #170 the permission is opt-in: an account with no stored value
  // shows the box unticked, so ticking it is the edit people actually make.
  it('opt-in: ticking the box on an account with no stored value is persisted', async () => {
    const { data_allowed: _, ...unset } = ACCOUNT_LLM;
    overrides.get_llm_settings = () => ({ ...unset });
    await open('accounts/' + GMAIL.id);
    expect(dataAllowedBox().checked).toBe(false);
    dataAllowedBox().click();
    flushSync();
    pageSave().click();
    await settle();
    expect(llmWrites()).toHaveLength(1);
    expect(llmWrites()[0][1]).toMatchObject({ cfg: { data_allowed: true } });
  });

  it('clean LLM section: page Save writes the labels only', async () => {
    await open('accounts/' + GMAIL.id);
    type(settingsDialog().querySelector<HTMLInputElement>('input[placeholder="Notes"]')!, 'Notes2');
    pageSave().click();
    await settle();
    expect(calls('update_account_settings')[0][1]).toEqual({ accountId: GMAIL.id, notesLabel: 'Notes2', metaLabel: 'Notes-Meta' });
    expect(llmWrites()).toHaveLength(0);
  });

  it('LLM write fails: the page stays open and the edit stays on screen', async () => {
    overrides.update_llm_settings = () => { throw new Error('disk full'); };
    await open('accounts/' + GMAIL.id);
    dataAllowedBox().click();
    flushSync();
    pageSave().click();
    await settle();
    expect(get(settingsRoute)).toBe('accounts/' + GMAIL.id);
    expect(dataAllowedBox().checked).toBe(false);
    expect(settingsDialog().textContent).toContain('disk full');
  });

  it('closing over an unsaved LLM edit asks first (the 2026-10-09 loss)', async () => {
    await open('accounts/' + GMAIL.id);
    dataAllowedBox().click();
    flushSync();
    settingsDialog().querySelector<HTMLButtonElement>('.close')!.click();
    flushSync();
    expect(guard()).toBeTruthy();
    button(guard()!, 'Save').click();
    await settle();
    expect(llmWrites()).toHaveLength(1);
    expect(get(settingsRoute)).toBeNull();
  });

  it('local folder: renaming the vault updates the accounts store', async () => {
    await open('accounts/' + LOCAL.id);
    type(settingsDialog().querySelector<HTMLInputElement>('input[placeholder="My Notes"]')!, 'Vault 2');
    pageSave().click();
    await settle();
    expect(calls('rename_local_account')[0][1]).toEqual({ accountId: LOCAL.id, name: 'Vault 2' });
    expect(get(accounts).find((a) => a.id === LOCAL.id)!.email).toBe('Vault 2');
  });
});

// ─── Moved from AppSettings.test.ts ────────────────────────────────────────

describe('Settings › General and Advanced (moved from AppSettings)', () => {
  it('opens and reopens with metadata commands only; unchanged Save does not touch OAuth credentials', async () => {
    await open('advanced');
    settingsRoute.set(null);
    await settle();
    settingsRoute.set('advanced');
    await settle();
    expect(calls('get_oauth_settings')).toHaveLength(2);
    expect(commands()).not.toContain('get_oauth_config');
    expect(commands()).not.toContain('check_app_llm_key');
    // Nothing to save: the page Save is there, and disabled.
    expect(pageSave().textContent).toBe('Save');
    expect(pageSave().disabled).toBe(true);
    pageSave().click();
    await settle();
    expect(commands()).not.toContain('save_oauth_config');
    expect(commands()).not.toContain('save_ms_oauth_config');
  });

  it('a new Client ID without its secret is refused before anything is sent', async () => {
    await open('advanced');
    const clientId = [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.includes('Client ID'))!.querySelector('input')!;
    type(clientId, 'new-id.apps.googleusercontent.com');
    expect(settingsDialog().textContent).toContain('Paste the secret issued for this Client ID.');
    pageSave().click();
    await settle();
    expect(commands()).not.toContain('save_oauth_config');
    expect(settingsDialog().querySelector('.save-error')!.textContent).toContain('The stored secret no longer matches this Client ID');
    // Shown once: the inline warning gives way to the page's error line.
    expect(occurrences(settingsDialog().textContent!, 'The stored secret no longer matches this Client ID')).toBe(1);
  });

  it('keeps the secret warning visible when a save fails for another reason', async () => {
    overrides.save_ms_oauth_config = () => { throw 'ms down'; };
    await open('advanced');
    const clientId = [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.includes('Client ID'))!.querySelector('input')!;
    type(clientId, 'new-id.apps.googleusercontent.com');
    const msId = settingsDialog().querySelector<HTMLInputElement>('input[placeholder="(using the built-in client)"]')!;
    type(msId, 'ms-client-id');
    pageSave().click();
    await settle();
    // The Microsoft write failed first; the page shows that failure ...
    expect(settingsDialog().querySelector('.save-error')!.textContent).toContain('ms down');
    // ... and the Google secret is still missing, so its warning must stay.
    expect(settingsDialog().querySelector('.help.warn')!.textContent).toContain('Paste the secret issued for this Client ID.');
  });

  it('a failed OAuth write is shown once, above the form', async () => {
    overrides.save_ms_oauth_config = () => { throw 'keychain locked'; };
    await open('advanced');
    const ms = [...settingsDialog().querySelectorAll('label')].filter((l) => l.textContent?.includes('Client ID'))[1].querySelector('input')!;
    type(ms, 'my-ms-id');
    pageSave().click();
    await settle();
    expect(settingsDialog().querySelector('.save-error')!.textContent).toBe('keychain locked');
    expect(occurrences(settingsDialog().textContent!, 'keychain locked')).toBe(1);
  });

  function askHistoryBox() {
    return [...settingsDialog().querySelectorAll('label')].find((l) => l.textContent?.includes('Keep Ask Jodd history'))!
      .querySelector('input') as HTMLInputElement;
  }

  it('turns Ask history off, then deletes existing history only when asked', async () => {
    await open('general');
    const box = askHistoryBox();
    expect(box.checked).toBe(true);
    box.click();
    await settle();
    expect(invoke).toHaveBeenCalledWith('set_ask_history_enabled', { enabled: false });
    expect(settingsDialog().textContent).toContain('Also delete existing history?');
    expect(commands()).not.toContain('clear_ask_history');
    button(settingsDialog(), 'Delete history').click();
    await settle();
    expect(commands()).toContain('clear_ask_history');
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
  });

  it('Keep history closes the prompt without deleting', async () => {
    await open('general');
    askHistoryBox().click();
    await settle();
    button(settingsDialog(), 'Keep history').click();
    await settle();
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
    expect(commands()).not.toContain('clear_ask_history');
  });

  it('rolls the Ask history switch back when saving fails', async () => {
    overrides.set_ask_history_enabled = () => { throw 'denied'; };
    await open('general');
    askHistoryBox().click();
    await settle();
    expect(askHistoryBox().checked).toBe(true);
    expect(settingsDialog().textContent).toContain('denied');
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
  });

  it('re-checking the box while the switch-off is still saving takes the Delete offer away at once', async () => {
    await open('general');
    askHistoryBox().click();
    await settle();
    expect(settingsDialog().textContent).toContain('Also delete existing history?');
    let resolveSet!: (v: unknown) => void;
    overrides.set_ask_history_enabled = () => new Promise((r) => { resolveSet = r; });
    askHistoryBox().click(); // back on; set_ask_history_enabled(true) now pending
    flushSync();
    expect(askHistoryBox().checked).toBe(true);
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
    expect(maybeButton(settingsDialog(), 'Delete history')).toBeUndefined();
    resolveSet(null);
    await settle();
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
    expect(commands()).not.toContain('clear_ask_history');
  });

  it('reopening Settings drops a Delete offer left from the last visit', async () => {
    await open('general');
    askHistoryBox().click();
    await settle();
    expect(settingsDialog().textContent).toContain('Also delete existing history?');
    overrides.get_ask_history_enabled = () => false; // still off, so only the reset can hide it
    settingsRoute.set(null);
    await settle();
    settingsRoute.set('general');
    await settle();
    expect(settingsDialog().textContent).not.toContain('Also delete existing history?');
  });

  it('Date display applies at once', async () => {
    await open('general');
    button(settingsDialog(), 'Buddhist (BE)').click();
    flushSync();
    expect(button(settingsDialog(), 'Buddhist (BE)').getAttribute('aria-pressed')).toBe('true');
    expect(localStorage.getItem('jodd.calendar')).toBe('buddhist');
    button(settingsDialog(), 'Automatic').click();
    flushSync();
    expect(localStorage.getItem('jodd.calendar')).toBe('auto');
  });
});

// ─── Moved from AgentWorkspaceSettings.test.ts ─────────────────────────────

describe('Settings › Privacy — hidden folders', () => {
  it('offers only hideable folders', async () => {
    accounts.set([GMAIL]);
    await open('privacy');
    const details = settingsDialog().querySelector<HTMLDetailsElement>('details:not(.learn-more)')!;
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    await settle();
    const labels = [...details.querySelectorAll('label.folder')].map((l) => l.textContent?.trim());
    expect(labels).toEqual(['Personal', 'Work']);
  });

  it('rolls a refused toggle back and says why', async () => {
    accounts.set([GMAIL]);
    overrides.set_folder_hidden_from_agents = () => { throw 'scope file is not valid JSON'; };
    await open('privacy');
    const details = settingsDialog().querySelector<HTMLDetailsElement>('details:not(.learn-more)')!;
    details.open = true;
    details.dispatchEvent(new Event('toggle'));
    await settle();
    const box = details.querySelector<HTMLInputElement>('label.folder input')!;
    box.checked = true;
    // Svelte 5 delegates `change` to the root, so it must bubble — as it does in a browser.
    box.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();
    expect(details.querySelector<HTMLInputElement>('label.folder input')!.checked).toBe(false);
    expect(settingsDialog().textContent).toContain('Could not change Notes/Personal');
  });

  it('shows the AI receipts with their retention control', async () => {
    await open('privacy');
    expect(settingsDialog().textContent).toContain('Execution receipts');
    expect(settingsDialog().querySelector('.ai-receipts select')).toBeTruthy();
  });
});
