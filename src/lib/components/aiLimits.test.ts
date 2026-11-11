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
  // The Settings page's one Save calls the section's save(); there is no button here.
  expect(host.querySelector('button')).toBeNull();
  await (app as unknown as { save(): Promise<void> }).save(); await tick(); await tick(); flushSync();
  expect(invoke).toHaveBeenCalledWith('set_ai_limits', { settings: { ...settings, automatic_enrichment: true } });
  expect(host.textContent).toContain('AI limits saved');
  await unmount(app); host.remove();
});
it('exposes isDirty() and save() for the section-form contract', async () => {
  invoke.mockReset(); invoke.mockResolvedValue(settings);
  const host = document.createElement('div'); document.body.append(host);
  const app = mount(AiLimits, { target: host }) as unknown as { isDirty(): boolean; save(): Promise<void> };
  await tick(); await tick(); flushSync();
  expect(app.isDirty()).toBe(false);
  host.querySelector<HTMLInputElement>('input[type=checkbox]')!.click(); flushSync();
  expect(app.isDirty()).toBe(true);
  await app.save(); flushSync();
  expect(invoke).toHaveBeenCalledWith('set_ai_limits', { settings: { ...settings, automatic_enrichment: true } });
  expect(app.isDirty()).toBe(false);
  host.querySelector<HTMLInputElement>('input[type=checkbox]')!.click(); flushSync();
  invoke.mockRejectedValue(new Error('nope'));
  await expect(app.save()).rejects.toThrow('Could not save AI limits. Check the values and try again.');
  expect(app.isDirty()).toBe(true);
  await unmount(app as never); host.remove();
});
import Harness from './__fixtures__/AiLimitsHarness.svelte';
it('isDirty() is reactive: a derived page-level flag flips to clean after save()', async () => {
  invoke.mockReset(); invoke.mockResolvedValue(settings);
  const host = document.createElement('div'); document.body.append(host);
  const app = mount(Harness, { target: host }) as unknown as { save(): Promise<void> };
  await tick(); await tick(); flushSync();
  const flag = () => host.querySelector('output')!.textContent;
  expect(flag()).toBe('clean');
  host.querySelector<HTMLInputElement>('input[type=checkbox]')!.click(); flushSync();
  expect(flag()).toBe('dirty');
  await app.save(); await tick(); flushSync();
  expect(flag()).toBe('clean');
  await unmount(app as never); host.remove();
});
it('save() while a write is in flight returns the same promise and writes once', async () => {
  invoke.mockReset(); invoke.mockImplementation((cmd: string) => cmd === 'get_ai_limits' ? Promise.resolve(settings) : new Promise<void>((r) => setTimeout(r, 20)));
  const host = document.createElement('div'); document.body.append(host);
  const app = mount(AiLimits, { target: host }) as unknown as { save(): Promise<void> };
  await tick(); await tick(); flushSync();
  const p1 = app.save(); const p2 = app.save();
  expect(p2).toBe(p1);
  await Promise.all([p1, p2]);
  expect(invoke.mock.calls.filter((c) => c[0] === 'set_ai_limits')).toHaveLength(1);
  await unmount(app as never); host.remove();
});
