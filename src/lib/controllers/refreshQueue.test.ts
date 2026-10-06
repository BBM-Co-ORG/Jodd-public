import { describe, it, expect, vi } from 'vitest';
import { createRefreshQueue } from './refreshQueue';
const settle = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };
describe('refresh controller', () => {
  it('coalesces while saving and while fetching, drains after failure', async () => {
    let saving = true;
    let reject!: (e: unknown) => void;
    const error = vi.fn(), old = vi.fn(async () => {}), latest = vi.fn(async () => {});
    const q = createRefreshQueue(() => saving, error);
    q.schedule(old);
    q.schedule(() => new Promise((_, no) => { reject = no; }));
    expect(old).not.toHaveBeenCalled();
    saving = false; q.drain();
    q.schedule(old); q.schedule(latest);
    reject('offline'); await settle();
    expect(error).toHaveBeenCalledWith('offline');
    expect(old).not.toHaveBeenCalled(); expect(latest).toHaveBeenCalledOnce();
  });
  it('disposal prevents late completion from starting queued work', async () => {
    let done!: () => void;
    const next = vi.fn(async () => {});
    const q = createRefreshQueue(() => false, vi.fn());
    q.schedule(() => new Promise(resolve => { done = resolve; }));
    q.schedule(next); q.dispose(); done(); await settle();
    q.schedule(next); expect(next).not.toHaveBeenCalled();
  });
});
