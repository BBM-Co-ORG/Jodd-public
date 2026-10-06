// Share to Jodd — the frontend half (spec
// docs/superpowers/specs/2026-10-06-share-to-jodd-design.md §3.5).
//
// Rust owns the queue and every check; this module only reads it, asks Rust
// to act on an id, and repaints from the cache afterwards.

import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';
import {
  notes,
  selectedFolder,
  selectedNote,
  currentAccount,
  indexUpsertOnSave,
  canWriteAccount,
  type BackendCapabilities,
} from './stores/notes';
import type { Account, Note, ExtractedNote, ModalWorkflow } from './types';

export type CapturePayload = { url: string | null; text: string | null; title: string | null };

export type PendingCapture = {
  id: string;
  payload: CapturePayload;
  links: string[];
  default_title: string;
  received_at_ms: number;
};

/** What the sheet shows. Replaced wholesale on every drain. */
export const pendingCaptures = writable<PendingCapture[]>([]);

/**
 * Hand-off to LessonExtractModal for "Summarize with AI". Consumed once by
 * the modal when it opens; `captureId` lets it retire the capture after the
 * note exists.
 */
export type ExtractPrefill = { captureId: string; text: string; title: string; workflow?: ModalWorkflow };

/** The AI modes the sheet offers — exactly LessonExtractModal's selector. */
export { WORKFLOW_OPTIONS as AI_MODES } from './ingestSources';

/** Where the capture came from, as far as the payload can tell: the first link's site. */
export function sourceLabel(c: PendingCapture): string {
  const first = c.links[0];
  if (first) {
    try {
      return 'from ' + new URL(first).hostname.replace(/^www\./, '');
    } catch {
      /* fall through */
    }
  }
  return c.payload.text ? 'text' : '';
}
export const extractPrefill = writable<ExtractPrefill | null>(null);

/** Re-read the queue from Rust. Safe to call any number of times. */
export async function drainCaptures(): Promise<PendingCapture[]> {
  const list = await invoke<PendingCapture[]>('take_pending_captures');
  pendingCaptures.set(list);
  return list;
}

/** Of the ACTIVE accounts (`activeAccounts`), those able to write notes. */
export function writableAccounts(
  active: Account[],
  caps: Record<string, BackendCapabilities>,
): Account[] {
  return active.filter((a) => canWriteAccount(caps[a.id]));
}

/** The account the sheet preselects: the current one if it can take the note. */
export function defaultAccount(writable: Account[], current: string | null): string | null {
  if (current && writable.some((a) => a.id === current)) return current;
  return writable[0]?.id ?? null;
}

/**
 * The text the AI path starts from: the link (if the text doesn't already
 * carry it) above the shared text — what someone would have pasted.
 */
export function prefillText(c: PendingCapture): string {
  const text = c.payload.text ?? '';
  const url = c.payload.url;
  if (url && !text.includes(url)) return text ? `${url}\n\n${text}` : url;
  return text;
}

/**
 * After a capture became a note: navigate to where Rust filed it and select
 * it, from the cache only (local-first doctrine). Same steps as
 * LessonExtractModal's showCreatedNote, minus the AI suggestions.
 */
export async function showSavedCapture(acct: string, created: ExtractedNote): Promise<void> {
  if (get(currentAccount) !== acct) currentAccount.set(acct);
  selectedFolder.set(created.label);
  try {
    const cached = await invoke<Note[]>('list_cached_notes_in_folder', { accountId: acct, path: created.label });
    notes.update((ns) => [...ns.filter((n) => !(n.account_id === acct && n.label === created.label)), ...cached]);
  } catch (e) {
    console.warn('capture: cache paint failed', e);
  }
  const found = get(notes).find((n) => n.account_id === acct && n.uuid === created.uuid);
  if (found) {
    selectedNote.set(found);
    // Makes a freshly created Notes/Inbox appear in the sidebar (see
    // LessonExtractModal.showCreatedNote).
    indexUpsertOnSave(acct, null, found.id, found.label);
  }
}

/** The bookmarklet offered in Settings: the page's link, title and selection. */
export const BOOKMARKLET =
  "javascript:(()=>{const e=encodeURIComponent;location.href='jodd://capture?url='+e(location.href)+'&title='+e(document.title)+'&text='+e(String(getSelection()))})()";

/** The command a launcher (Raycast, Shortcuts, PowerToys) runs. */
export function openCommand(isWindows: boolean): string {
  return isWindows
    ? 'start "" "jodd://capture?text=Hello%20from%20PowerToys"'
    : 'open "jodd://capture?text=Hello%20from%20Raycast"';
}
