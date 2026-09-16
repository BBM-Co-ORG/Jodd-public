import { invoke } from '@tauri-apps/api/core';
import { setFolderSuggestion } from './stores/notes';

/** Mirrors Rust's `llm::filing::FolderSuggestionOutcome` (`#[serde(tag = "kind")]`). */
export type FolderSuggestionOutcome =
  | { kind: 'suggested'; uuid: string; path: string; reason: string | null }
  | { kind: 'no_candidates' }
  | { kind: 'none_fits' }
  | { kind: 'note_not_found' };

function newRequestId(): string {
  return globalThis.crypto?.randomUUID?.() ?? `req-${Date.now()}-${Math.random()}`;
}

export function requestFolderSuggestion(accountId: string, uuid: string): Promise<FolderSuggestionOutcome> {
  return invoke<FolderSuggestionOutcome>('suggest_note_folder', {
    accountId,
    uuid,
    requestId: newRequestId(),
  });
}

/**
 * Only a real proposal reaches the chip store; every other outcome is
 * silent here.
 *
 * Stored under `outcome.uuid` AND, when it differs, under the `uuid` passed
 * in: a backend (Microsoft) can rekey a note's uuid — its SQLite primary key
 * — between the request and the answer (gotcha #16). `outcome.uuid` is the
 * note's uuid as of right now, which a refreshed row holds; the open editor
 * may still hold the pre-rekey uuid, because nothing moves `selectedNote` on
 * a rekey. Both keys hold the SAME object, which is how
 * `clearFolderSuggestion` finds and removes the twin.
 */
export function recordFolderSuggestion(accountId: string, uuid: string, outcome: FolderSuggestionOutcome) {
  if (outcome.kind === 'suggested') {
    const s = { path: outcome.path, reason: outcome.reason };
    setFolderSuggestion(accountId, outcome.uuid, s);
    if (uuid !== outcome.uuid) setFolderSuggestion(accountId, uuid, s);
  }
}

/**
 * What the EXPLICIT "Suggest folder" action tells the user. The automatic
 * post-Extract caller shows none of these (spec "Suggestion" table).
 */
export function explicitOutcomeMessage(outcome: FolderSuggestionOutcome, noteIsOpen: boolean): string | null {
  switch (outcome.kind) {
    case 'suggested':
      // The chip renders in the editor; a note that is not open has no chip to see.
      return noteIsOpen ? null : `Suggested folder: ${outcome.path}. Open the note to move it.`;
    case 'no_candidates':
      return 'No folders to choose from yet';
    case 'none_fits':
      return 'No better folder found';
    case 'note_not_found':
      return 'This note no longer exists';
  }
}

/** `null` means say nothing (the user cancelled). */
export function explicitErrorMessage(e: unknown): string | null {
  const msg = String(e);
  if (msg === 'cancelled') return null;
  // ExtractError::NotConfigured's Display — covers a Disabled provider too.
  if (msg.startsWith('provider not configured')) return 'No LLM provider is configured for this account';
  return `Suggest folder failed: ${msg}`;
}
