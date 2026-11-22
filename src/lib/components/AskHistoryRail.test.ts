// @vitest-environment jsdom
// Named AskHistoryRail.test.ts, not AskHistory.test.ts: on a case-insensitive
// filesystem (macOS, Windows) that would be the same path as Task 5's
// askHistory.test.ts and silently overwrite it.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';
import { get } from 'svelte/store';
import AskHistory from './AskHistory.svelte';
import { error } from '../stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

const NOW = Date.now();
const ACTIVE = [
  { id: 'c1', title: 'Why keep-both?', scope: { kind: 'all_accounts' }, updated_at: NOW, archived: false, turn_count: 2 },
  { id: 'c2', title: 'Gmail quota', scope: { kind: 'all_accounts' }, updated_at: NOW - 1000, archived: false, turn_count: 1 },
];
const ARCHIVED = [{ id: 'c3', title: 'Old one', scope: { kind: 'all_accounts' }, updated_at: 1, archived: true, turn_count: 1 }];

async function settle() { for (let i = 0; i < 10; i++) await Promise.resolve(); flushSync(); }
function btn(root: HTMLElement, text: string) {
  return Array.from(root.querySelectorAll('button')).find((b) => b.textContent?.trim() === text) as HTMLButtonElement;
}
async function render(props: Record<string, unknown> = {}) {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const handlers = { onSelect: vi.fn(), onNew: vi.fn(), onChanged: vi.fn() };
  const host = mount(AskHistory, { target, props: { activeId: null, refreshKey: 0, ...handlers, ...props } });
  await settle();
  return { target, host, ...handlers };
}

