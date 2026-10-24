// Research Studio (spec 2026-10-23 §3): the dialog's types and copy. The
// types mirror the Rust serde shapes in src-tauri/src/studio/{plan,run}.rs
// field for field, snake_case included — there is no rename layer.
import { writable, type Writable } from 'svelte/store';

export type StudioOutput = 'briefing' | 'faq' | 'timeline' | 'study_guide';

export const OUTPUTS: { value: StudioOutput; label: string; description: string }[] = [
  { value: 'briefing', label: 'Briefing', description: 'Summary, key points, disagreements and open questions' },
  { value: 'faq', label: 'FAQ', description: 'Questions a reader would ask, answered from the notes' },
  { value: 'timeline', label: 'Timeline', description: 'Dated events in order; undated ones listed apart' },
  { value: 'study_guide', label: 'Study guide', description: 'Key concepts, glossary, and questions with answers' },
];

/** The folder the dialog is open for; null while closed. Set by the folder menu, cleared by the dialog. */
export const studioRequest: Writable<{ accountId: string; label: string } | null> = writable(null);

/** `studio_plan` re-runs this long after the last change to the output, Focus or ticks. */
export const PLAN_DEBOUNCE_MS = 250;

export interface PlanNote {
  uuid: string;
  title: string;
  label: string;
  date_ms: number;
  chars: number;
  is_studio: boolean;
}

export interface StudioEstimate {
  calls: number;
  bytes: number;
  map_reduce: boolean;
  refusal: string | null;
}

export interface StudioPlan {
  notes: PlanNote[];
  not_shown: number;
  selected: string[];
  estimate: StudioEstimate;
}

export type StudioProgress =
  | { stage: 'reading'; total: number }
  | { stage: 'summarizing'; index: number; total: number }
  | { stage: 'writing' }
  | { stage: 'saving' }
  | { stage: 'done' };

export function progressLine(p: StudioProgress): string {
  switch (p.stage) {
    case 'reading':
      return `Reading ${p.total} notes…`;
    case 'summarizing':
      return `Summarising ${p.index} of ${p.total}…`;
    case 'writing':
      return 'Writing…';
    case 'saving':
      return 'Saving…';
    case 'done':
      return 'Done';
  }
}

/** The dialog's estimate line: nothing readable, else the refusal, else the call count. */
export function estimateLine(plan: StudioPlan): string {
  if (plan.notes.length === 0) return 'No notes here that AI can read.';
  if (plan.estimate.refusal) return plan.estimate.refusal;
  const n = plan.estimate.calls;
  return `About ${n} AI ${n === 1 ? 'call' : 'calls'}.`;
}
