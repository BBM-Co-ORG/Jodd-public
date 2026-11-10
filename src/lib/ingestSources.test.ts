import { describe, it, expect } from 'vitest';
import type { IngestAnalysis, IngestSource } from './types';
import { WORKFLOW_OPTIONS, MAX_URLS_PER_INGEST, initialSelection, offersOwnTranscription, progressLine, toggleUrl, backendWorkflow, ingestModeFor, NO_READABLE_TEXT, needsJavascript, renderedToSend } from './ingestSources';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

function source(url: string, supported = true): IngestSource {
  return { url, kind: supported ? 'web' : 'unsupported', supported, reason: supported ? null : 'nope', duplicate_owner: null, may_transcribe: false, comments_closed: false };
}

function analysis(urls: string[], mostly_urls: boolean, extra: IngestSource[] = []): IngestAnalysis {
  return { sources: [...urls.map((u) => source(u)), ...extra], mostly_urls, context_text: '', context_chars: 0, stt_host: null, stt_insecure: false };
}

describe('initialSelection (spec Decision 9)', () => {
  it('pre-checks supported links when the text is mostly links', () => {
    expect(initialSelection(analysis(['https://a', 'https://b'], true, [source('https://p', false)]))).toEqual(['https://a', 'https://b']);
  });

  it('checks nothing when the text is an article with inline links', () => {
    expect(initialSelection(analysis(['https://a'], false))).toEqual([]);
  });

  it('checks at most MAX_URLS_PER_INGEST', () => {
    const urls = Array.from({ length: 10 }, (_, i) => `https://e/${i}`);
    expect(initialSelection(analysis(urls, true))).toEqual(urls.slice(0, MAX_URLS_PER_INGEST));
  });
});

describe('toggleUrl', () => {
  it('adds and removes', () => {
    expect(toggleUrl([], 'a')).toEqual(['a']);
    expect(toggleUrl(['a', 'b'], 'a')).toEqual(['b']);
  });

  it('refuses a ninth link', () => {
    const eight = Array.from({ length: 8 }, (_, i) => `u${i}`);
    expect(toggleUrl(eight, 'u9')).toBe(eight);
  });
});

describe('progressLine', () => {
  it('names the stage, the count and the host — nothing else', () => {
    expect(progressLine({ stage: 'fetching', index: 1, total: 3, url_host: 'www.youtube.com' })).toBe('Fetching 1 of 3 · www.youtube.com');
    expect(progressLine({ stage: 'summarizing', index: 2, total: 2, url_host: null })).toBe('Summarizing 2 of 2');
    expect(progressLine({ stage: 'synthesizing', index: 1, total: 1, url_host: null })).toBe('Combining the sources');
    expect(progressLine({ stage: 'writing', index: 1, total: 1, url_host: null })).toBe('Writing the note');
    expect(progressLine({ stage: 'done', index: 1, total: 1, url_host: null })).toBe('Done');
  });

  it('names the social video steps with the same counts', () => {
    expect(progressLine({ stage: 'downloading', index: 2, total: 3, url_host: 'www.tiktok.com' })).toBe('Downloading the video 2 of 3 · www.tiktok.com');
    expect(progressLine({ stage: 'transcribing', index: 2, total: 3, url_host: 'www.tiktok.com' })).toBe('Transcribing the audio 2 of 3 · www.tiktok.com');
  });
});

describe('ingestModeFor', () => {
  it('maps each modal workflow to the ingest mode', () => {
    expect(ingestModeFor('extract', true)).toEqual({ kind: 'key_points' });
    expect(ingestModeFor('summarize', true)).toEqual({ kind: 'workflow', workflow: 'summarize' });
    expect(ingestModeFor('expand_bullets', true)).toEqual({ kind: 'workflow', workflow: 'expand_bullets' });
    expect(ingestModeFor('transcript', true)).toEqual({ kind: 'transcript', clean: true });
    expect(ingestModeFor('transcript', false)).toEqual({ kind: 'transcript', clean: false });
  });
  it('never sends action items to ingest', () => {
    expect(ingestModeFor('action_items', true)).toEqual({ kind: 'key_points' });
  });
});

describe('backendWorkflow', () => {
  it('sends Transcript as clean_transcript and the rest unchanged', () => {
    expect(backendWorkflow('transcript')).toBe('clean_transcript');
    expect(backendWorkflow('summarize')).toBe('summarize');
  });
});

describe('progressLine cleaning', () => {
  it('names the host and the section', () => {
    expect(progressLine({ stage: 'cleaning', index: 3, total: 12, url_host: 'youtube.com' })).toBe('Cleaning youtube.com — section 3 of 12');
  });
});

