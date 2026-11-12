// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import AgentWorkspaceSettings from './AgentWorkspaceSettings.svelte';
import { accounts, refreshNotes } from '../stores/notes';

let enabled = false;
let hidden: Record<string, string[]> = {};
let refreshes = 0;
const invoke = vi.fn(async (cmd: string, args?: Record<string, unknown>) => {
  switch (cmd) {
    case 'agent_workspace_status':
      return {
        account_id: enabled ? 'gmail:a@x.com' : null,
        folder: 'Notes/__Agent__',
        eligible: [{ account_id: 'gmail:a@x.com', email: 'a@x.com' }],
        hidden,
        scope_path: '/tmp/mcp_write_scope.json',
        error: null,
      };
    case 'enable_agent_workspace':
      expect(args).toEqual({ accountId: 'gmail:a@x.com' });
      enabled = true;
      return null;
    default:
      throw new Error(`Unexpected command: ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: [string, Record<string, unknown>?]) => invoke(...a) }));

let component: ReturnType<typeof mount> | undefined;
let target: HTMLElement;
beforeEach(() => {
  enabled = false; hidden = {};
  accounts.set([{ id: 'gmail:a@x.com', email: 'a@x.com', added_at: '2026-01-01T00:00:00Z' }]);
  refreshes = 0;
  refreshNotes.set(() => { refreshes++; });
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
const button = (text: string) => [...target.querySelectorAll('button')].find(b => b.textContent?.includes(text));

it('enables the workspace and shows where it lives', async () => {
  component = mount(AgentWorkspaceSettings, { target });
  await settle();
  button('Enable agent workspace')!.click();
  await settle();
  expect(target.textContent).toContain('Enabled: Notes/__Agent__ in a@x.com');
  // The new pages must reach the note list without a manual refresh (gotcha #6).
  expect(refreshes).toBe(1);
  // The hidden-folder list moved to Settings › Privacy (settings.test.ts).
  expect(target.querySelector('details:not(.learn-more)')).toBeNull();
});
