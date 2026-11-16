// @vitest-environment jsdom
import { describe, it, expect, vi } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { targetFromHref, targetFromSpan, NAMED_APP_SCHEMES, type LinkTarget } from './target';
import { primaryOpen, chipActions, type LinkContext } from './handlers';

function ctx(): LinkContext {
  return {
    openUrl: vi.fn(async () => {}),
    openNote: vi.fn(async () => {}),
    scrollToAnchor: vi.fn(),
    copy: vi.fn(),
    editLink: vi.fn(),
    removeLink: vi.fn(),
  };
}
const href = (h: string, a?: HTMLAnchorElement) => targetFromHref(h, a) as LinkTarget;
const ids = (t: LinkTarget) => chipActions(t).map((a) => a.id);

describe('primaryOpen', () => {
  it('opens web through the opener by both gestures', () => {
    const c = ctx();
    const t = href('https://example.com');
    expect(primaryOpen(t, c, 'chip')).toBe(true);
    expect(primaryOpen(t, c, 'modifier')).toBe(true);
    expect(c.openUrl).toHaveBeenCalledTimes(2);
    expect(c.openUrl).toHaveBeenCalledWith('https://example.com');
  });

  it('opens mailto and tel by both gestures (today\'s modifier-click keeps working)', () => {
    const c = ctx();
    expect(primaryOpen(href('mailto:a@b.co'), c, 'modifier')).toBe(true);
    expect(primaryOpen(href('tel:+66123'), c, 'chip')).toBe(true);
    expect(c.openUrl).toHaveBeenCalledTimes(2);
  });

  it('never opens inert, and calls nothing', () => {
    const c = ctx();
    const t = href('ms-msdt:x');
    expect(primaryOpen(t, c, 'chip')).toBe(false);
    expect(primaryOpen(t, c, 'modifier')).toBe(false);
    for (const fn of Object.values(c)) expect(fn).not.toHaveBeenCalled();
  });

  it('opens a note through ctx.openNote with the target', () => {
    const c = ctx();
    const t = targetFromSpan({ kind: 'wikilink', text: 'My Plan', start: 0, end: 7, insideAnchor: false });
    expect(primaryOpen(t, c, 'chip')).toBe(true);
    expect(primaryOpen(t, c, 'modifier')).toBe(true);
    expect(c.openNote).toHaveBeenCalledWith(t);
  });

  it('scrolls to an anchor without the hash', () => {
    const c = ctx();
    expect(primaryOpen(href('#meeting-x'), c, 'modifier')).toBe(true);
    expect(c.scrollToAnchor).toHaveBeenCalledWith('meeting-x');
    expect(c.openUrl).not.toHaveBeenCalled();
  });

  it('logs, rather than throws or leaves unhandled, when the open rejects', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const c = ctx();
    (c.openUrl as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('nope'));
    expect(primaryOpen(href('https://x.io'), c, 'chip')).toBe(true);
    await new Promise((r) => setTimeout(r, 0));
    expect(warn).toHaveBeenCalled();
    warn.mockRestore();
  });
});

describe('chipActions', () => {
  it('web from an <a> offers open, copy, edit, remove', () => {
    const a = document.createElement('a');
    expect(ids(href('https://x.io', a))).toEqual(['open', 'copy', 'edit', 'remove']);
    expect(chipActions(href('https://x.io', a)).map((x) => x.label)).toEqual([
      'Open in browser', 'Copy link', 'Edit link', 'Remove link',
    ]);
  });

  it('a bare url offers open, copy', () => {
    expect(ids(href('https://x.io'))).toEqual(['open', 'copy']);
  });

  it('a note offers open, copy (Copy text)', () => {
    const t = targetFromSpan({ kind: 'wikilink', text: 'My Plan', start: 0, end: 7, insideAnchor: false });
    const acts = chipActions(t);
    expect(acts.map((a) => a.id)).toEqual(['open', 'copy']);
    expect(acts.map((a) => a.label)).toEqual(['Open', 'Copy text']);
    const c = ctx();
    acts[1].run(c);
    expect(c.copy).toHaveBeenCalledWith('[[My Plan]]');
  });

  it('inert offers copy only, copying the full uri', () => {
    const t = href('ms-msdt:x');
    expect(ids(t)).toEqual(['copy']);
    const c = ctx();
    chipActions(t)[0].run(c);
    expect(c.copy).toHaveBeenCalledWith('ms-msdt:x');
  });

  it('anchor offers Go to', () => {
    const acts = chipActions(href('#m'));
    expect(acts.map((a) => [a.id, a.label])).toEqual([['open', 'Go to']]);
  });

  it('os mailto/tel: Open + Copy', () => {
    expect(chipActions(href('mailto:a@b.co')).map((a) => a.label)).toEqual(['Open', 'Copy']);
  });

  it('edit and remove call the context with the anchor', () => {
    const a = document.createElement('a');
    const acts = chipActions(href('https://x.io', a));
    const c = ctx();
    acts.find((x) => x.id === 'edit')!.run(c);
    acts.find((x) => x.id === 'remove')!.run(c);
    expect(c.editLink).toHaveBeenCalledWith(a);
    expect(c.removeLink).toHaveBeenCalledWith(a);
  });

  it('copy copies the uri and open opens it', () => {
    const acts = chipActions(href('https://x.io'));
    const c = ctx();
    acts[0].run(c);
    acts[1].run(c);
    expect(c.openUrl).toHaveBeenCalledWith('https://x.io');
    expect(c.copy).toHaveBeenCalledWith('https://x.io');
  });
});

