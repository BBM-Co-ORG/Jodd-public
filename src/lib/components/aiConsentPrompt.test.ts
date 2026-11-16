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
// Modal reads $isAndroid, which asks the host for platform_name once; that read is not an action.
const actionCalls = () => invoke.mock.calls.filter(([cmd]) => cmd !== 'platform_name');
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import AiConsentPrompt from './AiConsentPrompt.svelte';
import { AI_CONSENT_NEEDED, consentRequest, offerAiConsent } from '../aiConsent';
import { accounts } from '../stores/notes';
import type { Account } from '../types';

let host: HTMLElement;
let component: ReturnType<typeof mount>;
beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue(undefined);
  consentRequest.set(null);
  accounts.set([{ id: 'gmail:a@x.com', email: 'a@x.com' } as Account]);
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

it('Allow AI for <account> grants the refused account, once', async () => {
  const handled = offerAiConsent(`provider not configured: ${AI_CONSENT_NEEDED}`, 'gmail:a@x.com');
  await settle();
  const labelId = host.querySelector('dialog')?.getAttribute('aria-labelledby');
  expect(host.querySelector(`#${labelId}`)?.textContent).toBe('Allow AI for a@x.com?');
  button('Allow AI for a@x.com')!.click();
  await settle();
  expect(await handled).toBe(true);
  expect(actionCalls()).toEqual([['allow_ai_for_account', { accountId: 'gmail:a@x.com' }]]);
  expect(host.querySelector('dialog')).toBeNull();
});

it('Cancel invokes nothing', async () => {
  const handled = offerAiConsent(`provider not configured: ${AI_CONSENT_NEEDED}`, 'gmail:a@x.com');
  await settle();
  button('Cancel')!.click();
  await settle();
  expect(await handled).toBe(true);
  expect(actionCalls()).toEqual([]);
  expect(host.querySelector('dialog')).toBeNull();
});
