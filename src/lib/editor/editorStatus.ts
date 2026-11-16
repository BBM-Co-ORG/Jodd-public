// The editor's status system: every state a note in the editor can be in,
// the ONE place that says it, the actions it offers there, and whether a
// screen reader hears it (DESIGN.md, The One Voice Rule).
//
// Why a table: each surface used to decide on its own. The refused push ended
// up said three times — the metadata chip, its popover and the notice, with
// two Try again buttons — and the save word announced every autosave cycle.
// A new state is added here first; the components read this table, and
// editorStatus.test.ts checks the mounted editor says each state exactly
// where the table says, once.
//
// Voices:
//   saveWord — the toolbar word (and the phone header's copy of it): routine
//              progress. Never announced: an autosave every few seconds is
//              noise to a screen reader.
//   notice   — a row above the title. For states that need the user: it
//              carries the state's actions and is announced (role="status").
//   chip     — the metadata line's status chip, with its popover: states that
//              matter but do not interrupt. Only states without a notice.

export type EditorStateId =
  | 'unsaved'
  | 'saving'
  | 'saved'
  | 'draft'
  | 'external'
  | 'conflictCopy'
  | 'notSynced'
  | 'readonly'
  | 'accountReadOnly'
  | 'trust'
  | 'pinBlocked';

export type Voice = 'saveWord' | 'notice' | 'chip';

export interface StatePlan {
  voice: Voice;
  /** The actions, by label, offered where the state is said (and nowhere else). */
  actions: readonly string[];
  /** Whether a screen reader hears the state arrive. */
  announced: boolean;
}

export const COPY_TO_NEW_NOTE = 'Copy to a new note';
export const MOVE_TO_NOTES = 'Move to Notes';

export const EDITOR_STATUS: Readonly<Record<EditorStateId, StatePlan>> = {
  unsaved: { voice: 'saveWord', actions: [], announced: false },
  saving: { voice: 'saveWord', actions: [], announced: false },
  // Synced / Sync pending / On this device / Saved to folder / Write pending.
  saved: { voice: 'saveWord', actions: [], announced: false },
  draft: { voice: 'saveWord', actions: [], announced: false },
  // Another device or an agent changed the note while it is open (amber).
  external: { voice: 'notice', actions: ['Use other version'], announced: true },
  // The other device changed or removed a line: the edits here became a copy.
  conflictCopy: { voice: 'notice', actions: ['Open original', 'Dismiss'], announced: true },
  // The backend refused the push (gotcha #14): the text is safe here. The
  // refusal's class picks which of these it offers (noteStatus reasonForUser),
  // so no button is shown that cannot work for that refusal.
  notSynced: { voice: 'notice', actions: ['Try again', COPY_TO_NEW_NOTE, MOVE_TO_NOTES], announced: true },
  // The remote's document refused the write (gotcha #24): the last edit is
  // only on this device, so the notice offers a copy that keeps it.
  readonly: { voice: 'notice', actions: [COPY_TO_NEW_NOTE], announced: true },
  // The whole account cannot write notes (Capabilities.writes.notes = false).
  accountReadOnly: { voice: 'notice', actions: [], announced: false },
  // A change by someone else (an agent, another device) not reviewed yet.
  trust: { voice: 'chip', actions: ['Mark reviewed'], announced: false },
  // The pin's push was refused.
  pinBlocked: { voice: 'chip', actions: ['Try again'], announced: false },
};

/** The states said by `voice`, in table order. */
export function statesSaidBy(voice: Voice): EditorStateId[] {
  return (Object.keys(EDITOR_STATUS) as EditorStateId[]).filter((id) => EDITOR_STATUS[id].voice === voice);
}
