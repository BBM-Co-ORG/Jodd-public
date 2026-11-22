// The open note's save state as short text (spec 2026-10-20 §6.1), for the
// phone note PaneHeader. NoteEditor owns the state — it alone knows whether
// the user has typed since the last save — and publishes the result of its
// existing {#if} chain here; null while no editor is mounted.
import { writable } from 'svelte/store';
import { publication } from './publication';

export type SaveStateKind = 'saving' | 'unsaved' | 'draft' | 'blocked' | 'saved';

export interface EditorSaveState {
  kind: SaveStateKind;
  /** The short form shown in the header. */
  text: string;
  /** The full sentence the short form replaces: title and accessible name. */
  title: string;
}

export const editorSaveState = writable<EditorSaveState | null>(null);

/** An editor's handle on `editorSaveState` — see publication.ts. */
export const claimEditorSaveState = publication(editorSaveState, null);

/**
 * The same precedence as NoteEditor's toolbar chain: saving, then unsaved
 * edits, then a draft that exists only in memory, then a refused push, then
 * where the saved note stands with its remote.
 *
 * Once saved, the word describes the REMOTE (`persistenceStatus`): "Synced",
 * "Sync pending", "On this device" — never a bare "Saved", which covered
 * both synced and not yet sent. A refused push reads "On this device" too:
 * "Not synced" and its Try again are the metadata line's chip, and only that
 * (a remote-locked note there reads "Read-only", with no Try again).
 */
export function shortSaveState(s: {
  saving: boolean;
  unsaved: boolean;
  draft: boolean;
  blocked: boolean;
  /** `persistenceStatus` for the open note. */
  saved: { word: string; sentence: string };
}): EditorSaveState {
  // A refused push is said by its notice (the One Voice Rule), which stays
  // up while the user types: the word stays held rather than flashing
  // "Unsaved" and "Saving…" beside it on every pause (re-critique after #294).
  if (s.blocked) return { kind: 'blocked', text: 'On this device', title: 'Saved on this device · Sync blocked' };
  if (s.saving) return { kind: 'saving', text: 'Saving…', title: 'Saving…' };
  if (s.unsaved) return { kind: 'unsaved', text: 'Unsaved', title: 'Unsaved changes on this device' };
  if (s.draft) return { kind: 'draft', text: 'Draft', title: 'Draft — start typing to save' };
  return { kind: 'saved', text: s.saved.word, title: s.saved.sentence };
}

/**
 * "Edited on another device" is said once, by the editor's notice row under
 * the metadata line, on every layout: with the reassurance, how the edits
 * are kept, and "Use other version". The toolbar and the phone header used
 * to repeat the label (and the header read it from a published store); the
 * final re-critique found the duplicate reflowed the toolbar mid-edit.
 */
/** The notice row under the metadata line while the banner is up. */
export const EXTERNAL_CHANGE_NOTICE = 'Edited on another device. Your edits here will be kept.';
/** How they are kept, as visible text under the notice: touch cannot read a title. */
// Exactly the editor's merge rule (NoteEditor saveNote → mergeOntoRow,
// merge.rs): it merges only when the other device ADDED lines; any other
// change keeps both. "Changes on different lines are merged" promised more
// than that (re-critique after #292). Pinned in noteEditorCollisionNotice.test.ts.
export const EXTERNAL_CHANGE_DETAIL =
  'Lines the other device added are merged into yours. If it changed or removed a line, your version is saved as a separate copy.';
export const USE_OTHER_VERSION_LABEL = 'Use other version';
export const USE_OTHER_VERSION_TITLE =
  "Show the other device's version of this note instead of the text here";
