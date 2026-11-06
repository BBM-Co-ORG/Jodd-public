// What a link IS. Pure: no Svelte, no stores, no IPC — the editor plugs in
// through `LinkContext` (handlers.ts). Note HTML arrives from Apple sync, Share
// to Jodd, AI workflows and MCP agents, so an href is hostile input; this file
// is where it is sorted into "opens", "drawn but never opens" and "not a link".
// Design: docs/superpowers/specs/2026-10-22-hypermedia-links-design.md.

import type { LinkSpan } from './scan';

export type LinkKind = 'note' | 'anchor' | 'web' | 'os' | 'inert';

export interface LinkTarget {
  kind: LinkKind;
  /** What the chip shows and Copy copies. For a note this is for display only. */
  uri: string;
  label: string;
  /** The raw wikilink inner, for `resolve_note_link`. Set on `note` only. */
  key?: string;
  /** Set when the source was an `<a>` element. */
  anchor?: HTMLAnchorElement;
}

/**
 * Schemes Jodd may hand to the OS beyond `opener:default` (http, https, mailto,
 * tel). Ships empty. An entry here MUST be matched by an `opener:allow-open-url`
 * scope for that scheme alone in src-tauri/capabilities/default.json — a test
 * reads that file and fails when the two differ.
 */
export const NAMED_APP_SCHEMES: ReadonlyArray<{ scheme: string; app: string }> = [];

/** Never a link, however it is dressed: executes or reads local content. */
const REFUSED_SCHEMES: ReadonlySet<string> = new Set(['javascript', 'data', 'file', 'vbscript']);

/** What `opener:default` allows besides http(s). */
const OS_SCHEMES: ReadonlySet<string> = new Set(['mailto', 'tel']);

/** RFC 3986 scheme, anchored. Whitespace or control characters inside the
 *  scheme (`java\tscript:`) fail to match and fall through to "relative". */
const SCHEME_RE = /^([a-z][a-z0-9+.-]*):/i;

export function schemeOf(href: string): string | null {
  const m = SCHEME_RE.exec(href);
  return m ? m[1].toLowerCase() : null;
}

/** The named-app entry for a scheme, if any. */
export function namedApp(scheme: string): { scheme: string; app: string } | undefined {
  return NAMED_APP_SCHEMES.find((n) => n.scheme.toLowerCase() === scheme);
}

/**
 * Sort an href. `null` means "not a link at all" (javascript:, data:, file:,
 * vbscript:, relative, empty); `inert` means "draw it as a link, never open it".
 * Never throws.
 */
export function targetFromHref(href: string, anchor?: HTMLAnchorElement): LinkTarget | null {
  let uri = (href ?? '').trim();
  if (!uri) return null;

  const make = (kind: LinkKind): LinkTarget => {
    const t: LinkTarget = { kind, uri, label: uri };
    if (anchor) {
      t.anchor = anchor;
      if (kind === 'anchor') t.label = (anchor.textContent ?? '').trim() || uri;
    }
    return t;
  };

  if (uri.startsWith('#')) return make('anchor');

  const scheme = schemeOf(uri);
  if (scheme === null) return null; // relative, protocol-relative, query-only
  if (REFUSED_SCHEMES.has(scheme)) return null;
  // tauri-plugin-opener matches the raw string against case-sensitive globs
  // (`https://*`), so what we classify must be exactly what reaches openUrl:
  // lowercase the scheme, leave the rest of the URL untouched.
  if (scheme === 'http' || scheme === 'https') {
    uri = scheme + uri.slice(scheme.length);
    return make('web');
  }
  if (OS_SCHEMES.has(scheme) || namedApp(scheme)) {
    uri = scheme + uri.slice(scheme.length);
    return make('os');
  }
  return make('inert'); // jodd:, ms-msdt:, applenotes:, … — drawn, never opened
}

/** Ported from `parse_link_key` (db.rs): a trailing `-` + 8 hex digits is an id. */
function idSuffix(inner: string): string | null {
  const t = inner.trim();
  const pos = t.lastIndexOf('-');
  if (pos < 0) return null;
  const suffix = t.slice(pos + 1);
  return /^[0-9a-f]{8}$/i.test(suffix) ? suffix.toLowerCase() : null;
}

/**
 * A scanned span → its target. A wikilink is always a `note`; the `jodd://` URI
 * is for display and copy only — resolution goes through `resolve_note_link(key)`.
 * A URL span the scanner found but `targetFromHref` refuses stays visible as `inert`.
 */
export function targetFromSpan(span: LinkSpan): LinkTarget {
  if (span.kind === 'wikilink') {
    const id = idSuffix(span.text);
    const uri = id
      ? `jodd://note/${id}`
      : `jodd://note?title=${encodeURIComponent(span.text.trim())}`;
    return { kind: 'note', uri, label: span.text, key: span.text };
  }
  return targetFromHref(span.text) ?? { kind: 'inert', uri: span.text, label: span.text };
}
