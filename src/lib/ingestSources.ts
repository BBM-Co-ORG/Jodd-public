// Pure helpers for the Extract modal's "Sources from links" section.
// docs/superpowers/specs/2026-09-15-url-ingest-design.md
import type { FollowedLink, IngestAnalysis, IngestSourceKind, IngestProgress, IngestMode, LinkCandidate, LinkDiscovery, LinkPick, ModalWorkflow, WorkflowKind } from './types';

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
  bluesky: 'Bluesky',
  mastodon: 'Mastodon',
  hackernews: 'Hacker News',
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

// ── Followed links (spec 2026-10-28-follow-links-and-discussions-design.md §5) ──

/** Mirrors `ingest::links::MAX_FOLLOWED_TOTAL`; the backend refuses more. */
export const MAX_FOLLOWED_TOTAL = 8;
/** Candidates shown before "Show N more". */
export const CANDIDATES_SHOWN_FIRST = 8;
/** Kinds whose audio may go to speech-to-text (spec 2026-10-18 §5.5). */
const CLIP_KINDS: IngestSourceKind[] = ['tiktok', 'instagram', 'facebook', 'x'];

/** D2: the best candidates arrive ticked — added to what is already followed,
 *  never past the total cap, never twice. */
export function initialFollowed(current: FollowedLink[], d: LinkDiscovery, fromUrl: string): FollowedLink[] {
  const out = [...current];
  for (const c of d.candidates) {
    if (!c.preticked || out.length >= MAX_FOLLOWED_TOTAL) continue;
    if (out.some((f) => f.url === c.url)) continue;
    out.push({ url: c.url, from_url: fromUrl, kind: c.kind });
  }
  return out;
}

export function isFollowed(followed: FollowedLink[], url: string): boolean {
  return followed.some((f) => f.url === url);
}

export function toggleFollowed(followed: FollowedLink[], c: LinkCandidate, fromUrl: string): FollowedLink[] {
  if (isFollowed(followed, c.url)) {
    // Unticking a link also drops what was followed from it (depth 2).
    const gone = new Set([c.url]);
    let next = followed.filter((f) => f.url !== c.url);
    let changed = true;
    while (changed) {
      changed = false;
      for (const f of next) {
        if (gone.has(f.from_url)) {
          gone.add(f.url);
          changed = true;
        }
      }
      next = next.filter((f) => !gone.has(f.url));
    }
    return next;
  }
  if (followed.length >= MAX_FOLLOWED_TOTAL) return followed;
  return [...followed, { url: c.url, from_url: fromUrl, kind: c.kind }];
}

/** What is sent: only links whose chain reaches a selected source — a link
 *  followed from an unticked source is kept in the list but not ingested. */
export function followedToSend(followed: FollowedLink[], selected: string[]): FollowedLink[] {
  const reachable = new Set(selected);
  const out: FollowedLink[] = [];
  let changed = true;
  while (changed) {
    changed = false;
    for (const f of followed) {
      if (!out.includes(f) && reachable.has(f.from_url)) {
        out.push(f);
        reachable.add(f.url);
        changed = true;
      }
    }
  }
  // Keep the user's order.
  return followed.filter((f) => out.includes(f));
}

export function anyFollowedClip(followed: FollowedLink[]): boolean {
  return followed.some((f) => CLIP_KINDS.includes(f.kind));
}

/** "Skipped 9 links: a share button (4), a tag, category, archive or search page (3), …" */
export function skippedSummary(d: LinkDiscovery): string {
  const total = d.skipped.reduce((n, s) => n + s.count, 0);
  if (total === 0) return '';
  const parts = d.skipped.slice(0, 3).map((s) => `${s.reason} (${s.count})`);
  const more = d.skipped.length > 3 ? ', …' : '';
  return `Skipped ${total} ${total === 1 ? 'link' : 'links'}: ${parts.join(', ')}${more}`;
}

