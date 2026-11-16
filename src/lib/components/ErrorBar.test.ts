// @vitest-environment jsdom
//
// The app-level error surface.
//
// Two defects this pins, both live on 2026-09-20 behind one screenshot
// ("Folder not found: __TRASH__" sitting under an unrelated folder):
//
//  1. PLACEMENT. The bar used to live inside NoteEditor.svelte, which
//     App.svelte swaps out for TrashPreview whenever '__TRASH__' is
//     selected. So an error raised while the user was in Recently Deleted
//     rendered nowhere at all, then appeared the moment they opened a note
//     in some other folder — looking like that folder had failed. A global
//     error store needs a surface that does not depend on which pane won.
//
//  2. LIFETIME. Nothing cleared it. The only `error.set(null)` outside
//     AuthScreen was NoteEditor's save path, so a one-off failure stayed on
//     screen until the user happened to save a note or sign in again.
//
// The fix is deliberately NOT "clear it on navigation": this same store
// carries `handleAuthLoss`'s "Signed out — Keychain credentials were
// removed. Please sign in again." (App.svelte), which is an account state
// the user must act on, not a transient failure. Auto-expiring the bar
// would hide it. Dismissal is the user's call instead.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { parseTokens, parseColor, composite, contrast, type RGB } from '../color';
import { mount, unmount, flushSync } from 'svelte';
import { get } from 'svelte/store';
import { error } from '../stores/notes';
import { failureText, listingFailureText } from '../errorText';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

import ErrorBar from './ErrorBar.svelte';

function render() {
  const target = document.createElement('div');
  document.body.appendChild(target);
  const host = mount(ErrorBar, { target });
  flushSync();
  return { target, host };
}

describe('ErrorBar', () => {
  let host: ReturnType<typeof mount> | null = null;

  beforeEach(() => {
    document.body.innerHTML = '';
    error.set(null);
  });

  afterEach(() => {
    if (host) unmount(host);
    host = null;
    error.set(null);
  });

  it('shows nothing while there is no error', () => {
    const r = render();
    host = r.host;
    expect(r.target.querySelector('.error-bar')).toBeNull();
  });

  it('shows the message once an error is set', () => {
    const r = render();
    host = r.host;

    error.set('Folder not found: __TRASH__');
    flushSync();

    const bar = r.target.querySelector('.error-bar');
    expect(bar).not.toBeNull();
    expect(bar!.textContent).toContain('Folder not found: __TRASH__');
  });

  // The stuck-banner half. Before the dismiss existed the only way out was
  // saving a note or signing in again.
  it('clears the store and hides itself when dismissed', () => {
    const r = render();
    host = r.host;

    error.set('Failed to move note: nope');
    flushSync();

    const dismiss = r.target.querySelector<HTMLButtonElement>('button[aria-label="Dismiss error"]');
    expect(dismiss).not.toBeNull();

    dismiss!.click();
    flushSync();

    expect(get(error)).toBeNull();
    expect(r.target.querySelector('.error-bar')).toBeNull();
  });

  // Placement half: the bar must not depend on a note being open or on
  // which pane App.svelte chose. Mounting it standalone, with no editor and
  // no selected note anywhere, is exactly the Recently Deleted case.
  it('renders with no note open and no editor mounted', () => {
    const r = render();
    host = r.host;

    error.set('Folder not found: __TRASH__');
    flushSync();

    expect(r.target.querySelector('.error-bar')!.textContent).toContain('Folder not found');
  });
});

