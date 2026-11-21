// Share to Jodd — the frontend half (spec
// docs/superpowers/specs/2026-10-06-share-to-jodd-design.md §3.5).
//
// Rust owns the queue and every check; this module only reads it, asks Rust
// to act on an id, and repaints from the cache afterwards.

import { invoke } from '@tauri-apps/api/core';
import { get, writable } from 'svelte/store';
import {
  notes,
  navigateToFolder,
  selectedNote,
  currentAccount,
  indexUpsertOnSave,
  setAccountNoteTags,
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
 * The sheet was put away (✕ / Back / Esc) with captures still queued. Shared
 * so the "N shared" pill can bring it back. Reset by a new share, never by
 * Jodd returning to the foreground.
 */
export const captureSnoozed = writable(false);

// The capture the person picked from the queue; it stays first across drains
// (Rust hands the queue back oldest-first) until it leaves the queue.
let focusedId: string | null = null;

/** Work on this queued capture next, whatever its place in the queue. */
export function focusCapture(id: string): void {
  focusedId = id;
  pendingCaptures.update((list) => putFirst(list, id));
}

/** `id` moved to the front; the rest keep their order. */
export function putFirst(list: PendingCapture[], id: string | null): PendingCapture[] {
  const at = id === null ? -1 : list.findIndex((c) => c.id === id);
  if (at <= 0) return list;
  return [list[at], ...list.slice(0, at), ...list.slice(at + 1)];
}

/**
 * Hand-off to LessonExtractModal for "Summarize with AI". Consumed once by
 * the modal when it opens; `captureId` lets it retire the capture after the
 * note exists.
 */
export type ExtractPrefill =
  | { kind: 'capture'; captureId: string; text: string; title: string; workflow?: ModalWorkflow }
  // "What's on my TV": the modal asks the TV itself (spec 2026-11-02 §5.4).
  | { kind: 'tv' };

/** The AI modes the sheet offers — exactly LessonExtractModal's selector. */
export { WORKFLOW_OPTIONS as AI_MODES, WORKFLOW_HINTS as AI_HINTS } from './ingestSources';

/** What the share sheet makes: the capture as it is, or one AI mode. */
export type CaptureMake = 'as_is' | ModalWorkflow;
const MAKE_KEY = 'jodd:capture-make';
const MAKES: readonly CaptureMake[] = ['as_is', 'extract', 'summarize', 'action_items', 'expand_bullets', 'transcript'];

/** The last choice, so a person who always shares to Key points starts there.
 * localStorage can throw or be empty; either way the sheet starts at As it is. */
export function loadCaptureMake(): CaptureMake {
  try {
    const v = localStorage.getItem(MAKE_KEY);
    return MAKES.includes(v as CaptureMake) ? (v as CaptureMake) : 'as_is';
  } catch {
    return 'as_is';
  }
}

export function saveCaptureMake(make: CaptureMake) {
  try {
    localStorage.setItem(MAKE_KEY, make);
  } catch {
    /* private window: forget it */
  }
}

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

/**
 * The event that asks App to re-read one folder from its backend, in the
 * background. The sheet sends it when a note is picked to append to: the
 * share may have woken a device that slept for a day (Android freezes a
 * backgrounded Jodd), and the append lands on the note as this device last
 * saw it (2026-11-05, note 80D6C8E1). Save never waits for the read — the
 * push-side base check and the merge in `merge.rs` handle a read that loses
 * the race; the read makes that race rarer.
 */
export const REFRESH_FOLDER_EVENT = 'jodd:refresh-folder';
export interface RefreshFolderRequest { accountId: string; folder: string }
export function requestFolderPull(req: RefreshFolderRequest): void {
  window.dispatchEvent(new CustomEvent<RefreshFolderRequest>(REFRESH_FOLDER_EVENT, { detail: req }));
}

/** Re-read the queue from Rust. Safe to call any number of times. */
export async function drainCaptures(): Promise<PendingCapture[]> {
  const raw = await invoke<PendingCapture[]>('take_pending_captures');
  if (focusedId !== null && !raw.some((c) => c.id === focusedId)) focusedId = null;
  const list = putFirst(raw, focusedId);
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
 * The folder search under "New note": every word of the query must appear in
 * the path, in any order and any case, so "proj work" finds Notes/Work/Projects.
 * NFKC on both sides: Thai ำ typed as ํ + า (a common keyboard habit) is
 * only a *compatibility* equivalent, which NFC leaves apart.
 */
export function filterFolders(folders: string[], query: string): string[] {
  const fold = (s: string) => s.normalize('NFKC').toLocaleLowerCase();
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return folders;
  return folders.filter((f) => {
    const path = fold(f);
    return words.every((w) => path.includes(w));
  });
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
 * After a capture became a note, or was added to one: navigate to where the
 * note lives and select it, from the cache only (local-first doctrine). Same
 * steps as LessonExtractModal's showCreatedNote, minus the AI suggestions.
 */
export async function showSavedCapture(acct: string, created: ExtractedNote): Promise<void> {
  if (get(currentAccount) !== acct) currentAccount.set(acct);
  navigateToFolder(created.label);
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
    // LessonExtractModal.showCreatedNote). A note that was appended to
    // already has a stub: replace it rather than count the note twice. A new
    // note's id is still '' (unpushed), which is "no previous stub".
    indexUpsertOnSave(acct, found.id || null, found.id, found.label);
  }
  // #tags in the shared text are derived in SQLite; show them in the sidebar.
  try {
    setAccountNoteTags(acct, await invoke<{ uuid: string; tag: string }[]>('list_note_tags', { accountId: acct }));
  } catch (e) {
    console.warn('capture: tag refresh failed', e);
  }
}

/** The bookmarklet offered in Settings: the page's link, title and selection. */
export const BOOKMARKLET =
  "javascript:(()=>{const e=encodeURIComponent;location.href='jodd://capture?url='+e(location.href)+'&title='+e(document.title)+'&text='+e(String(getSelection()))})()";

/**
 * The command a launcher (Raycast, Shortcuts, PowerToys) runs. The text is a
 * placeholder that says what to do with it, so running the command as copied
 * shows the person the instruction in the capture sheet.
 */
export function openCommand(isWindows: boolean): string {
  const link = 'jodd://capture?text=Replace%20this%20text%20with%20yours%20(URL-encoded)';
  return isWindows ? `start "" "${link}"` : `open "${link}"`;
}
