// When an AI action shows, and whether it can be used (spec
// 2026-10-28-ai-actions-and-agent-folders-ux-design.md §4). Every AI entry
// point — menus, the account header, the phone AI sheet — asks this one
// function instead of writing its own condition, so the surfaces cannot
// drift apart again.
//
// This is a UI hint, never the guard: the backend still refuses a write it
// cannot make (`refuse_write`) and a folder hidden from AI (Studio's
// `plan.rs`, Ask's pool, Extract's source check).
import { AI_HIDDEN_HINT } from '../stores/aiScope';

export const NO_FOLDER_HINT = 'Select a folder first.';

export interface AiGateInput {
  /** The action creates or changes a note: can this account do that? Omit
   *  for a read-only action (Ask Jodd). */
  canWrite?: boolean;
  /** The folder or note the action reads is hidden from AI. */
  hidden: boolean;
  /** The action needs a selected folder: is one selected? Omit when the
   *  action carries its own scope (a folder or note menu). */
  hasFolder?: boolean;
}

export interface AiGate {
  /** Render the entry at all. A write the account cannot make is hidden,
   *  as every write action in the menus already is. */
  show: boolean;
  disabled: boolean;
  /** Why it is disabled, for the entry's tooltip; '' when enabled. */
  reason: string;
}

export function aiGate({ canWrite = true, hidden, hasFolder = true }: AiGateInput): AiGate {
  if (!canWrite) return { show: false, disabled: true, reason: '' };
  if (hidden) return { show: true, disabled: true, reason: AI_HIDDEN_HINT };
  if (!hasFolder) return { show: true, disabled: true, reason: NO_FOLDER_HINT };
  return { show: true, disabled: false, reason: '' };
}
