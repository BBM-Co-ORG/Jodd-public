// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { targetFromHref, targetFromSpan, NAMED_APP_SCHEMES } from './target';
import type { LinkSpan } from './scan';

const span = (kind: LinkSpan['kind'], text: string): LinkSpan => ({
  kind, text, start: 0, end: text.length, insideAnchor: false,
});

describe('targetFromHref', () => {
  it('classifies http and https, any case, as web', () => {
    expect(targetFromHref('https://example.com/a')?.kind).toBe('web');
    expect(targetFromHref('HTTP://example.com')?.kind).toBe('web');
    expect(targetFromHref('  https://example.com ')?.uri).toBe('https://example.com');
  });

  it('classifies mailto and tel as os', () => {
    expect(targetFromHref('mailto:a@b.co')?.kind).toBe('os');
    expect(targetFromHref('TEL:+66123')?.kind).toBe('os');
  });

  it('classifies a #fragment as anchor and keeps the href as the uri', () => {
    const t = targetFromHref('#meeting-x');
    expect(t?.kind).toBe('anchor');
    expect(t?.uri).toBe('#meeting-x');
  });

  it('uses the anchor text as the label, else the uri', () => {
    const a = document.createElement('a');
    a.textContent = '  Meeting  ';
    expect(targetFromHref('#m', a)?.label).toBe('Meeting');
    expect(targetFromHref('#m', a)?.anchor).toBe(a);
    const empty = document.createElement('a');
    expect(targetFromHref('#m', empty)?.label).toBe('#m');
    expect(targetFromHref('https://x.io')?.label).toBe('https://x.io');
  });

  it('draws unknown schemes as inert links while the named list is empty', () => {
    expect(NAMED_APP_SCHEMES).toEqual([]);
    expect(targetFromHref('applenotes:x')?.kind).toBe('inert');
    expect(targetFromHref('ms-msdt:x')?.kind).toBe('inert');
    expect(targetFromHref('search-ms:query=x')?.kind).toBe('inert');
    expect(targetFromHref('jodd://note/a1b2c3d4')?.kind).toBe('inert');
  });

  it('is not a link at all for javascript, data, file, vbscript, relative and empty', () => {
    for (const h of [
      'javascript:alert(1)', 'JaVaScRiPt:alert(1)', 'data:text/html,x', 'file:///etc/passwd',
      'vbscript:x', '/rel', 'rel/path', '', '   ', '//host/x', '?q=1',
    ]) {
      expect(targetFromHref(h), h).toBeNull();
    }
  });

  it('is not fooled by whitespace or control characters inside a scheme', () => {
    for (const h of ['java\tscript:alert(1)', 'java\nscript:alert(1)', ' javascript:alert(1)', '\u0001javascript:x']) {
      const t = targetFromHref(h);
      expect(t === null || (t.kind !== 'web' && t.kind !== 'os'), h).toBe(true);
    }
  });

  it('never throws on garbage', () => {
    expect(() => targetFromHref('http://[bad')).not.toThrow();
    expect(() => targetFromHref(':::')).not.toThrow();
  });
});

describe('targetFromSpan', () => {
  it('turns a wikilink with an 8-hex suffix into jodd://note/<id8>', () => {
    const t = targetFromSpan(span('wikilink', 'plan-a1b2c3d4'));
    expect(t.kind).toBe('note');
    expect(t.uri).toBe('jodd://note/a1b2c3d4');
    expect(t.key).toBe('plan-a1b2c3d4');
    expect(t.label).toBe('plan-a1b2c3d4');
  });

  it('lowercases the id, as parse_link_key does', () => {
    expect(targetFromSpan(span('wikilink', 'Plan-A1B2C3D4')).uri).toBe('jodd://note/a1b2c3d4');
  });

  it('falls back to the title form for a plain title', () => {
    expect(targetFromSpan(span('wikilink', 'My Plan')).uri).toBe('jodd://note?title=My%20Plan');
  });

  it('falls back to the title form when the suffix is not hex or not 8 long', () => {
    expect(targetFromSpan(span('wikilink', 'plan-a1b2c3dz')).uri).toBe('jodd://note?title=plan-a1b2c3dz');
    expect(targetFromSpan(span('wikilink', 'plan-a1b2c3d')).uri).toBe('jodd://note?title=plan-a1b2c3d');
  });

  it('trims the inner for the uri but keeps the raw inner as the key', () => {
    const t = targetFromSpan(span('wikilink', ' My Plan '));
    expect(t.uri).toBe('jodd://note?title=My%20Plan');
    expect(t.key).toBe(' My Plan ');
  });

  it('routes a url span through targetFromHref', () => {
    expect(targetFromSpan(span('url', 'https://example.com')).kind).toBe('web');
  });

  it('gives an unopenable url span an inert target rather than null', () => {
    const t = targetFromSpan(span('url', 'file:///etc/passwd'));
    expect(t.kind).toBe('inert');
  });
});