describe('offersOwnTranscription (D2)', () => {
  const tiktok: IngestSource = { ...source('https://t'), kind: 'tiktok', may_transcribe: true };
  const insta: IngestSource = { ...source('https://i'), kind: 'instagram', may_transcribe: true };
  const withStt = (sources: IngestSource[], stt_host: string | null): IngestAnalysis => ({ ...analysis([], true, sources), stt_host });

  it('needs a selected TikTok link and a configured endpoint', () => {
    expect(offersOwnTranscription(withStt([tiktok], 'api.groq.com'), ['https://t'])).toBe(true);
    expect(offersOwnTranscription(withStt([tiktok], null), ['https://t'])).toBe(false);
    expect(offersOwnTranscription(withStt([tiktok], 'api.groq.com'), [])).toBe(false);
    expect(offersOwnTranscription(withStt([insta], 'api.groq.com'), ['https://i'])).toBe(false);
    expect(offersOwnTranscription(null, ['https://t'])).toBe(false);
  });
});

describe('WORKFLOW_OPTIONS', () => {
  it('never offers a Studio output as a per-note workflow', () => {
    expect(WORKFLOW_OPTIONS.some((o) => String(o.value).startsWith('studio'))).toBe(false);
  });
});

// ── Followed links (spec 2026-10-28 §5) ─────────────────────────────────────
import type { FollowedLink, LinkCandidate, LinkDiscovery } from './types';
import { MAX_FOLLOWED_TOTAL, initialFollowed, toggleFollowed, followedToSend, anyFollowedClip, skippedSummary, shortUrl } from './ingestSources';

function cand(url: string, preticked = false, kind: LinkCandidate['kind'] = 'web'): LinkCandidate {
  return { url, anchor: url, context: '', region: 'body', kind, score: preticked ? 40 : 5, reasons: [], preticked, duplicate_owner: null };
}

function disc(candidates: LinkCandidate[], skipped: LinkDiscovery['skipped'] = []): LinkDiscovery {
  return { url: 'https://a.example/post', title: 'Post', candidates, not_shown: 0, skipped };
}

const POST = 'https://a.example/post';

describe('initialFollowed (D2)', () => {
  it('adds the pre-ticked candidates once, under their source', () => {
    const d = disc([cand('https://b.example/1', true), cand('https://b.example/2'), cand('https://b.example/3', true)]);
    const once = initialFollowed([], d, POST);
    expect(once.map((f) => f.url)).toEqual(['https://b.example/1', 'https://b.example/3']);
    expect(once.every((f) => f.from_url === POST)).toBe(true);
    expect(initialFollowed(once, d, POST)).toHaveLength(2);
  });

  it('never goes past the total cap', () => {
    const many = Array.from({ length: MAX_FOLLOWED_TOTAL + 3 }, (_, i) => cand(`https://b.example/${i}`, true));
    expect(initialFollowed([], disc(many), POST)).toHaveLength(MAX_FOLLOWED_TOTAL);
  });
});

describe('toggleFollowed', () => {
  it('ticks, and unticking drops what was followed from it', () => {
    let f: FollowedLink[] = toggleFollowed([], cand('https://b.example/paper'), POST);
    f = toggleFollowed(f, cand('https://c.example/data'), 'https://b.example/paper');
    f = toggleFollowed(f, cand('https://d.example/other'), POST);
    expect(f).toHaveLength(3);
    f = toggleFollowed(f, cand('https://b.example/paper'), POST);
    expect(f.map((x) => x.url)).toEqual(['https://d.example/other']);
  });

  it('a full list ignores a new tick', () => {
    const full = Array.from({ length: MAX_FOLLOWED_TOTAL }, (_, i) => ({ url: `https://b.example/${i}`, from_url: POST, kind: 'web' as const }));
    expect(toggleFollowed(full, cand('https://z.example/'), POST)).toBe(full);
  });
});

describe('followedToSend', () => {
  it('sends only links whose chain reaches a selected source', () => {
    const f: FollowedLink[] = [
      { url: 'https://b.example/1', from_url: POST, kind: 'web' },
      { url: 'https://c.example/2', from_url: 'https://b.example/1', kind: 'web' },
      { url: 'https://d.example/3', from_url: 'https://other.example/', kind: 'web' },
    ];
    expect(followedToSend(f, [POST]).map((x) => x.url)).toEqual(['https://b.example/1', 'https://c.example/2']);
    expect(followedToSend(f, [])).toEqual([]);
  });
});

describe('anyFollowedClip / skippedSummary / shortUrl', () => {
  it('a followed TikTok is a clip; a page is not', () => {
    expect(anyFollowedClip([{ url: 'u', from_url: POST, kind: 'web' }])).toBe(false);
    expect(anyFollowedClip([{ url: 'u', from_url: POST, kind: 'tiktok' }])).toBe(true);
  });

  it('summarises the top three reasons', () => {
    expect(skippedSummary(disc([]))).toBe('');
    const d = disc([], [
      { reason: 'a share button', count: 4 },
      { reason: 'a site page', count: 2 },
      { reason: 'a file, not a page', count: 1 },
      { reason: 'an ad', count: 1 },
    ]);
    expect(skippedSummary(d)).toBe('Skipped 8 links: a share button (4), a site page (2), a file, not a page (1), …');
  });

  it('shortUrl is host and path', () => {
    expect(shortUrl('https://www.example.com/a/b?q=1#x')).toBe('example.com/a/b');
    expect(shortUrl('https://example.com/')).toBe('example.com');
    expect(shortUrl('not a url')).toBe('not a url');
  });
});

