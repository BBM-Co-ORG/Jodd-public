// @vitest-environment jsdom
//
// The editor's link scanner, held to the same fixture as the backend's
// (`link_text` + `scan_links` in db.rs): tests/fixtures/link-detection.json.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { buildLinkText, scanLinks, spanToRange, type LinkSpan } from './scan';

interface Fixture {
  cases: Array<{ name: string; html: string; links: Array<{ kind: 'url' | 'wikilink'; text: string }> }>;
}
const fixture: Fixture = JSON.parse(readFileSync('tests/fixtures/link-detection.json', 'utf8'));

const parse = (html: string) => new DOMParser().parseFromString(html, 'text/html');
const scan = (html: string) => {
  const lt = buildLinkText(parse(html).documentElement);
  return { lt, spans: scanLinks(lt) };
};

/** Per kind de-duplicated, first-seen order across both kinds — the fixture's shape. */
function dedupe(spans: LinkSpan[]) {
  const seen = new Set<string>();
  const out: Array<{ kind: string; text: string }> = [];
  for (const s of spans) {
    const key = `${s.kind}\u0000${s.text}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ kind: s.kind, text: s.text });
  }
  return out;
}

describe('link scanner', () => {
  it('has the fixture cases', () => {
    expect(fixture.cases.length).toBeGreaterThan(30);
  });

  for (const c of fixture.cases) {
    it(`matches the shared fixture: ${c.name}`, () => {
      expect(dedupe(scan(c.html).spans)).toEqual(c.links);
    });
  }

  it('draws every occurrence', () => {
    const { spans } = scan('<div>https://a.co https://a.co</div>');
    expect(spans.map((s) => s.text)).toEqual(['https://a.co', 'https://a.co']);
    expect(spans[1].start).toBeGreaterThan(spans[0].end);
  });

  it('marks anchors', () => {
    expect(scan('<a href="x">https://a.co</a>').spans[0].insideAnchor).toBe(true);
    expect(scan('<div>https://a.co</div>').spans[0].insideAnchor).toBe(false);
  });

  it('maps back to nodes', () => {
    const { lt, spans } = scan('<div>go [[n<b>ot</b>e]] now</div>');
    expect(spans).toHaveLength(1);
    expect(spans[0].text).toBe('note');
    const r = spanToRange(lt, spans[0].start, spans[0].end);
    expect(r?.toString()).toBe('[[note]]');
  });

  it('maps a url that spans inline tags back to a Range', () => {
    const { lt, spans } = scan('<div>see https://x.co/<b>path</b> ok</div>');
    expect(spanToRange(lt, spans[0].start, spans[0].end)?.toString()).toBe('https://x.co/path');
  });

  it('keeps UTF-16 offsets right after an astral character', () => {
    const { lt, spans } = scan('<div>😀 https://a.co/x 😀 [[Né]]</div>');
    expect(spans.map((s) => s.text)).toEqual(['https://a.co/x', 'Né']);
    expect(lt.text.slice(spans[0].start, spans[0].end)).toBe('https://a.co/x');
    expect(spanToRange(lt, spans[0].start, spans[0].end)?.toString()).toBe('https://a.co/x');
    expect(spanToRange(lt, spans[1].start, spans[1].end)?.toString()).toBe('[[Né]]');
  });

  it('records each text node at its offset, sorted', () => {
    const { lt } = scan('<div>ab<b>cd</b></div><div>ef</div>');
    const starts = lt.nodes.map((n) => n.start);
    expect([...starts].sort((a, b) => a - b)).toEqual(starts);
    for (const { node, start } of lt.nodes) {
      expect(lt.text.slice(start, start + node.data.length)).toBe(node.data);
    }
  });

  it('does not match a scheme with a dotless Latin small long s (ASCII-only case fold)', () => {
    expect(scan('<div>httpſ://a.co</div>').spans).toEqual([]);
  });

  it('returns null for an empty or out-of-range span', () => {
    const { lt } = scan('<div>abc</div>');
    expect(spanToRange(lt, 2, 2)).toBeNull();
    expect(spanToRange(lt, 0, 9999)).toBeNull();
  });

  it('maps a span in the middle of a long document (binary search)', () => {
    const body = Array.from({ length: 200 }, (_, i) => `<div>line ${i}</div>`).join('') + '<div>https://end.co/z</div>';
    const { lt, spans } = scan(body);
    expect(spanToRange(lt, spans[0].start, spans[0].end)?.toString()).toBe('https://end.co/z');
  });
  // Live-editor DOM shapes. innerHTML cannot build these (the parser merges
  // adjacent text and drops empty nodes), so they are assembled by hand.
  describe('live-editor DOM shapes', () => {
    const t = (s: string) => document.createTextNode(s);
    const el = (name: string, ...kids: Node[]) => {
      const e = document.createElement(name);
      e.append(...kids);
      return e;
    };

    it('reads empty and adjacent text nodes inside one URL as one span', () => {
      const div = el('div', t('see '), t(''), t('https://x'), t(''), t('.co/'), t('pa'), t('th'), t(' ok'));
      const lt = buildLinkText(div);
      const spans = scanLinks(lt);
      expect(spans).toHaveLength(1);
      expect(spans[0].text).toBe('https://x.co/path');
      expect(spanToRange(lt, spans[0].start, spans[0].end)?.toString()).toBe('https://x.co/path');
    });

    it('marks a URL whose first char is the first char of an anchor text as inside it', () => {
      const div = el('div', t('go '), el('a', t('https://a.co')), t(' now'));
      const [span] = scanLinks(buildLinkText(div));
      expect(span.text).toBe('https://a.co');
      expect(span.insideAnchor).toBe(true);
    });

    it('ends a range inside the first element when the span hugs its end', () => {
      const b1 = el('b', t('https://a.co/x'));
      const b2 = el('b', t(' more'));
      const div = el('div', t('see '), b1, b2);
      const lt = buildLinkText(div);
      const [span] = scanLinks(lt);
      const r = spanToRange(lt, span.start, span.end)!;
      expect(r.toString()).toBe('https://a.co/x');
      expect(r.endContainer).toBe(b1.firstChild);
      expect(r.endOffset).toBe('https://a.co/x'.length);
    });

    it('does not treat an anchor ABOVE the scanned root as an anchor around the span', () => {
      const root = el('div', t('https://a.co [[n]]'));
      el('a', root);
      const spans = scanLinks(buildLinkText(root));
      expect(spans.map((s) => s.text)).toEqual(['https://a.co', 'n']);
      expect(spans.map((s) => s.insideAnchor)).toEqual([false, false]);
    });

    it('still marks an anchor inside the scanned root', () => {
      const root = el('div', el('a', t('https://a.co')));
      el('a', root);
      expect(scanLinks(buildLinkText(root))[0].insideAnchor).toBe(true);
    });

    it('ignores an SVG <a> — only an HTML anchor makes a span insideAnchor', () => {
      const svgA = document.createElementNS('http://www.w3.org/2000/svg', 'a');
      svgA.append(t('https://a.co'));
      const div = el('div', svgA);
      expect(scanLinks(buildLinkText(div))[0].insideAnchor).toBe(false);
    });
  });
});
