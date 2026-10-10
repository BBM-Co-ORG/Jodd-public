// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import AgentWorkspaceSettings from './AgentWorkspaceSettings.svelte';
import { accounts, refreshNotes } from '../stores/notes';

let enabled = false;
let hidden: Record<string, string[]> = {};
let refuseHide = false;
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
    case 'list_folders':
      return ['Notes', 'Notes/__Agent__', 'Notes/__Agent__/Projects', 'Notes/Personal', 'Notes/Work'];
    case 'set_folder_hidden_from_agents':
      if (refuseHide) throw 'scope file is not valid JSON';
      return null;
    default:
      throw new Error(`Unexpected command: ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: [string, Record<string, unknown>?]) => invoke(...a) }));

let component: ReturnType<typeof mount> | undefined;
let target: HTMLElement;
beforeEach(() => {
  enabled = false; hidden = {}; refuseHide = false;
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

it('enables the workspace and then offers only hideable folders', async () => {
  component = mount(AgentWorkspaceSettings, { target });
  await settle();
  button('Enable agent workspace')!.click();
  await settle();
  expect(target.textContent).toContain('Enabled: Notes/__Agent__ in a@x.com');
  // The new pages must reach the note list without a manual refresh (gotcha #6).
  expect(refreshes).toBe(1);

  const details = target.querySelector('details')!;
  details.open = true;
  details.dispatchEvent(new Event('toggle'));
  await settle();
  const labels = [...details.querySelectorAll('label.folder')].map(l => l.textContent?.trim());
  expect(labels).toEqual(['Personal', 'Work']);
});

it('rolls a refused toggle back and says why', async () => {
  enabled = true;
  refuseHide = true;
  component = mount(AgentWorkspaceSettings, { target });
  await settle();
  const details = target.querySelector('details')!;
  details.open = true;
  details.dispatchEvent(new Event('toggle'));
  await settle();
  const box = details.querySelector<HTMLInputElement>('label.folder input')!;
  box.checked = true;
  // Svelte 5 delegates `change` to the root, so it must bubble — as it does in a browser.
  box.dispatchEvent(new Event('change', { bubbles: true }));
  await settle();
  expect(details.querySelector<HTMLInputElement>('label.folder input')!.checked).toBe(false);
  expect(target.textContent).toContain('Could not change Notes/Personal');
});