/** `host/path` — the readable form of a candidate's URL. */
export function shortUrl(url: string): string {
  try {
    const u = new URL(url);
    const path = u.pathname === '/' ? '' : u.pathname;
    return `${u.host.replace(/^www\./, '')}${path}`;
  } catch {
    return url;
  }
}

// ── Pasted comments (spec 2026-10-28 D8) ────────────────────────────────────

/** How long typing in a paste box pauses before its links are ranked. */
export const PASTE_DEBOUNCE_MS = 400;

/** A new ranking of a paste box replaces the old one: links that were ticked
 *  from the previous ranking and are gone from the text are unticked, and the
 *  new ranking's pre-ticks are added. Ticks from anywhere else are kept. */
export function refollowPasted(followed: FollowedLink[], previous: LinkDiscovery | null, next: LinkDiscovery, fromUrl: string): FollowedLink[] {
  const stillThere = new Set(next.candidates.map((c) => c.url));
  const before = new Set((previous?.candidates ?? []).map((c) => c.url));
  const kept = followed.filter((f) => !(f.from_url === fromUrl && before.has(f.url) && !stillThere.has(f.url)));
  return initialFollowed(kept, next, fromUrl);
}

/** What `ingest_sources` receives: non-blank pastes for links still selected. */
export function pastedToSend(pasted: Record<string, string>, selected: string[]): { url: string; text: string }[] {
  return selected.filter((u) => (pasted[u] ?? '').trim() !== '').map((u) => ({ url: u, text: pasted[u] }));
}

// ── Phase 3: auto-follow and AI pick (spec 2026-10-28 D2, §5.4) ─────────────

/** D2: with auto-follow on, the best `max` candidates at or above
 *  `minScore` are ticked — stricter than the pre-tick, because nobody looks
 *  at the list first. Never past the total cap, never twice. */
export function autoFollowed(current: FollowedLink[], d: LinkDiscovery, fromUrl: string, max: number, minScore: number): FollowedLink[] {
  const out = [...current];
  let added = 0;
  for (const c of d.candidates) {
    if (added >= max || out.length >= MAX_FOLLOWED_TOTAL) break;
    if (c.score < minScore || out.some((f) => f.url === c.url)) continue;
    out.push({ url: c.url, from_url: fromUrl, kind: c.kind });
    added++;
  }
  return out;
}

/** "Let AI choose" replaces the ticks under one source with the AI's picks
 *  (and drops what was followed from the unticked ones). Picks outside the
 *  list are ignored — the backend already refuses them. */
export function applyPicks(followed: FollowedLink[], d: LinkDiscovery, fromUrl: string, picks: LinkPick[]): FollowedLink[] {
  let next = followed;
  for (const c of d.candidates) {
    if (next.some((f) => f.url === c.url && f.from_url === fromUrl)) next = toggleFollowed(next, c, fromUrl);
  }
  for (const p of picks) {
    const c = d.candidates.find((x) => x.url === p.url);
    if (c && !isFollowed(next, c.url)) next = toggleFollowed(next, c, fromUrl);
  }
  return next;
}

/** Mirrors `ingest::web::NO_READABLE_TEXT`: the one failure the private
 *  reader is offered for (spec 2026-10-28 §7.3). */
export const NO_READABLE_TEXT = 'no readable text (the page may need JavaScript)';

/** The links an ingest error says build their text in JavaScript. Reads the
 *  "• url — failed: …" lines of `IngestError::NothingUsable`; the URLs are the
 *  backend's own (canonical) spelling, which the reader and ingest key on. */
export function needsJavascript(error: string): string[] {
  const out: string[] = [];
  for (const line of error.split('\n')) {
    const m = /^• (\S+) — failed: (.*)$/.exec(line.trim());
    if (m && m[2] === NO_READABLE_TEXT && !out.includes(m[1])) out.push(m[1]);
  }
  return out;
}

/** The pages to read from the private reader instead of fetching again. */
export function renderedToSend(rendered: Record<string, { status: string }>): string[] {
  return Object.entries(rendered)
    .filter(([, r]) => r.status === 'done')
    .map(([url]) => url);
}
