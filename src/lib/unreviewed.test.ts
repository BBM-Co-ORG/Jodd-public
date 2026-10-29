import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
import { unreviewedTrust, unreviewedKey, type Trust } from './stores/notes';
import { refreshUnreviewed, markReviewed, isAgentWorkspace, relativeAgo } from './unreviewed';

const T: Trust = { tier: 'unreviewed', by: 'claude-code/2.1.0', at: '2026-10-07T01:00:00Z', event_id: 7, unreviewed_by: 'claude-code/2.1.0', unreviewed_at: '2026-10-07T01:00:00Z', created_by: 'claude-code/2.1.0', unrecorded_change: false, note_local_version: 3 };
const entry = (account: string, trust: Trust = T) => ({ account, trust });

describe('unreviewed trust', () => {
  beforeEach(() => { invoke.mockReset(); unreviewedTrust.set({}); });

  it('replaces only the refreshed account', async () => {
    unreviewedTrust.set({ [unreviewedKey('b', 'X')]: entry('b'), [unreviewedKey('a', 'old')]: entry('a') });
    invoke.mockResolvedValue([{ uuid: 'U', trust: T }]);
    await refreshUnreviewed(['a']);
    expect(invoke).toHaveBeenCalledWith('list_unreviewed_trust', { accountId: 'a' });
    expect(Object.keys(get(unreviewedTrust)).sort()).toEqual([unreviewedKey('a', 'U'), unreviewedKey('b', 'X')].sort());
  });

  it('one failing account does not stop the others', async () => {
    invoke.mockImplementation((_c: string, a: { accountId: string }) =>
      a.accountId === 'a' ? Promise.reject('boom') : Promise.resolve([{ uuid: 'U', trust: T }]));
    await refreshUnreviewed(['a', 'b']);
    expect(Object.keys(get(unreviewedTrust))).toEqual([unreviewedKey('b', 'U')]);
  });

  it('mark reviewed removes optimistically and restores on failure', async () => {
    unreviewedTrust.set({ [unreviewedKey('a', 'U')]: entry('a') });
    let reject!: (v: unknown) => void;
    invoke.mockReturnValue(new Promise((_r, j) => (reject = j)));
    const p = markReviewed('a', 'U');
    expect(get(unreviewedTrust)[unreviewedKey('a', 'U')]).toBeUndefined(); // synchronous, before the IPC settles
    reject('boom');
    await expect(p).resolves.toBe('failed');
    expect(get(unreviewedTrust)[unreviewedKey('a', 'U')]).toEqual(entry('a'));
    expect(invoke).toHaveBeenCalledWith('verify_note', { accountId: 'a', uuid: 'U', seenEventId: 7, seenLocalVersion: 3 });
  });

  it('stale puts back the fresh trust', async () => {
    unreviewedTrust.set({ [unreviewedKey('a', 'U')]: entry('a') });
    const fresh = { ...T, by: 'other/2', event_id: 9 };
    invoke.mockResolvedValue({ kind: 'stale', trust: fresh });
    await expect(markReviewed('a', 'U')).resolves.toBe('stale');
    expect(get(unreviewedTrust)[unreviewedKey('a', 'U')]).toEqual(entry('a', fresh));
  });

  // The chip's version is when its trust was read; the editor may show an
  // older body. Binding the verify to what was rendered lets Db::verify_note
  // refuse content the user has not seen even if the UI races.
  it('sends the version the editor rendered when it has one', async () => {
    unreviewedTrust.set({ [unreviewedKey('a', 'U')]: entry('a', { ...T, note_local_version: 4 }) });
    invoke.mockResolvedValue({ kind: 'stale', trust: T });
    await markReviewed('a', 'U', 3);
    expect(invoke).toHaveBeenCalledWith('verify_note', { accountId: 'a', uuid: 'U', seenEventId: 7, seenLocalVersion: 3 });
  });

  it('verified leaves it removed', async () => {
    unreviewedTrust.set({ [unreviewedKey('a', 'U')]: entry('a') });
    invoke.mockResolvedValue({ kind: 'verified', trust: { ...T, tier: 'human_reviewed' } });
    await expect(markReviewed('a', 'U')).resolves.toBe('verified');
    expect(get(unreviewedTrust)[unreviewedKey('a', 'U')]).toBeUndefined();
  });

  it('knows the agent workspace, and only it', () => {
    expect(isAgentWorkspace('Notes/__Agent__')).toBe(true);
    expect(isAgentWorkspace('Notes/__Agent__/Projects')).toBe(true);
    expect(isAgentWorkspace('Notes/__Agent__x')).toBe(false);
    expect(isAgentWorkspace('Notes')).toBe(false);
  });

  it('formats relative time', () => {
    const now = Date.parse('2026-10-07T12:00:00Z');
    expect(relativeAgo('2026-10-07T11:59:40Z', now)).toBe('just now');
    expect(relativeAgo('2026-10-07T11:30:00Z', now)).toBe('30 min ago');
    expect(relativeAgo('2026-10-07T09:00:00Z', now)).toBe('3 h ago');
    expect(relativeAgo('2026-10-04T12:00:00Z', now)).toBe('3 d ago');
    expect(relativeAgo('garbage', now)).toBe('');
  });
});