describe('opener sees a lowercase scheme (its globs are case-sensitive)', () => {
  it.each([
    ['HTTPS://x/Path', 'https://x/Path'],
    ['HTTP://Example.com/A?B=C', 'http://Example.com/A?B=C'],
    ['TEL:+66', 'tel:+66'],
    ['Mailto:Bob@Example.com', 'mailto:Bob@Example.com'],
  ])('%s is opened as %s', (input, expected) => {
    const c = ctx();
    const t = href(input);
    expect(t.uri).toBe(expected);
    expect(primaryOpen(t, c, 'chip')).toBe(true);
    expect(c.openUrl).toHaveBeenCalledWith(expected);
  });
});

// --- the capability file -----------------------------------------------------

interface CapabilityCheck {
  schemes: Set<string>;
  problems: string[];
}

/** One literal scheme, then `:*` or `://*`. Anything else is a widening. */
const SCOPE_URL_RE = /^([a-z][a-z0-9+.-]*):(?:\/\/)?\*$/;

/** Collect every `opener:allow-open-url` scope; anything but a single literal
 *  scheme (wildcards, bare permission with no scope, odd shapes) is a problem. */
export function checkOpenerScopes(cap: { permissions?: unknown[] }, file = 'capability'): CapabilityCheck {
  const schemes = new Set<string>();
  const problems: string[] = [];
  for (const e of cap.permissions ?? []) {
    if (e === 'opener:allow-open-url') {
      problems.push(`${file}: bare "opener:allow-open-url" has no scope (allows every URL)`);
      continue;
    }
    if (typeof e !== 'object' || e === null) continue;
    const o = e as { identifier?: string; allow?: Array<{ url?: unknown }> };
    if (o.identifier !== 'opener:allow-open-url') continue;
    if (!Array.isArray(o.allow) || o.allow.length === 0) {
      problems.push(`${file}: opener:allow-open-url entry has no allow list`);
      continue;
    }
    for (const a of o.allow) {
      const m = typeof a?.url === 'string' ? SCOPE_URL_RE.exec(a.url) : null;
      if (m) schemes.add(m[1]);
      else problems.push(`${file}: opener:allow-open-url url ${JSON.stringify(a?.url)} is not a single literal scheme`);
    }
  }
  return { schemes, problems };
}

describe('checkOpenerScopes (the checker itself)', () => {
  const entry = (...urls: string[]) => ({
    permissions: [{ identifier: 'opener:allow-open-url', allow: urls.map((url) => ({ url })) }],
  });

  it.each(['*', '*:*', 'ms-msdt*', 'http*:*', '*://*', 'slack:/*', 'slack://open', ''])(
    'fails on url %j',
    (url) => {
      expect(checkOpenerScopes(entry(url)).problems).toHaveLength(1);
    },
  );

  it('fails on a bare string permission, which has no scope', () => {
    expect(checkOpenerScopes({ permissions: ['opener:allow-open-url'] }).problems).toHaveLength(1);
  });

  it('fails on an entry with no allow list', () => {
    expect(checkOpenerScopes({ permissions: [{ identifier: 'opener:allow-open-url' }] }).problems).toHaveLength(1);
  });

  it('accepts scheme:* and scheme://* and collects the schemes', () => {
    const r = checkOpenerScopes(entry('slack://*', 'foo-bar:*'));
    expect(r.problems).toEqual([]);
    expect(r.schemes).toEqual(new Set(['slack', 'foo-bar']));
  });

  it('ignores opener:default and other permissions', () => {
    const r = checkOpenerScopes({ permissions: ['opener:default', 'core:default', { identifier: 'x', allow: [{ url: '*' }] }] });
    expect(r.problems).toEqual([]);
    expect(r.schemes.size).toBe(0);
  });

  it('a slack://* scope yields exactly the scheme a matching named entry would declare', () => {
    const r = checkOpenerScopes(entry('slack://*'));
    expect(r.problems).toEqual([]);
    expect(r.schemes).toEqual(new Set(['slack']));
  });
});

describe('opener scopes match NAMED_APP_SCHEMES', () => {
  const dir = 'src-tauri/capabilities';
  const files = readdirSync(dir).filter((f) => f.endsWith('.json'));
  const caps = files.map((f) => ({ f, cap: JSON.parse(readFileSync(`${dir}/${f}`, 'utf8')) }));

  it('reads at least the default capability', () => {
    expect(files).toContain('default.json');
  });

  it('keeps opener:default, which allows exactly http, https, mailto, tel', () => {
    const def = caps.find((c) => c.f === 'default.json')!;
    expect(def.cap.permissions).toContain('opener:default');
  });

  it('every opener:allow-open-url scope is one literal scheme, and the set equals the named schemes', () => {
    const schemes = new Set<string>();
    const problems: string[] = [];
    for (const { f, cap } of caps) {
      const r = checkOpenerScopes(cap, f);
      r.schemes.forEach((s) => schemes.add(s));
      problems.push(...r.problems);
    }
    expect(problems).toEqual([]);
    expect(schemes).toEqual(new Set(NAMED_APP_SCHEMES.map((s) => s.scheme.toLowerCase())));
  });
});
