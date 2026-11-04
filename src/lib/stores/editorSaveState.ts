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
 * the persistence label.
 */
export function shortSaveState(s: {
  saving: boolean;
  unsaved: boolean;
  draft: boolean;
  blocked: boolean;
  savedLabel: string;
}): EditorSaveState {
  if (s.saving) return { kind: 'saving', text: 'Saving…', title: 'Saving…' };
  if (s.unsaved) return { kind: 'unsaved', text: 'Unsaved', title: 'Unsaved changes on this device' };
  if (s.draft) return { kind: 'draft', text: 'Draft', title: 'Draft — start typing to save' };
  if (s.blocked) return { kind: 'blocked', text: 'Not synced', title: 'Saved on this device · Sync blocked' };
  return { kind: 'saved', text: 'Saved', title: s.savedLabel || 'Saved' };
}

/**
 * "Edited on another device" + Reload (spec 2026-10-20 §6.1): the toolbar
 * shows it beside the save state, and it stays visible on phone, where the
 * toolbar row does not render (§5.6) — the phone note PaneHeader reads this.
 * Deliberately NOT a SaveStateKind: it describes the other device's save, and
 * is true at the same time as this device's "Unsaved". Non-null while the
 * banner is up; `reload` discards the unsaved work here and loads the other
 * device's version. NoteEditor owns it, as it owns the save state.
 */
export interface EditorExternalChange {
  reload: () => void;
}

export const editorExternalChange = writable<EditorExternalChange | null>(null);

/** An editor's handle on `editorExternalChange` — see publication.ts. */
export const claimEditorExternalChange = publication(editorExternalChange, null);

/** The banner's short text, and the full sentence that is its title. */
export const EXTERNAL_CHANGE_TEXT = 'Edited on another device';
export const EXTERNAL_CHANGE_TITLE =
  "This note was edited on another device. Your changes are still here and will be saved as a separate conflict copy; this note keeps the other device's version.";
export const RELOAD_TITLE = "Discard the unsaved changes here and load the other device's version";
