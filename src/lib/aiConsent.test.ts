// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { get } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { AI_CONSENT_NEEDED, isConsentRefusal, offerAiConsent, answerConsent, consentRequest } from './aiConsent';
import { error } from './stores/notes';

const REFUSAL = `provider not configured: ${AI_CONSENT_NEEDED}`;

describe('isConsentRefusal', () => {
  it('matches the backend refusal as the UI receives it (ExtractError prefix included)', () => {
    expect(isConsentRefusal(REFUSAL)).toBe(true);
    expect(isConsentRefusal(new Error(REFUSAL))).toBe(true);
  });
  it('does not match other AI errors', () => {
    expect(isConsentRefusal('provider not configured: AI data access is disabled or this account is unavailable.')).toBe(false);
    expect(isConsentRefusal('No accounts allow AI data access. Review Account Settings.')).toBe(false);
    expect(isConsentRefusal('cancelled')).toBe(false);
    expect(isConsentRefusal(undefined)).toBe(false);
  });
});

describe('offerAiConsent', () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockResolvedValue(undefined);
    consentRequest.set(null);
    error.set(null);
    vi.stubGlobal('alert', vi.fn());
    vi.stubGlobal('confirm', vi.fn(() => true));
  });
  afterEach(() => vi.unstubAllGlobals());

  it('ignores any other error and opens nothing', async () => {
    expect(await offerAiConsent('boom', 'gmail:a@x.com')).toBe(false);
    expect(get(consentRequest)).toBeNull();
  });

  it('opens the prompt for the refused account in the same turn, never a native confirm', () => {
    void offerAiConsent(REFUSAL, 'gmail:a@x.com');
    expect(get(consentRequest)?.accountId).toBe('gmail:a@x.com');
    expect(globalThis.confirm).not.toHaveBeenCalled();
    answerConsent(false);
  });

  it('Allow closes the prompt at once, then grants the refused account', async () => {
    const handled = offerAiConsent(REFUSAL, 'gmail:a@x.com');
    answerConsent(true);
    expect(get(consentRequest)).toBeNull();
    expect(await handled).toBe(true);
    expect(invoke.mock.calls).toEqual([['allow_ai_for_account', { accountId: 'gmail:a@x.com' }]]);
  });

  it('Cancel grants nothing and still counts as handled', async () => {
    const handled = offerAiConsent(REFUSAL, 'gmail:a@x.com');
    answerConsent(false);
    expect(await handled).toBe(true);
    expect(invoke).not.toHaveBeenCalled();
  });

  it('a failed allow goes to the error bar, never to alert()', async () => {
    invoke.mockRejectedValue('save accounts: disk full');
    const handled = offerAiConsent(REFUSAL, 'gmail:a@x.com');
    answerConsent(true);
    await handled;
    expect(get(error)).toBe('Could not allow AI for this account: save accounts: disk full');
    expect(globalThis.alert).not.toHaveBeenCalled();
  });

  it('a newer refusal replaces an unanswered one, which resolves as Cancel', async () => {
    const first = offerAiConsent(REFUSAL, 'gmail:a@x.com');
    const second = offerAiConsent(REFUSAL, 'gmail:b@x.com');
    expect(await first).toBe(true);
    expect(get(consentRequest)?.accountId).toBe('gmail:b@x.com');
    answerConsent(false);
    await second;
    expect(invoke).not.toHaveBeenCalled();
  });
});
