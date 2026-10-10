// @vitest-environment jsdom
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick, flushSync } from 'svelte';
import CurateReview from './CurateReview.svelte';
import { refreshNotes } from '../stores/notes';

const proposal = {
  id: 7, kind: 'duplicate', status: 'pending', created_at: 0, error: null,
  payload: {
    notes: [
      { uuid: 'A', title: 'Course v1', label: 'Notes/Inbox', local_version: 1, date: '', chars: 10 },
      { uuid: 'B', title: 'Course v2', label: 'Notes/Inbox', local_version: 1, date: '', chars: 99 },
    ],
    action: { type: 'keep', keep: 'B' }, reason: 'Same video, v2 is complete.', evidence: ['both cite youtu.be/x'],
  },
};
let listed: unknown[] = [];
let scanResult: Record<string, unknown> | null = null;
const invoke = vi.fn(async (cmd: string, _args?: Record<string, unknown>) => {
  switch (cmd) {
    case 'curate_list': return listed;
    case 'curate_scan': if (scanResult) return scanResult; listed = [proposal]; return { duplicates: 1, misfiled: 0, secrets: 0, skipped: 0, notes: [] };
    case 'curate_preview': return [{ uuid: 'A', title: 'Course v1', label: 'Notes/Inbox', text: 'alpha' }, { uuid: 'B', title: 'Course v2', label: 'Notes/Inbox', text: 'beta' }];
    case 'allow_ai_for_account': return null;
    case 'curate_apply': return null;
    case 'curate_dismiss': return null;
    default: throw new Error(`Unexpected command: ${cmd}`);
  }
});
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: [string, Record<string, unknown>?]) => invoke(...a) }));

let component: ReturnType<typeof mount> | undefined;
let target: HTMLElement;
let refreshes = 0;
beforeEach(() => {
  listed = []; refreshes = 0; scanResult = null;
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
const button = (text: string) => [...target.querySelectorAll('button')].find(b => b.textContent?.trim() === text)!;

it('finds problems, previews, and approves the action the user switched to', async () => {
  component = mount(CurateReview, { target, props: { accountId: 'gmail:a@x.com', onClose: () => {} } });
  await settle();
  button('Find problems').click();
  await settle();
  expect(target.textContent).toContain('Found 1 duplicate group.');
  expect(target.textContent).toContain('Same video, v2 is complete.');

  const select = target.querySelector('select')!;
  const appendInto = [...select.options].findIndex(o => o.textContent?.includes('Append the others into “Course v2”'));
  select.selectedIndex = appendInto;
  select.dispatchEvent(new Event('change', { bubbles: true }));
  await settle();

  button('Preview').click();
  await settle();
  expect(target.querySelector('.merged pre')?.textContent).toBe('beta\n\n———\nMerged from: Course v1\nalpha');

  button('Approve').click();
  await settle();
  const apply = invoke.mock.calls.find(c => c[0] === 'curate_apply')!;
  expect(apply[1]).toEqual({ accountId: 'gmail:a@x.com', id: 7, action: { type: 'append', into: 'B' } });
  expect(refreshes).toBe(1);
  expect(target.querySelector('[data-proposal="7"]')).toBeNull();
});

it('dismiss removes the card without applying', async () => {
  listed = [proposal];
  component = mount(CurateReview, { target, props: { accountId: 'gmail:a@x.com', onClose: () => {} } });
  await settle();
  button('Dismiss').click();
  await settle();
  expect(invoke.mock.calls.map(c => c[0])).not.toContain('curate_apply');
  expect(target.querySelector('[data-proposal="7"]')).toBeNull();
});

it('a scan skipped for consent offers Allow AI for this account', async () => {
  scanResult = { duplicates: 0, misfiled: 0, secrets: 0, skipped: 0, ai: 'consent_needed',
    notes: ['AI data access is not allowed for this account, so only secrets were checked.'] };
  component = mount(CurateReview, { target, props: { accountId: 'gmail:a@x.com', onClose: () => {} } });
  await settle();
  button('Find problems').click();
  await settle();
  expect(target.textContent).toContain('AI data access is not allowed for this account');
  button('Allow AI for this account').click();
  await settle();
  expect(invoke).toHaveBeenCalledWith('allow_ai_for_account', { accountId: 'gmail:a@x.com' });
  expect(button('Allow AI for this account')).toBeUndefined();
});

it('an ordinary scan offers no allow', async () => {
  component = mount(CurateReview, { target, props: { accountId: 'gmail:a@x.com', onClose: () => {} } });
  await settle();
  button('Find problems').click();
  await settle();
  expect(button('Allow AI for this account')).toBeUndefined();
});
