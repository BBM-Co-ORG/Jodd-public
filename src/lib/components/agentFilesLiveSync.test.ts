// @vitest-environment jsdom
// Gotcha #6 for the Claude Code bridge's live sync (spec 2026-11-05 §10 Phase
// 3): a run the app does on its own must reach both Settings sections. The
// route is the `agent-files-synced` event; these tests fire it and check that
// what is on screen is read again and repainted.
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import AgentFilesSettings from './AgentFilesSettings.svelte';
import AgentSkillsSettings from './AgentSkillsSettings.svelte';
import { AGENT_FILES_SYNCED } from '../agentFiles';

let written = 0;
let skillState = 'not_published';
const invoke = vi.fn(async (cmd: string) => {
  switch (cmd) {
    case 'agent_files_status':
      return {
        enabled: true, path: '/h/Jodd for agents', default_path: '/h/Jodd for agents',
        accounts: ['gmail:a@x.com'], choices: [{ account_id: 'gmail:a@x.com', email: 'a@x.com' }],
        last: {
          finished_at: '2026-11-05T09:12:03+07:00', outcome: 'ok', path: '/h/Jodd for agents', problems: [],
          accounts: [{ account_id: 'gmail:a@x.com', notes: 10, written, deleted: 0, masked_notes: 0, skipped_attachments: 0, overwritten_local_edits: 0, held_back: false }],
        },
        error: null, notice: null,
      };
    case 'agent_skills_view':
      return {
        skills_root: '/h/.claude/skills', last_finished_at: null, problems: [], error: null,
        skills: [{
          name: 'brand', account_id: 'gmail:a@x.com', folder: 'Notes/Brand', assets_dir: null, only_reviewed: false, email: 'a@x.com',
          status: { name: 'brand', account_id: 'gmail:a@x.com', folder: 'Notes/Brand', state: skillState, waiting_for_review: [], problems: [], files: 3 },
        }],
      };
    default:
      throw new Error(`Unexpected command: ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: [string]) => invoke(...a) }));

const listeners = new Map<string, Set<() => void>>();
const unlistened: string[] = [];
vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, cb: () => void) => {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event)!.add(cb);
    return Promise.resolve(() => { listeners.get(event)!.delete(cb); unlistened.push(event); });
  },
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));

function fire(event: string) {
  for (const cb of listeners.get(event) ?? []) cb();
}

let component: ReturnType<typeof mount> | undefined;
let target: HTMLElement;
beforeEach(() => {
  written = 0; skillState = 'not_published';
  listeners.clear(); unlistened.length = 0;
  target = document.createElement('div');
  document.body.append(target);
});
afterEach(async () => {
  if (component) await unmount(component);
  component = undefined;
  document.body.innerHTML = '';
  invoke.mockClear();
});
async function settle() { for (let i = 0; i < 12; i++) await tick(); flushSync(); }

it('the mirror section repaints when the app syncs on its own', async () => {
  component = mount(AgentFilesSettings, { target });
  await settle();
  expect(target.textContent).toContain('0 updated in the last sync');
  written = 3;
  fire(AGENT_FILES_SYNCED);
  await settle();
  expect(target.textContent).toContain('3 updated in the last sync');
  await unmount(component); component = undefined;
  expect(unlistened).toContain(AGENT_FILES_SYNCED);
});

it('the skills section repaints when the app syncs on its own', async () => {
  component = mount(AgentSkillsSettings, { target });
  await settle();
  expect(target.textContent).toContain('Not updated.');
  skillState = 'published';
  fire(AGENT_FILES_SYNCED);
  await settle();
  expect(target.textContent).toContain('Published as jodd-brand (3 files).');
  await unmount(component); component = undefined;
  expect(unlistened).toContain(AGENT_FILES_SYNCED);
});
