// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { tvs, loadTvs, defaultTv, type TvRow } from './tvs';

const row = (o: Partial<TvRow>): TvRow => ({
  tv_id: 'a', label: 'Living room', model: null, is_default: false, needs_repair: false, last_status: null, ...o,
});

describe('tvs store', () => {
  beforeEach(() => {
    invoke.mockReset();
    tvs.set([]);
    vi.spyOn(console, 'warn').mockImplementation(() => {});
  });

  it('loadTvs fills the store from list_tvs', async () => {
    invoke.mockResolvedValue([row({ is_default: true })]);
    await loadTvs();
    expect(invoke).toHaveBeenCalledWith('list_tvs');
    expect(get(tvs)).toHaveLength(1);
  });

  it('loadTvs keeps the last list when the command fails', async () => {
    tvs.set([row({})]);
    invoke.mockRejectedValue(new Error('boom'));
    await loadTvs();
    expect(get(tvs)).toHaveLength(1);
  });

  it('loadTvs ignores a non-list answer', async () => {
    tvs.set([row({})]);
    invoke.mockResolvedValue(undefined);
    await loadTvs();
    expect(get(tvs)).toHaveLength(1);
  });

  it('defaultTv is the default row, else the first, else null', () => {
    const a = row({ tv_id: 'a' });
    const b = row({ tv_id: 'b', is_default: true });
    expect(defaultTv([a, b])).toBe(b);
    expect(defaultTv([a])).toBe(a);
    expect(defaultTv([])).toBeNull();
  });
});