describe('AskHistory', () => {
  beforeEach(() => {
    invoke.mockReset();
    error.set(null);
    invoke.mockImplementation((cmd: string, args: { archived?: boolean }) => {
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? ARCHIVED : ACTIVE);
      return Promise.resolve(null);
    });
  });

  it('lists active conversations and counts archived ones', async () => {
    const { target, host } = await render();
    expect(Array.from(target.querySelectorAll('.ask-row-title')).map((e) => e.textContent)).toEqual(['Why keep-both?', 'Gmail quota']);
    expect(btn(target, 'Archived (1)')).toBeTruthy();
    expect(target.textContent).not.toContain('Old one');
    btn(target, 'Archived (1)').click(); flushSync();
    expect(target.textContent).toContain('Old one');
    unmount(host);
  });

  it('treats a null list as empty', async () => {
    invoke.mockImplementation(() => Promise.resolve(null));
    const { target, host } = await render();
    expect(target.querySelectorAll('.ask-row')).toHaveLength(0);
    expect(btn(target, '+ New')).toBeTruthy();
    unmount(host);
  });

  it('selects a row and starts a new one', async () => {
    const { target, host, onSelect, onNew } = await render();
    (target.querySelector('.ask-row[data-id="c2"] .ask-row-title') as HTMLElement).click();
    expect(onSelect).toHaveBeenCalledWith('c2');
    btn(target, '+ New').click();
    expect(onNew).toHaveBeenCalled();
    unmount(host);
  });

  it('archives optimistically and rolls back on failure', async () => {
    let fail!: (e: unknown) => void;
    invoke.mockImplementation((cmd: string, args: { archived?: boolean }) => {
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? ARCHIVED : ACTIVE);
      if (cmd === 'set_ask_conversation_archived') return new Promise((_, rej) => { fail = rej; });
      return Promise.resolve(null);
    });
    const { target, host, onChanged } = await render();
    (target.querySelector('.ask-row[data-id="c1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Archive').click(); flushSync();
    // Moved before the IPC settles.
    expect(target.querySelector('.ask-row[data-id="c1"]')).toBeNull();
    expect(btn(target, 'Archived (2)')).toBeTruthy();
    fail('disk full'); await settle();
    expect(target.querySelector('.ask-row[data-id="c1"]')).toBeTruthy();
    expect(btn(target, 'Archived (1)')).toBeTruthy();
    expect(String(get(error))).toContain('disk full');
    expect(onChanged).not.toHaveBeenCalled();
    unmount(host);
  });

  it('deletes only after the inline confirmation', async () => {
    const { target, host, onChanged } = await render();
    (target.querySelector('.ask-row[data-id="c2"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Delete').click(); flushSync();
    expect(invoke.mock.calls.some((c) => c[0] === 'delete_ask_conversation')).toBe(false);
    btn(target, 'Delete conversation').click(); flushSync();
    expect(target.querySelector('.ask-row[data-id="c2"]')).toBeNull();
    await settle();
    expect(invoke).toHaveBeenCalledWith('delete_ask_conversation', { conversationId: 'c2' });
    expect(onChanged).toHaveBeenCalledWith('c2', 'deleted');
    unmount(host);
  });

  it('renames in place with the normalized title the backend returns', async () => {
    invoke.mockImplementation((cmd: string, args: { archived?: boolean }) => {
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? ARCHIVED : ACTIVE);
      if (cmd === 'rename_ask_conversation') return Promise.resolve('New name');
      return Promise.resolve(null);
    });
    const { target, host, onChanged } = await render();
    (target.querySelector('.ask-row[data-id="c1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Rename').click(); flushSync();
    const input = target.querySelector('.ask-row[data-id="c1"] input') as HTMLInputElement;
    input.value = '  New   name '; input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); flushSync();
    await settle();
    expect(invoke).toHaveBeenCalledWith('rename_ask_conversation', { conversationId: 'c1', title: '  New   name ' });
    expect(target.querySelector('.ask-row[data-id="c1"] .ask-row-title')?.textContent).toBe('New name');
    expect(onChanged).toHaveBeenCalledWith('c1', 'renamed');
    unmount(host);
  });

  it('sends one rename IPC even when blur follows the Enter commit', async () => {
    invoke.mockImplementation((cmd: string, args: { archived?: boolean }) => {
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? ARCHIVED : ACTIVE);
      if (cmd === 'rename_ask_conversation') return Promise.resolve('New name');
      return Promise.resolve(null);
    });
    const { target, host, onChanged } = await render();
    (target.querySelector('.ask-row[data-id="c1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Rename').click(); flushSync();
    const input = target.querySelector('.ask-row[data-id="c1"] input') as HTMLInputElement;
    input.value = 'New name'; input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    // A browser fires blur while the input is being removed, i.e. before the
    // re-render settles; jsdom does not, so send it explicitly at that point.
    input.dispatchEvent(new Event('blur'));
    flushSync();
    await settle();
    const renames = invoke.mock.calls.filter((c) => c[0] === 'rename_ask_conversation');
    expect(renames).toHaveLength(1);
    expect(onChanged).toHaveBeenCalledTimes(1);
    unmount(host);
  });

  it('rolls back only the failed row when archive operations overlap', async () => {
    const pending: Record<string, { resolve: () => void; reject: (e: unknown) => void }> = {};
    invoke.mockImplementation((cmd: string, args: { archived?: boolean; conversationId?: string }) => {
      if (cmd === 'list_ask_conversations') return Promise.resolve(args.archived ? ARCHIVED : ACTIVE);
      if (cmd === 'set_ask_conversation_archived') {
        return new Promise<void>((resolve, reject) => { pending[args.conversationId!] = { resolve, reject }; });
      }
      return Promise.resolve(null);
    });
    const { target, host, onChanged } = await render();
    (target.querySelector('.ask-row[data-id="c1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Archive').click(); flushSync();
    (target.querySelector('.ask-row[data-id="c2"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Archive').click(); flushSync();
    pending.c2.resolve(); await settle();
    pending.c1.reject('disk full'); await settle();
    expect(target.querySelector('.ask-row[data-id="c1"]')).toBeTruthy();
    expect(target.querySelector('.ask-row[data-id="c2"]')).toBeNull();
    expect(btn(target, 'Archived (2)')).toBeTruthy();
    btn(target, 'Archived (2)').click(); flushSync();
    expect(target.querySelector('.ask-row[data-id="c2"]')).toBeTruthy();
    expect(onChanged).toHaveBeenCalledWith('c2', 'archived');
    expect(onChanged).not.toHaveBeenCalledWith('c1', 'archived');
    unmount(host);
  });

  it('keeps the row archived and sets no error when onChanged throws after success', async () => {
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
    const onChanged = vi.fn(() => { throw new Error('parent blew up'); });
    const { target, host } = await render({ onChanged });
    (target.querySelector('.ask-row[data-id="c1"] .ask-row-menu') as HTMLButtonElement).click(); flushSync();
    btn(target, 'Archive').click(); flushSync();
    await settle();
    expect(onChanged).toHaveBeenCalledWith('c1', 'archived');
    expect(target.querySelector('.ask-row[data-id="c1"]')).toBeNull();
    expect(btn(target, 'Archived (2)')).toBeTruthy();
    expect(get(error)).toBeNull();
    expect(errSpy).toHaveBeenCalled();
    errSpy.mockRestore();
    unmount(host);
  });
});

// S23 FE live pass, 2026-10-15: the row menu hung below the row with
// `position: absolute` inside the rail's `overflow-y: auto`, so the rail
// clipped it — on a phone the only row's menu was invisible and Delete
// unreachable. jsdom does no layout, so pin the stylesheet instead.
describe('AskHistory row menu layout', () => {
  it('keeps the actions in the row flow, never absolutely positioned', async () => {
    const { readFileSync } = await import('node:fs');
    const src = readFileSync('src/lib/components/AskHistory.svelte', 'utf8');
    const rule = src.match(/\.ask-row-actions\s*\{([^}]*)\}/);
    expect(rule, '.ask-row-actions rule must exist').toBeTruthy();
    expect(rule![1]).not.toMatch(/position\s*:\s*(absolute|fixed)/);
  });
});
