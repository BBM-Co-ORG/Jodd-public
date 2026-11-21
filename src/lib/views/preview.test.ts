// @vitest-environment jsdom
//
// The live strip on a view note (spec 2026-11-19 §8.5): one query per burst of
// typing, the errors shown rather than swallowed, no call outside __Views__,
// and the latest request wins over a slower older one.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { createViewPreview, isViewsLabel, type PreviewState } from './preview';
import ViewPreview from '../components/ViewPreview.svelte';

const A = 'gmail:a@b.c';
const req = (body = 'Show: notes', label: string | null = 'Notes/__Views__') =>
  ({ accountId: A, label, title: 'My view', body: () => body });

beforeEach(() => { invoke.mockReset(); vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); });

describe('view preview', () => {
  it('typing twice within 300 ms makes one preview_view call', async () => {
    invoke.mockResolvedValue({ rows: { Notes: [1, 2, 3].map((n) => ({ uuid: String(n) })) }, notices: [] });
    const states: PreviewState[] = [];
    const p = createViewPreview((s) => states.push(s));
    p.schedule(req('a'));
    await vi.advanceTimersByTimeAsync(200);
    p.schedule(req('ab'));
    await vi.advanceTimersByTimeAsync(299);
    expect(invoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('preview_view', { accountId: A, title: 'My view', bodyHtml: 'ab' });
    expect(states.at(-1)).toEqual({ kind: 'ok', summary: 'ตรง 3 โน้ต', notices: [] });
  });

  it('says tasks for a tasks view and carries notices', async () => {
    invoke.mockResolvedValue({ rows: { Tasks: [{}, {}] }, notices: ['Due: x is not a date'] });
    const states: PreviewState[] = [];
    createViewPreview((s) => states.push(s)).schedule(req());
    await vi.advanceTimersByTimeAsync(300);
    expect(states.at(-1)).toEqual({ kind: 'ok', summary: 'ตรง 2 งาน', notices: ['Due: x is not a date'] });
  });

  it('an Err renders its messages (JSON errors, plain text fallback)', async () => {
    const states: PreviewState[] = [];
    const p = createViewPreview((s) => states.push(s));
    invoke.mockRejectedValueOnce(JSON.stringify([{ line: 1, key: 'Due', message: 'cannot read "soon"' }]));
    p.schedule(req());
    await vi.advanceTimersByTimeAsync(300);
    expect(states.at(-1)).toEqual({ kind: 'error', errors: ['2: cannot read "soon"'] });
    invoke.mockRejectedValueOnce('database is locked');
    p.schedule(req());
    await vi.advanceTimersByTimeAsync(300);
    expect(states.at(-1)).toEqual({ kind: 'error', errors: ['database is locked'] });
  });

  it('a note outside __Views__ makes no call', async () => {
    const states: PreviewState[] = [];
    const p = createViewPreview((s) => states.push(s));
    p.schedule(req('x', 'Notes'));
    p.schedule(req('x', 'Notes/__Views__x'));
    p.schedule(req('x', null));
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke).not.toHaveBeenCalled();
    expect(isViewsLabel('Notes/Projects/__Views__')).toBe(true);
    expect(states.every((s) => s.kind === 'idle')).toBe(true);
  });

  it('an older answer arriving after a newer request is dropped', async () => {
    const releases: Array<(v: unknown) => void> = [];
    invoke.mockImplementation(() => new Promise((r) => releases.push(r)));
    const states: PreviewState[] = [];
    const p = createViewPreview((s) => states.push(s));
    p.schedule(req('one'));
    await vi.advanceTimersByTimeAsync(300);
    p.schedule(req('two'));
    await vi.advanceTimersByTimeAsync(300);
    expect(releases).toHaveLength(2);
    releases[1]({ rows: { Notes: [{}] }, notices: [] });
    await vi.advanceTimersByTimeAsync(0);
    releases[0]({ rows: { Notes: [{}, {}, {}, {}, {}] }, notices: [] });
    await vi.advanceTimersByTimeAsync(0);
    expect(states).toEqual([{ kind: 'ok', summary: 'ตรง 1 โน้ต', notices: [] }]);
  });

  it('an answer after cancel (note switch) is dropped', async () => {
    let release!: (v: unknown) => void;
    invoke.mockImplementation(() => new Promise((r) => { release = r; }));
    const states: PreviewState[] = [];
    const p = createViewPreview((s) => states.push(s));
    p.schedule(req());
    await vi.advanceTimersByTimeAsync(300);
    p.cancel();
    release({ rows: { Notes: [] }, notices: [] });
    await vi.advanceTimersByTimeAsync(0);
    expect(states).toEqual([]);
  });

  it('the component paints the strip for a view note and nothing otherwise', async () => {
    invoke.mockResolvedValue({ rows: { Notes: [{}, {}] }, notices: [] });
    const host = document.createElement('div');
    document.body.append(host);
    const props = { noteKey: 'k1', accountId: A, label: 'Notes/__Views__', title: 'v', getBody: () => 'b', editRev: 0 };
    const c = mount(ViewPreview, { target: host, props });
    flushSync();
    expect(host.querySelector('[data-testid="view-preview"]')).toBeNull();
    await vi.advanceTimersByTimeAsync(300);
    flushSync();
    expect(host.querySelector('[data-testid="view-preview"]')?.textContent).toContain('ตรง 2 โน้ต');
    unmount(c);
    const other = mount(ViewPreview, { target: host, props: { ...props, noteKey: 'k2', label: 'Notes' } });
    invoke.mockClear();
    await vi.advanceTimersByTimeAsync(1000);
    expect(invoke).not.toHaveBeenCalled();
    unmount(other);
    host.remove();
  });
});
