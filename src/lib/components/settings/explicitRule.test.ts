// @vitest-environment jsdom
//
// The Explicit Rule (spec 2026-11-11 Settings › Privacy & Data): a governance fact
// — what leaves the device, what is kept, what is not encrypted, what AI may
// see — is never behind Learn more, never inside a collapsed section, and
// never missing from a platform's screen. Each fact is a constant in
// settings/governanceCopy.ts; this file mounts the pages that show them, on
// desktop and on Android, and checks each one is on screen outside any
// <details>.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '../__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick, type Component } from 'svelte';
import type { Writable } from 'svelte/store';

const invoke = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case 'get_app_llm_config':
      return { cfg: { llm: { provider: 'none' }, apply_to_accounts: false }, has_api_key: null };
    case 'get_llm_settings': return { provider: 'none', data_allowed: false };
    case 'get_stt_config': return { cfg: { base_url: '', confirmed_host: null }, has_api_key: null };
    case 'get_link_settings': return { auto_follow: [] };
    case 'get_youtube_key_status': return null;
    case 'agent_workspace_status':
      return { account_id: null, folder: 'Notes/__Agent__', eligible: [], hidden: {}, scope_path: '/tmp/s.json', error: null };
    case 'agent_files_status': return { enabled: false, path: '', default_path: '/tmp/files', choices: [], error: null };
    case 'agent_skills_view': return { skills_root: '/tmp/skills', skills: [] };
    case 'get_ai_limits':
      return { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
    case 'get_ai_receipt_retention': return 30;
    case 'get_ask_history_enabled': return true;
    case 'get_log_settings': return { file_logging_enabled: true, log_file_path: '/tmp/jodd.log', log_file_size_bytes: 0 };
    case 'backup_statuses': return {};
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

import DataPage from './DataPage.svelte';
import BackupPage from './BackupPage.svelte';
import LinksPage from './LinksPage.svelte';
import AgentFilesSettings from '../AgentFilesSettings.svelte';
import * as copy from '../../settings/governanceCopy';
import { isAndroid as isAndroidReadonly } from '../../stores/platform';
import { accounts } from '../../stores/notes';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;

let host: HTMLElement;
let comp: ReturnType<typeof mount> | undefined;

async function settle(rounds = 8) {
  for (let i = 0; i < rounds; i++) {
    await tick();
    await new Promise((r) => setTimeout(r, 0));
  }
  flushSync();
}

async function show(c: Component<any>, props: Record<string, unknown> = {}) {
  comp = mount(c, { target: host, props });
  await settle();
}

/** On screen: in a <p> or <span> with no <details> around it (closed or not). */
function hiddenFacts(facts: string[]): string[] {
  const shown = [...host.querySelectorAll('p, span')]
    .filter((el) => !el.closest('details'))
    .map((el) => el.textContent!.replace(/\s+/g, ' '));
  return facts.filter((f) => !shown.some((t) => t.includes(f)));
}

beforeEach(() => {
  invoke.mockClear();
  isAndroid.set(false);
  accounts.set([{ id: 'gmail:a@x.com', email: 'a@x.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail' }]);
  host = document.createElement('div');
  document.body.appendChild(host);
});
afterEach(() => {
  if (comp) unmount(comp);
  comp = undefined;
  host.remove();
});

const DATA_PAGE_FACTS = [
  copy.PER_DEVICE, copy.MASKING, copy.MASKING_KINDS, copy.MASKING_LIMITS,
  copy.ALLOW_COVERS, copy.HIDDEN_FOLDERS_HELP, copy.ENRICHMENT_RUNS_ALONE,
  copy.RECEIPTS_KEEP, copy.ASK_HISTORY_KEEPS, copy.LOGS_DEFAULT_ON, copy.LOGS_HOLD, copy.LOGS_TRIMMED,
];

describe('the Explicit Rule — governance facts stay on screen', () => {
  for (const android of [false, true]) {
    const where = android ? 'Android' : 'desktop';

    it(`Privacy & Data shows every fact on ${where}`, async () => {
      isAndroid.set(android);
      await show(DataPage, { saveError: null, leave: vi.fn(async () => true) });
      // Desktop-only channels name their reach on desktop and say they are
      // absent on Android, rather than vanishing (E15).
      const platform = android ? [copy.NOT_ON_THIS_DEVICE] : [copy.AGENTS_READ, copy.AGENTS_OWN_RULES];
      expect(hiddenFacts([...DATA_PAGE_FACTS, ...platform])).toEqual([]);
    });

    it(`Backup says backups are plain files on ${where}`, async () => {
      isAndroid.set(android);
      await show(BackupPage);
      expect(hiddenFacts([copy.BACKUPS_PLAIN, copy.BACKUPS_REMOVE_KEEPS])).toEqual([]);
    });

    it(`Links says commenters are never saved on ${where}`, async () => {
      isAndroid.set(android);
      await show(LinksPage, { saveError: null });
      expect(hiddenFacts([copy.LINKS_COMMENTERS])).toEqual([]);
    });
  }

  // Desktop only: the copy on disk does not exist on Android, and Privacy & Data
  // says so there (checked above).
  it('the notes-on-disk copy says it is not encrypted', async () => {
    await show(AgentFilesSettings);
    expect(hiddenFacts([copy.NOTES_ON_DISK_PLAIN])).toEqual([]);
  });

  it('every fact fits one line of help', () => {
    const long = Object.entries(copy).filter(([, v]) => typeof v === 'string' && v.length > 110);
    expect(long).toEqual([]);
  });
});
