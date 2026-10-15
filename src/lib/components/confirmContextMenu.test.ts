// @vitest-environment jsdom
import './__fixtures__/dialogStub';
import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import NoteContextMenu from './NoteContextMenu.svelte';
import { accounts, capabilitiesByAccount, currentAccount, notes } from '../stores/notes';
import type { Note } from '../types';
const invoke = vi.fn().mockResolvedValue([]);
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
let component: ReturnType<typeof mount>;
afterEach(async () => { await unmount(component); document.body.replaceChildren(); invoke.mockClear(); });
it.each([false, true])('settles the nested confirmation once without a delete (batch=%s)', async (batch) => {
  accounts.set([]); currentAccount.set('fixture'); capabilitiesByAccount.set({ fixture: { has_trash: false } });
  const note = { id: 'fixture', uuid: 'fixture', title: 'Fixture', account_id: 'fixture', body_html: '', label: 'Notes' } as Note;
  notes.set([note]); const onClose = vi.fn();
  component = mount(NoteContextMenu, { target: document.body, props: {
    note, x: 0, y: 0, selection: batch ? [note, { ...note, uuid: 'second' }] : [], onClose, onLinkSuggestions: vi.fn(),
  } }); flushSync();
  (document.querySelector('.item.danger') as HTMLButtonElement).click(); flushSync(); await tick();
  const cancel = document.querySelector('dialog button')!;
  cancel.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
  await tick(); flushSync();
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(document.querySelector('dialog')).toBeNull();
  expect(invoke).not.toHaveBeenCalled();
});
