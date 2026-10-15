// @vitest-environment jsdom
import { mount, unmount, flushSync, tick } from 'svelte';
import { it, expect, vi } from 'vitest';
import AiLimits from './AiLimits.svelte';
import { automaticEnrichmentEnabled } from '../aiLimits';
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
const settings = { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
it('keeps automatic follow-ups off when disabled or settings cannot be read', async () => {
  invoke.mockResolvedValue(settings); expect(await automaticEnrichmentEnabled()).toBe(false);
  invoke.mockRejectedValue(new Error('offline')); expect(await automaticEnrichmentEnabled()).toBe(false);
  invoke.mockResolvedValue({ ...settings, automatic_enrichment: true }); expect(await automaticEnrichmentEnabled()).toBe(true);
});
it('saves explicit limits without touching model/provider and explains unknown accounting', async () => {
  invoke.mockReset(); invoke.mockResolvedValue(settings);
  const host = document.createElement('div'); document.body.append(host);
  const app = mount(AiLimits, { target: host });
  await tick(); await tick(); flushSync();
  expect(host.textContent).toContain('not a hard spending cap');
  const checkbox = host.querySelector<HTMLInputElement>('input[type=checkbox]')!;
  expect(checkbox.checked).toBe(false); checkbox.click(); flushSync();
  host.querySelector('button')!.click(); await tick(); await tick(); flushSync();
  expect(invoke).toHaveBeenCalledWith('set_ai_limits', { settings: { ...settings, automatic_enrichment: true } });
  expect(host.textContent).toContain('AI limits saved');
  await unmount(app); host.remove();
});
