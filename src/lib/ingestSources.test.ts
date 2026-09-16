import { describe, it, expect } from 'vitest';
import type { IngestAnalysis, IngestSource } from './types';
import { MAX_URLS_PER_INGEST, initialSelection, progressLine, toggleUrl } from './ingestSources';

function source(url: string, supported = true): IngestSource {
  return { url, kind: supported ? 'web' : 'unsupported', supported, reason: supported ? null : 'nope', duplicate_owner: null };
}

function analysis(urls: string[], mostly_urls: boolean, extra: IngestSource[] = []): IngestAnalysis {
  return { sources: [...urls.map((u) => source(u)), ...extra], mostly_urls, context_text: '', context_chars: 0 };
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
});
