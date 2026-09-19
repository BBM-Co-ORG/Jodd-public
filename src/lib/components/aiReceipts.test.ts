// @vitest-environment jsdom
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import AiReceipts from './AiReceipts.svelte';
import { usageLabel, type AiReceipt } from '../aiReceipts';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const row: AiReceipt = { run_id: 'opaque-id', started_ms: 1, prompt_version: 'v1', storage_failed: false,
  steps: [{ id: 'step', kind: 'ask', stage: 'answering', outcome: 'running', latency_ms: 0, scope_version: 'opaque', checks: [] }],
  calls: [{ step_id: 'step', provider: 'agent_cli', model: null, model_source: 'unknown', stage: 'answering', outcome: 'running', latency_ms: 0, usage: { input_tokens: null, output_tokens: null, provenance: 'unknown' } }] };
let host: HTMLElement, app: ReturnType<typeof mount>;
beforeEach(() => {
  vi.useFakeTimers(); invoke.mockReset();
  invoke.mockImplementation(async cmd => cmd === 'list_ai_receipts' ? [structuredClone(row)] : cmd === 'get_ai_receipt_retention' ? 30 : undefined);
  host = document.createElement('div'); document.body.append(host);
});
afterEach(async () => { if (app) await unmount(app); app = undefined!; host.remove(); vi.useRealTimers(); });
async function settle() { flushSync(); await tick(); await vi.advanceTimersByTimeAsync(0); flushSync(); }
it('shows observable progress and collapsed metadata without inventing usage', async () => {
  app = mount(AiReceipts, { target: host, props: { requestId: 'r' } }); await settle();
  expect(host.querySelector('[role=status]')?.textContent).toContain('Generating answer');
  expect(host.querySelector('details')?.open).toBe(false);
  expect(host.textContent).toContain('Usage unknown'); expect(host.textContent).toContain('model unknown or redacted');
  expect(invoke).toHaveBeenCalledWith('list_ai_receipts', { requestId: 'r' });
});
it('distinguishes missing values, reported zero and estimates', () => {
  expect(usageLabel({ input_tokens: null, output_tokens: null, provenance: 'unknown' })).toBe('Usage unknown');
  expect(usageLabel({ input_tokens: 0, output_tokens: null, provenance: 'actual' })).toBe('Reported tokens: 0 in / unknown out');
  expect(usageLabel({ input_tokens: 8, output_tokens: 4, provenance: 'estimated' })).toMatch(/^Estimated/);
});
it('deletes only metadata and exports only backend-redacted output', async () => {
  invoke.mockImplementation(async cmd => cmd === 'list_ai_receipts' ? [row] : cmd === 'get_ai_receipt_retention' ? 30 : cmd === 'export_ai_receipts' ? '[{"run":1}]' : undefined);
  app = mount(AiReceipts, { target: host, props: { history: true } }); await settle();
  const click = (label: string) => (Array.from(host.querySelectorAll('button')).find(b => b.textContent === label) as HTMLButtonElement).click();
  click('Export redacted metadata'); await settle();
  expect((host.querySelector('textarea') as HTMLTextAreaElement).value).toBe('[{"run":1}]');
  click('Delete receipt'); await settle();
  expect(invoke).toHaveBeenCalledWith('delete_ai_receipts', { runId: 'opaque-id' });
  expect(host.querySelector('textarea')).toBeNull();
  const select = host.querySelector('select')!; select.value = '7'; select.dispatchEvent(new Event('change', { bubbles: true })); await settle();
  expect(invoke).toHaveBeenCalledWith('set_ai_receipt_retention', { days: 7 });
});
it('ignores late IPC after unmount and stops polling', async () => {
  let resolve!: (rows: AiReceipt[]) => void;
  invoke.mockImplementation(() => new Promise(r => { resolve = r; }));
  app = mount(AiReceipts, { target: host, props: { requestId: 'r' } }); flushSync();
  await unmount(app); app = undefined!; resolve([row]); await settle();
  await vi.advanceTimersByTimeAsync(5000); expect(invoke).toHaveBeenCalledTimes(1); expect(host.textContent).toBe('');
});
it('does not claim success when metadata IPC fails', async () => {
  invoke.mockRejectedValue(new Error('SECRET backend error'));
  app = mount(AiReceipts, { target: host, props: { requestId: 'r' } }); await settle();
  expect(host.textContent).toContain('Execution details are unavailable'); expect(host.textContent).not.toContain('SECRET');
});
it('cannot restore an export after its receipts were deleted', async () => {
  let resolveExport!: (value: string) => void;
  invoke.mockImplementation(async cmd => cmd === 'list_ai_receipts' ? [row] : cmd === 'get_ai_receipt_retention' ? 30 : cmd === 'export_ai_receipts' ? new Promise<string>(resolve => { resolveExport = resolve; }) : undefined);
  app = mount(AiReceipts, { target: host, props: { history: true } }); await settle();
  const click = (label: string) => (Array.from(host.querySelectorAll('button')).find(b => b.textContent === label) as HTMLButtonElement).click();
  click('Export redacted metadata'); await settle();
  click('Delete all receipts'); await settle();
  resolveExport('old deleted metadata'); await settle();
  expect(host.querySelector('textarea')).toBeNull();
});
