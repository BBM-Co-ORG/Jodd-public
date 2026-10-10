// @vitest-environment jsdom
//
// Privacy PR3 (pre-flight B2): the consent question is an in-DOM dialog,
// mounted once at app level, because WKWebView drops native confirm()
// silently and the note context menu has already unmounted itself (and any
// dialog of its own) by the time its AI action fails.
import './__fixtures__/dialogStub';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import AiConsentPrompt from './AiConsentPrompt.svelte';
import { AI_CONSENT_NEEDED, consentRequest, offerAiConsent } from '../aiConsent';

let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  consentRequest.set(null);
  host = document.createElement('div');
  document.body.append(host);
  component = mount(AiConsentPrompt, { target: host });
  flushSync();
});
afterEach(async () => {
  await unmount(component);
  host.remove();
  consentRequest.set(null);
});

async function settle() { for (let i = 0; i < 8; i++) await tick(); flushSync(); }
const button = (label: string) => [...host.querySelectorAll('button')].find((b) => b.textContent?.trim() === label);

it('renders nothing until a refusal asks', () => {
  expect(host.querySelector('dialog')).toBeNull();
});

it('Allow AI for this account grants the refused account, once', async () => {
  const handled = offerAiConsent(`provider not configured: ${AI_CONSENT_NEEDED}`, 'gmail:a@x.com');
  await settle();
  expect(host.querySelector('dialog')?.getAttribute('aria-label')).toBe('Allow AI for this account?');
  button('Allow AI for this account')!.click();
  await settle();
  expect(await handled).toBe(true);
  expect(invoke.mock.calls).toEqual([['allow_ai_for_account', { accountId: 'gmail:a@x.com' }]]);
  expect(host.querySelector('dialog')).toBeNull();
});

it('Cancel invokes nothing', async () => {
  const handled = offerAiConsent(`provider not configured: ${AI_CONSENT_NEEDED}`, 'gmail:a@x.com');
  await settle();
  button('Cancel')!.click();
  await settle();
  expect(await handled).toBe(true);
  expect(invoke).not.toHaveBeenCalled();
  expect(host.querySelector('dialog')).toBeNull();
});