// ── Pasted comments (spec 2026-10-28 D8) ────────────────────────────────────
import { refollowPasted, pastedToSend, sourceKindLabel } from './ingestSources';

describe('refollowPasted', () => {
  it('replaces the previous ranking of the same box and keeps other ticks', () => {
    const REEL = 'https://www.facebook.com/reel/1';
    const other: FollowedLink = { url: 'https://z.example/', from_url: POST, kind: 'web' };
    const first = disc([cand('https://a.example/1', true), cand('https://a.example/2', true)]);
    let f = refollowPasted([other], null, first, REEL);
    expect(f.map((x) => x.url)).toEqual(['https://z.example/', 'https://a.example/1', 'https://a.example/2']);
    const second = disc([cand('https://a.example/2', true)]);
    f = refollowPasted(f, first, second, REEL);
    expect(f.map((x) => x.url)).toEqual(['https://z.example/', 'https://a.example/2']);
  });
});

describe('pastedToSend', () => {
  it('sends only non-blank pastes for selected links', () => {
    expect(pastedToSend({ a: 'text', b: '  ', c: 'gone' }, ['a', 'b'])).toEqual([{ url: 'a', text: 'text' }]);
  });
});

describe('thread kinds', () => {
  it('have names', () => {
    expect(['bluesky', 'mastodon', 'hackernews'].map((k) => sourceKindLabel(k as never))).toEqual(['Bluesky', 'Mastodon', 'Hacker News']);
  });
});

// ── Phase 3 (spec 2026-10-28 D2, §5.4) ─────────────────────────────────────
import { autoFollowed, applyPicks } from './ingestSources';

describe('autoFollowed (D2)', () => {
  it('ticks the best few above the higher bar, in rank order', () => {
    const d = disc([
      { ...cand('https://a.example/1'), score: 75 },
      { ...cand('https://a.example/2'), score: 30 },
      { ...cand('https://a.example/3'), score: 60 },
      { ...cand('https://a.example/4'), score: 55 },
      { ...cand('https://a.example/5'), score: 50 },
    ]);
    expect(autoFollowed([], d, POST, 3, 40).map((f) => f.url)).toEqual(['https://a.example/1', 'https://a.example/3', 'https://a.example/4']);
    expect(autoFollowed([], d, POST, 3, 80)).toEqual([]);
  });
});

describe('applyPicks (§5.4)', () => {
  it("replaces this source's ticks with the picks and keeps everyone else's", () => {
    const d = disc([cand('https://a.example/1', true), cand('https://a.example/2'), cand('https://a.example/3', true)]);
    const other: FollowedLink = { url: 'https://z.example/', from_url: 'https://elsewhere.example/', kind: 'web' };
    const deep: FollowedLink = { url: 'https://deep.example/', from_url: 'https://a.example/1', kind: 'web' };
    const before = [...initialFollowed([], d, POST), deep, other];
    const after = applyPicks(before, d, POST, [{ url: 'https://a.example/2', why: 'the study' }, { url: 'https://not-listed.example/', why: 'x' }]);
    expect(after.map((f) => f.url)).toEqual(['https://z.example/', 'https://a.example/2']);
  });
});

describe('the private reader (spec 2026-10-28 §7.3)', () => {
  it('finds the pages an ingest error says need JavaScript, once each', () => {
    const msg = [
      'Nothing could be read from the links:',
      `• https://a.example/app — failed: ${NO_READABLE_TEXT}`,
      '• https://b.example/x — failed: HTTP 404',
      `• https://a.example/app — failed: ${NO_READABLE_TEXT}`,
      `• https://c.example/y — failed: no readable text, even rendered in a private reader (the page may need a sign-in)`,
    ].join('\n');
    expect(needsJavascript(msg)).toEqual(['https://a.example/app']);
    expect(needsJavascript('cancelled')).toEqual([]);
  });

  it('mirrors the backend wording exactly', () => {
    const web = readFileSync(resolve(__dirname, '../../src-tauri/src/ingest/web.rs'), 'utf8');
    expect(web).toContain(`const NO_READABLE_TEXT: &str = "${NO_READABLE_TEXT}";`);
  });

  it('sends only the pages it finished reading', () => {
    expect(renderedToSend({ a: { status: 'done' }, b: { status: 'loading' }, c: { status: 'error' } })).toEqual(['a']);
  });
});
