// Pure helpers for the Extract modal's "Sources from links" section.
// docs/superpowers/specs/2026-09-15-url-ingest-design.md
import type { IngestAnalysis, IngestSourceKind, IngestProgress, IngestMode, ModalWorkflow, WorkflowKind } from './types';

/** Mirrors `ingest::MAX_URLS_PER_INGEST`; the backend refuses more. */
export const MAX_URLS_PER_INGEST = 8;

/** How long typing pauses before `analyze_ingest_sources` runs. */
export const ANALYZE_DEBOUNCE_MS = 300;

/** The Extract modal's mode selector, in order. 'extract' is labelled "Key
 *  points" (see LessonExtractModal). Also offered by the capture sheet. */
export const WORKFLOW_OPTIONS: { value: ModalWorkflow; label: string }[] = [
  { value: 'extract', label: 'Key points' },
  { value: 'summarize', label: 'Summarize' },
  { value: 'action_items', label: 'Action items' },
  { value: 'expand_bullets', label: 'Expand bullets' },
  { value: 'transcript', label: 'Transcript' },
];

/** Spec Decision 9: nothing is fetched by surprise. Only mostly-links input
 * starts checked, and only its supported links, at most the cap. */
export function initialSelection(analysis: IngestAnalysis): string[] {
  if (!analysis.mostly_urls) return [];
  return analysis.sources.filter((s) => s.supported).slice(0, MAX_URLS_PER_INGEST).map((s) => s.url);
}

/** What the source list calls each kind of link. */
const SOURCE_KIND_LABELS: Record<string, string> = {
  web: 'Page',
  youtube: 'Video',
  tiktok: 'TikTok',
  instagram: 'Instagram',
  facebook: 'Facebook',
  x: 'X',
};

export function sourceKindLabel(kind: IngestSourceKind): string {
  return SOURCE_KIND_LABELS[kind] ?? 'Skipped';
}

/** The "Transcribe the audio myself" override (D2) applies to TikTok only —
 *  the one platform that has its own transcripts — and only when speech-to-text
 *  is set up, because it would otherwise do nothing. */
export function offersOwnTranscription(analysis: IngestAnalysis | null, selected: string[]): boolean {
  if (!analysis?.stt_host) return false;
  return analysis.sources.some((s) => selected.includes(s.url) && s.may_transcribe && s.kind === 'tiktok');
}

export function toggleUrl(selected: string[], url: string): string[] {
  if (selected.includes(url)) return selected.filter((u) => u !== url);
  if (selected.length >= MAX_URLS_PER_INGEST) return selected;
  return [...selected, url];
}

export function progressLine(p: IngestProgress): string {
  const host = p.url_host ? ` · ${p.url_host}` : '';
  switch (p.stage) {
    case 'fetching':
      return `Fetching ${p.index} of ${p.total}${host}`;
    case 'downloading':
      return `Downloading the video ${p.index} of ${p.total}${host}`;
    case 'transcribing':
      return `Transcribing the audio ${p.index} of ${p.total}${host}`;
    case 'summarizing':
      return `Summarizing ${p.index} of ${p.total}${host}`;
    case 'cleaning':
      return `Cleaning ${p.url_host ?? 'source'} — section ${p.index} of ${p.total}`;
    case 'synthesizing':
      return 'Combining the sources';
    case 'writing':
      return 'Writing the note';
    case 'done':
      return 'Done';
  }
}

/** Action items has no ingest mode (spec D3); the modal disables it while
 * links are selected, so this fallback only guards a stale call. */
export function ingestModeFor(w: ModalWorkflow, clean: boolean): IngestMode {
  switch (w) {
    case 'summarize':
    case 'expand_bullets':
      return { kind: 'workflow', workflow: w };
    case 'transcript':
      return { kind: 'transcript', clean };
    default:
      return { kind: 'key_points' };
  }
}

export function backendWorkflow(w: Exclude<ModalWorkflow, 'extract'>): WorkflowKind {
  return w === 'transcript' ? 'clean_transcript' : w;
}
