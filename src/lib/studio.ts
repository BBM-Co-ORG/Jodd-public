// Research Studio (spec 2026-10-23 §3): the dialog's types and copy. The
// types mirror the Rust serde shapes in src-tauri/src/studio/{plan,run,related}.rs
// field for field, snake_case included — there is no rename layer.
import { writable, type Writable } from 'svelte/store';

export type StudioOutput =
  | 'briefing'
  | 'faq'
  | 'timeline'
  | 'study_guide'
  | 'slide_deck'
  | 'course_module'
  | 'podcast_script'
  | 'video_script';

/** The dialog's two captioned groups (spec 2026-10-26 §4): outputs to read, then drafts to deliver. */
export type OutputGroup = 'understand' | 'deliver';

export const OUTPUT_GROUPS: { value: OutputGroup; label: string }[] = [
  { value: 'understand', label: 'Understand the notes' },
  { value: 'deliver', label: 'Draft a deliverable' },
];

export const OUTPUTS: { value: StudioOutput; label: string; description: string; group: OutputGroup }[] = [
  { value: 'briefing', label: 'Briefing', description: 'Summary, key points, disagreements and open questions', group: 'understand' },
  { value: 'faq', label: 'FAQ', description: 'Questions a reader would ask, answered from the notes', group: 'understand' },
  { value: 'timeline', label: 'Timeline', description: 'Dated events in order; undated ones listed apart', group: 'understand' },
  { value: 'study_guide', label: 'Study guide', description: 'Key concepts, glossary, and questions with answers', group: 'understand' },
  { value: 'slide_deck', label: 'Slide deck', description: 'A title slide, then a headline, bullets and speaker notes per slide', group: 'deliver' },
  { value: 'course_module', label: 'Course module', description: 'Objectives, lessons in order, exercises, and a quiz with answers last', group: 'deliver' },
  { value: 'podcast_script', label: 'Podcast script', description: 'Two hosts talk it through, with an intro and an outro', group: 'deliver' },
  { value: 'video_script', label: 'Video script', description: 'Scenes with what is on screen, the narration and a duration', group: 'deliver' },
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

/** A note from another folder of the account, suggested by `studio_related` (spec 2026-10-25 §4). */
export interface RelatedNote {
  uuid: string;
  title: string;
  label: string;
  date_ms: number;
  chars: number;
  /** Distinct folder notes this one is linked with, either direction. */
  linked_with: number;
  /** Shared, non-noise tags, sorted, without the `#`. */
  tags: string[];
  /** Matched search terms, in term order. */
  terms: string[];
}

export interface StudioRelated {
  notes: RelatedNote[];
  /** True when more qualified than were returned. */
  capped: boolean;
}

/** Why a note is suggested: up to three chips, in the order links, tags, words (spec 2026-10-25 §3). */
export function relatedReasons(n: RelatedNote): string[] {
  const out: string[] = [];
  if (n.linked_with > 0) out.push(`Linked with ${n.linked_with} ${n.linked_with === 1 ? 'note' : 'notes'}`);
  if (n.tags.length > 0) out.push(n.tags.slice(0, 3).map((t) => `#${t}`).join(' '));
  if (n.terms.length > 0) out.push(`Mentions ${n.terms.slice(0, 2).map((t) => `“${t}”`).join(', ')}`);
  return out;
}
