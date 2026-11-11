// @vitest-environment jsdom
//
// LinkSuggestionsModal renders through the shared Modal: Esc and the header
// close are Modal's, "Add to N notes" is its one primary, "Skip all" a footer
// secondary.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
vi.mock('../unreviewed', () => ({ refreshUnreviewed: vi.fn() }));

import LinkSuggestionsModal from './LinkSuggestionsModal.svelte';

const APPENDS = [
  { ai_result_id: 'r1', uuid: 'u1', title: 'First', addition_text: '[[Second]]' },
  { ai_result_id: 'r2', uuid: 'u2', title: 'Second', addition_text: '[[First]]' },
];

let host: HTMLElement;
let component: ReturnType<typeof mount>;
let onClose: ReturnType<typeof vi.fn<() => void>>;
let onApplied: ReturnType<typeof vi.fn<(appliedUuids: string[]) => void>>;

beforeEach(() => {
  invoke.mockReset();
  invoke.mockImplementation((cmd: string) =>
    cmd === 'apply_wiki_link_appends' ? Promise.resolve(['u1', 'u2']) : Promise.resolve('macos'),
  );
  onClose = vi.fn<() => void>();
  onApplied = vi.fn<(appliedUuids: string[]) => void>();
  host = document.createElement('div');
  document.body.append(host);
  component = mount(LinkSuggestionsModal, {
    target: host,
    props: { accountId: 'gmail:a@b.com', proposedAppends: APPENDS, onClose, onApplied },
  });
  flushSync();
});
afterEach(() => { unmount(component); host.remove(); });

const dialog = () => host.querySelector('dialog')!;
const button = (text: RegExp) =>
  [...dialog().querySelectorAll<HTMLButtonElement>('button')].find((b) => text.test(b.textContent?.trim() ?? ''))!;

describe('LinkSuggestionsModal as a Modal', () => {
  it('is an md Modal titled Related notes found, with Apply primary and Skip all', () => {
    expect(dialog().hasAttribute('data-presentation')).toBe(true);
    expect(dialog().dataset.size).toBe('md');
    expect(dialog().querySelector('h2')!.textContent).toBe('Related notes found');
    expect(button(/^Add to 2 notes$/).classList.contains('primary')).toBe(true);
    expect(button(/^Skip all$/).classList.contains('primary')).toBe(false);
  });

  it('Esc closes (new behaviour)', () => {
    dialog().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Skip all closes without applying', () => {
    button(/^Skip all$/).click();
    flushSync();
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalledWith('apply_wiki_link_appends', expect.anything());
  });

  it('Apply sends the selected appends, then reports and closes', async () => {
    dialog().querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[1].click();
    flushSync();
    expect(button(/^Add to 1 note$/)).toBeTruthy();
    button(/^Add to 1 note$/).click();
    for (let i = 0; i < 5; i++) await tick();
    expect(invoke).toHaveBeenCalledWith('apply_wiki_link_appends', {
      accountId: 'gmail:a@b.com',
      appends: [{ ai_result_id: 'r1', uuid: 'u1', addition_text: '[[Second]]' }],
    });
    expect(onApplied).toHaveBeenCalledWith(['u1', 'u2']);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Apply is disabled when nothing is selected', () => {
    dialog().querySelectorAll<HTMLInputElement>('input[type="checkbox"]').forEach((c) => c.click());
    flushSync();
    expect(button(/^Nothing selected$/).disabled).toBe(true);
  });
});
