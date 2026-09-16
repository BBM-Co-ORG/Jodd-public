// @vitest-environment jsdom
//
// Regression guard for the recovery banner's account filter.
//
// After `recover_from_key_mismatch` quarantines an undecryptable DB, the local
// note cache is empty and the banner offers to re-index. Only *remote-backed*
// accounts can be re-indexed: a LocalFs vault's notes live in .eml files the
// backend re-reads, and calling `index_account` on it is at best pointless.
// The predicate is a bare string comparison against the serialized
// `backend_kind`, and that exact class of bug (casing/spelling drift between
// the Rust enum's serde name and the string the component tests for) has
// already been caught once by hand in review. This test pins it.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import ReindexRecoveryBanner from './ReindexRecoveryBanner.svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

const GMAIL = { id: 'jodd.demo@gmail.com', backend_kind: 'gmail' };
const LOCAL_FS = { id: '/Users/x/vault', backend_kind: 'local_fs' };

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(ReindexRecoveryBanner, { target });
  flushSync();
  return { target, host };
}

function button(target: HTMLElement, label: string): HTMLButtonElement {
  const found = [...target.querySelectorAll('button')].find((b) =>
    (b.textContent ?? '').includes(label),
  );
  if (!found) throw new Error(`no button matching ${label}: ${target.innerHTML}`);
  return found as HTMLButtonElement;
}

describe('ReindexRecoveryBanner', () => {
  beforeEach(() => {
    invoke.mockReset();
    document.body.innerHTML = '';
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'needs_reindex_after_recovery':
          return Promise.resolve(true);
        case 'list_accounts':
          return Promise.resolve([GMAIL, LOCAL_FS]);
        case 'index_account':
        case 'clear_reindex_marker':
          return Promise.resolve(null);
        default:
          throw new Error(`unexpected command ${cmd}`);
      }
    });
  });

  it('re-indexes only remote-backed accounts, never LocalFs vaults', async () => {
    const { target, host } = render();
    await tick(); // let onMount's needs_reindex_after_recovery resolve
    flushSync();

    button(target, 'Re-index now').click();
    // reindexAll awaits list_accounts, then one invoke per gmail account, then
    // clear_reindex_marker — drain the microtask queue until it settles.
    for (let i = 0; i < 10; i++) await tick();
    flushSync();

    const indexCalls = invoke.mock.calls.filter(([cmd]) => cmd === 'index_account');
    expect(indexCalls).toHaveLength(1);
    expect(indexCalls[0][1]).toEqual({ accountId: GMAIL.id });
    expect(invoke.mock.calls.some(([cmd]) => cmd === 'clear_reindex_marker')).toBe(true);

    unmount(host);
  });

  it('stays hidden when no recovery happened', async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === 'needs_reindex_after_recovery' ? Promise.resolve(false) : Promise.resolve(null),
    );
    const { target, host } = render();
    await tick();
    flushSync();
    expect(target.querySelector('.reindex-banner')).toBeNull();
    unmount(host);
  });
});
