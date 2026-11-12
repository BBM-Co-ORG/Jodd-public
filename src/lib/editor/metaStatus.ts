// The editor metadata line's one status indicator (spec 2026-10-20 §6.1).
// Several per-note states can hold at once; the line shows only the most
// important, and its popover carries that one's full sentence and action.
//
// Only the states whose voice is the chip (editorStatus.ts, the One Voice
// Rule): a refused push and a remote lock are said by their notice above the
// title, never here as well.
import type { IconName } from '../components/Icon.svelte';

export type MetaStatusKind = 'trust' | 'pinBlocked';

export interface MetaStatusAction {
  label: string;
  /** May return a promise: the popover waits for it to settle (see EditorMetaLine). */
  run: () => void | Promise<unknown>;
  disabled?: boolean;
  /** Why the action is disabled, or what it does: its tooltip. */
  title?: string;
}

export interface MetaStatus {
  kind: MetaStatusKind;
  /** The short form on the indicator. */
  label: string;
  /** The full sentence the popover shows. */
  detail: string;
  action?: MetaStatusAction;
  error?: string | null;
  /** Optional glyph before the label. */
  icon?: IconName;
  /** Optional extra line in the popover (e.g. who created the note). */
  note?: string;
}

/** Highest first. */
export const META_STATUS_PRIORITY: readonly MetaStatusKind[] = ['trust', 'pinBlocked'];

/**
 * The status the line shows: trust > pinBlocked.
 * Absent candidates (null, undefined, false) are skipped, so callers can pass
 * each state's conditional expression directly.
 */
export function pickMetaStatus(
  candidates: ReadonlyArray<MetaStatus | null | undefined | false>,
): MetaStatus | null {
  let best: MetaStatus | null = null;
  for (const c of candidates) {
    if (!c) continue;
    if (!best || META_STATUS_PRIORITY.indexOf(c.kind) < META_STATUS_PRIORITY.indexOf(best.kind)) best = c;
  }
  return best;
}
