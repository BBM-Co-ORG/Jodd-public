// The phone note header's ⋯ ↔ the open editor's ⋯ menu (spec 2026-10-20
// §5.6). On a phone the editor's toolbar row does not render, so its ⋯ is the
// note PaneHeader's (NoteActionsButton, in App). The menu itself stays
// NoteEditor's — one NoteMoreMenu, wired to the editor's own Find, Graph,
// Tags and Mark reviewed — and NoteEditor publishes here how to open it. The
// same shape as editorSaveState: the editor owns the state, the header reads
// it; null while no editor on a phone is showing a note.
import { writable } from 'svelte/store';
import { publication } from './publication';

export interface NoteActionsMenu {
  /**
   * Open the menu, or close it if it is showing. `anchor` is the button that
   * asked: the sheet returns focus to it, and Tags… falls back to it.
   */
  toggle: (anchor: HTMLElement) => void;
  /** The menu is showing — the button's aria-expanded. */
  open: boolean;
}

export const noteActionsMenu = writable<NoteActionsMenu | null>(null);

/** An editor's handle on `noteActionsMenu` — see publication.ts. */
export const claimNoteActionsMenu = publication(noteActionsMenu, null);
