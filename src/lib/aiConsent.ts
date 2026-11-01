// The one-click AI consent (spec 2026-10-08 §4.5, privacy PR3). The refusal
// text is pinned against Rust's llm::policy::AI_CONSENT_NEEDED by a Rust test
// that reads this file — change both together.
import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';
import { error } from './stores/notes';

export const AI_CONSENT_NEEDED = 'AI data access is not allowed for this account. Allow it to continue.';
/** The consent button, the same words everywhere it is offered (spec 2026-10-28 §3). */
export function allowAiLabel(accountName: string): string {
  return `Allow AI for ${accountName}`;
}
/** The Account Settings checkbox the one-click consent ticks; named wherever the consent is offered. */
export const DATA_ALLOWED_LABEL = 'Allow this account’s data in Jodd-managed AI';

/** `includes`, not equality: the UI receives it behind `provider not configured: `, sometimes wrapped as `Error: …`. */
export function isConsentRefusal(e: unknown): boolean {
  return String(e ?? '').includes(AI_CONSENT_NEEDED);
}

export function allowAiForAccount(accountId: string): Promise<void> {
  return invoke<void>('allow_ai_for_account', { accountId });
}

/** The open consent prompt; `AiConsentPrompt` (mounted once in App.svelte) renders it. */
export type ConsentRequest = { accountId: string; resolve: (allowed: boolean) => void };
export const consentRequest = writable<ConsentRequest | null>(null);

/** Close the prompt and hand the answer to whoever opened it — synchronously, so the dialog goes away in the same turn as the click. */
export function answerConsent(allowed: boolean): void {
  const req = get(consentRequest);
  if (!req) return;
  consentRequest.set(null);
  req.resolve(allowed);
}

/**
 * For callers with nowhere inline to put a button: the note context menu has
 * already closed itself when its AI action fails. Resolves `true` when `e` was
 * a consent refusal — the prompt was shown and answered, and the caller must
 * not report `e` itself — and `false` otherwise. Never native confirm():
 * WKWebView drops it silently.
 */
export async function offerAiConsent(e: unknown, accountId: string): Promise<boolean> {
  if (!isConsentRefusal(e)) return false;
  answerConsent(false); // a newer refusal replaces one left unanswered
  const allowed = await new Promise<boolean>((resolve) => consentRequest.set({ accountId, resolve }));
  if (allowed) {
    try {
      await allowAiForAccount(accountId);
    } catch (err) {
      error.set(`Could not allow AI for this account: ${err}`);
    }
  }
  return true;
}
