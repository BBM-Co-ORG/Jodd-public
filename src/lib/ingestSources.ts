// Pure helpers for the Extract modal's "Sources from links" section.
// docs/superpowers/specs/2026-09-15-url-ingest-design.md
import type { IngestAnalysis, IngestProgress } from './types';

/** Mirrors `ingest::MAX_URLS_PER_INGEST`; the backend refuses more. */
export const MAX_URLS_PER_INGEST = 8;

/** How long typing pauses before `analyze_ingest_sources` runs. */
export const ANALYZE_DEBOUNCE_MS = 300;

/** Spec Decision 9: nothing is fetched by surprise. Only mostly-links input
 * starts checked, and only its supported links, at most the cap. */
export function initialSelection(analysis: IngestAnalysis): string[] {
  if (!analysis.mostly_urls) return [];
  return analysis.sources.filter((s) => s.supported).slice(0, MAX_URLS_PER_INGEST).map((s) => s.url);
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
    case 'summarizing':
      return `Summarizing ${p.index} of ${p.total}${host}`;
    case 'synthesizing':
      return 'Combining the sources';
    case 'writing':
      return 'Writing the note';
    case 'done':
      return 'Done';
  }
}
