// @vitest-environment jsdom
//
// The first-run door and the brand mark (2026-11-09 workspace review: "the
// first-run screen is least specific: emoji logo, 'technical users', privacy
// line names Google for every provider", and it could not scroll).
//
// DESIGN.md, The Brand Mark Rule: the Thai mark is the one Thai string in the
// UI. It lives in BrandMark.svelte only, appears on the sign-in screen and in
// About only, and is hidden from screen readers because the English name sits
// beside it.
import './components/__fixtures__/dialogStub';
import { describe, it, expect, vi, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { mount, unmount, flushSync, tick } from 'svelte';
import { parseTokens, parseColor, composite, contrast, type RGB } from './color';
import { findAll, sourceFiles } from './designGuards.test';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string) =>
    Promise.resolve(cmd === 'get_oauth_config' ? { credentials_available: true } : false),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(() => Promise.resolve(null)) }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: () => Promise.resolve('0.0.0-test') }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
  emit: () => Promise.resolve(),
}));
vi.mock('./stores/platform', async () => {
  const { writable } = await import('svelte/store');
  return { isAndroid: writable(false), isMacos: writable(false), supportsIcloud: writable(true) };
});

import AuthScreen from './components/AuthScreen.svelte';
import About from './components/About.svelte';
import { aboutModalOpen } from './stores/ui';

const MARK_FILE = 'src/lib/components/BrandMark.svelte';
const CSS = readFileSync('src/styles/tokens.css', 'utf8');
const LIGHT = parseTokens(CSS, ':root');
const DARK = { ...LIGHT, ...parseTokens(CSS, '@media (prefers-color-scheme: dark)') };

function rgb(tokens: Record<string, string>, name: string): RGB {
  const c = parseColor(tokens[name]);
  if (!c || c.alpha !== 1) throw new Error(`${name} is not opaque: ${tokens[name]}`);
  return c.rgb;
}

describe('The Brand Mark Rule', () => {
  it('the mark is the only Thai in the UI, in one file', () => {
    const thai = findAll(/[฀-๿]+/gu);
    expect(thai).toEqual([`${MARK_FILE}:15  จด`]);
  });

  it('appears on the sign-in screen and in About, nowhere else', () => {
    const users = sourceFiles()
      .filter(({ text }) => /<BrandMark\b/.test(text))
      .map(({ path }) => path)
      .sort();
    expect(users).toEqual(['src/lib/components/About.svelte', 'src/lib/components/AuthScreen.svelte']);
  });

  it('is hidden from screen readers', () => {
    expect(readFileSync(MARK_FILE, 'utf8')).toMatch(/<span class="brand-mark" aria-hidden="true">/);
  });

  it('only the mark uses the mark size', () => {
    const hits = findAll(/var\(--size-mark\)/g).filter((h) => !h.startsWith(`${MARK_FILE}:`));
    expect(hits).toEqual([]);
  });

  // Amber Ink (never Lamp Amber: The Two Ambers Rule) on the panel, under
  // the brightest part of the lamp's glow. The mark is 56px bold, so large
  // text's 3:1 is the floor; it clears the body-text 4.5:1 anyway.
  for (const [theme, t] of [['light', LIGHT], ['dark', DARK]] as const) {
    it(`reads on the lamp glow in the ${theme} theme`, () => {
      const glow = parseColor(t['--lamp-glow'])!;
      const ground = composite(glow.rgb, glow.alpha, rgb(t, '--surface-panel'));
      expect(contrast(rgb(t, '--accent-action'), ground)).toBeGreaterThanOrEqual(4.5);
    });
  }

  it('the lamp grows only with motion allowed; under reduced motion it only brightens', () => {
    const css = readFileSync(MARK_FILE, 'utf8');
    expect(css).toMatch(/animation: lamp-on var\(--dur-focal\) var\(--ease-out\) both/);
    const still = css.slice(css.indexOf('@keyframes lamp-on-still'));
    expect(still.slice(0, still.indexOf('@media'))).not.toMatch(/transform/);
    expect(css).toMatch(/@media \(prefers-reduced-motion: reduce\)[\s\S]*animation-name: lamp-on-still/);
  });
});

async function settle() {
  for (let i = 0; i < 8; i++) {
    await tick();
    await Promise.resolve();
  }
  flushSync();
}

describe('the first-run door', () => {
  let host: HTMLElement;
  let app: ReturnType<typeof mount> | null = null;
  afterEach(async () => {
    if (app) await unmount(app);
    app = null;
    host?.remove();
    aboutModalOpen.set(false);
  });

  async function door() {
    host = document.createElement('div');
    document.body.append(host);
    app = mount(AuthScreen, { target: host });
    await settle();
    return host;
  }

  it('shows the mark, hidden, with the English name as the heading', async () => {
    const el = await door();
    expect(el.querySelector('.brand-mark')?.getAttribute('aria-hidden')).toBe('true');
    expect(el.querySelector('h1')?.textContent).toBe('Jodd');
  });

  it('has no emoji logo and no copy written for "technical users"', async () => {
    const el = await door();
    expect(el.querySelector('.logo')).toBeNull();
    expect(el.textContent).not.toMatch(/\u{1F34E}|\u{1FA9F}|technical users/u);
  });

  it('offers iCloud first, then Google, then Microsoft', async () => {
    const el = await door();
    const order = [...el.querySelectorAll('.signin-btn')].map((b) => b.textContent!.trim());
    expect(order).toEqual(['Sign in with iCloud', 'Sign in with Google', 'Sign in with Microsoft']);
  });

  it('names no single provider in the privacy line: it is true for every door', async () => {
    const el = await door();
    const privacy = el.querySelector('.note')!.textContent!;
    expect(privacy).toMatch(/BBMedia never receives a copy/);
    expect(privacy).not.toMatch(/Google|Microsoft|Apple|iCloud/);
  });

  it('scrolls instead of clipping when the card is taller than the window', () => {
    const css = readFileSync('src/lib/components/AuthScreen.svelte', 'utf8');
    const screen = css.slice(css.indexOf('  .auth-screen {'));
    const rule = screen.slice(0, screen.indexOf('}'));
    expect(rule).toMatch(/overflow-y: auto/);
    // align-items: center would push the card's top out of reach.
    expect(rule).not.toMatch(/align-items: center/);
    expect(css).toMatch(/\.auth-card \{[^}]*margin: auto/);
  });

  it('About carries the same mark', async () => {
    host = document.createElement('div');
    document.body.append(host);
    app = mount(About, { target: host });
    aboutModalOpen.set(true);
    await settle();
    expect(document.querySelector('.about-mark .brand-mark')?.getAttribute('aria-hidden')).toBe('true');
  });
});