// The bar is fixed over the panes, so whatever is under it shows through a
// translucent background: an 8% wash let note text cross the message, which
// measured 2.18:1 where they overlapped (workspace review 2026-11-09). It
// paints the wash over an opaque surface, and its close control is a drawn
// icon with a full touch target, not a "×" glyph.
describe('ErrorBar surface', () => {
  const src = readFileSync('src/lib/components/ErrorBar.svelte', 'utf8');
  const rule = src.match(/\.error-bar \{([^}]*)\}/)![1];

  it('paints the wash over an opaque surface, never the wash alone', () => {
    const bg = rule.match(/background:\s*([^;]+);/)![1];
    expect(bg).toContain('var(--danger-wash)');
    expect(bg).toContain('var(--surface-panel)');
  });

  const css = readFileSync('src/styles/tokens.css', 'utf8');
  const themes: [string, Record<string, string>, RGB][] = [
    ['light', parseTokens(css, ':root'), [255, 255, 255]],
    ['dark', parseTokens(css, "@media (prefers-color-scheme: dark)"), [0, 0, 0]],
  ];
  for (const [label, t] of themes) {
    it(`${label}: --danger on that surface clears 4.5:1`, () => {
      const panel = parseColor(t['--surface-panel'])!;
      expect(panel.alpha).toBe(1);
      const wash = parseColor(t['--danger-wash'])!;
      const bg = composite(wash.rgb, wash.alpha, panel.rgb);
      const fg = parseColor(t['--danger'])!.rgb;
      expect(contrast(fg, bg)).toBeGreaterThanOrEqual(4.5);
    });
  }

  it('the close control is an icon with a full touch target', () => {
    const r = render();
    error.set('Failed to move note: nope');
    flushSync();
    const btn = r.target.querySelector<HTMLButtonElement>('button[aria-label="Dismiss error"]')!;
    expect(btn.querySelector('svg')).not.toBeNull();
    expect(btn.textContent?.trim()).toBe('');
    const dismiss = src.match(/\.error-dismiss \{([^}]*)\}/)![1];
    expect(dismiss).toMatch(/min-(?:width|height):\s*var\(--hit-target-desktop\)/);
    expect(src).toMatch(/@media \(pointer: coarse\)[\s\S]*?\.error-dismiss[\s\S]*?var\(--touch-target\)/);
    unmount(r.host);
  });
});

// Harden-copy pass: the backend's own text is a details line, and being
// offline or rate-limited is not shown as a failure (errorText.ts).
describe('ErrorBar wording', () => {
  let host: ReturnType<typeof mount> | null = null;
  afterEach(() => {
    if (host) unmount(host);
    host = null;
    error.set(null);
  });

  it('shows the details line apart from the sentence, without its prefix', () => {
    const r = render();
    host = r.host;
    error.set(failureText('delete the note', 'permanent: Gmail 403'));
    flushSync();
    const lines = [...r.target.querySelectorAll('.error-text p')];
    expect(lines.map((p) => p.textContent)).toEqual(["Couldn't delete the note.", 'Gmail 403']);
    expect(lines[1].classList.contains('error-detail')).toBe(true);
    expect(r.target.querySelector('.error-bar')!.getAttribute('role')).toBe('alert');
  });

  it('offline is calm: neutral, announced politely, still dismissible', () => {
    const r = render();
    host = r.host;
    error.set(listingFailureText('Gmail', 'transient: error sending request for url (https://x)'));
    flushSync();
    const bar = r.target.querySelector('.error-bar')!;
    expect(bar.classList.contains('calm')).toBe(true);
    expect(bar.getAttribute('role')).toBe('status');
    expect(bar.textContent).not.toMatch(/transient|https/);
    r.target.querySelector<HTMLButtonElement>('button[aria-label="Dismiss"]')!.click();
    flushSync();
    expect(get(error)).toBeNull();
  });

  const css = readFileSync('src/styles/tokens.css', 'utf8');
  const LIGHT = parseTokens(css, ':root');
  const DARK = { ...LIGHT, ...parseTokens(css, '@media (prefers-color-scheme: dark)') };
  for (const [label, t] of [['light', LIGHT], ['dark', DARK]] as const) {
    it(`${label}: the details line and the calm bar read at 4.5:1`, () => {
      const panel = parseColor(t['--surface-panel'])!.rgb;
      const wash = parseColor(t['--danger-wash'])!;
      const alarm = composite(wash.rgb, wash.alpha, panel);
      expect(contrast(parseColor(t['--text-secondary'])!.rgb, alarm)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(parseColor(t['--text'])!.rgb, panel)).toBeGreaterThanOrEqual(4.5);
    });
  }
});
