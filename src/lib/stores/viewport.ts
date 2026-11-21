import { readable, derived } from 'svelte/store';
import type { Readable } from 'svelte/store';
import { isAndroid } from './platform';

export type AndroidLayoutMode = 'phone' | 'tablet' | 'tabletWide';

/**
 * Below this width, Android gets the single-pane phone stack; at or above
 * it, the two-pane tablet layout. Provisional — verify against real
 * landscape-tablet/DeX hardware (see the implementation plan's Task 7)
 * rather than trusting this number blind.
 */
export const ANDROID_TABLET_BREAKPOINT = 700;

/**
 * At or above this width the tablet layout has room for its wide variant
 * (landscape tablet / DeX). Still two-pane chrome — callers that only care
 * about "not a phone" compare `!== 'phone'`, never `=== 'tablet'`.
 */
export const ANDROID_TABLET_WIDE_BREAKPOINT = 1024;

/** Pure so the breakpoints are testable without a DOM. */
export function deriveAndroidLayoutMode(width: number): AndroidLayoutMode {
  if (width >= ANDROID_TABLET_WIDE_BREAKPOINT) return 'tabletWide';
  return width >= ANDROID_TABLET_BREAKPOINT ? 'tablet' : 'phone';
}

/**
 * Live viewport width, updated on resize (covers device rotation on
 * Android — phone/tablet mode must re-derive live, not just at mount).
 */
export const viewportWidth: Readable<number> = readable(window.innerWidth, (set) => {
  const onResize = () => set(window.innerWidth);
  window.addEventListener('resize', onResize);
  return () => window.removeEventListener('resize', onResize);
});

/**
 * Callers MUST gate on `$isAndroid` before branching on this value — it is a
 * pure width classifier with no platform awareness of its own. See
 * App.svelte's `{#if $isAndroid}` template branch.
 */
export const androidLayoutMode: Readable<AndroidLayoutMode> = derived(
  viewportWidth,
  deriveAndroidLayoutMode,
);

/** The slice of `window.visualViewport` the inset arithmetic needs. */
export interface VisualViewportLike {
  height: number;
  offsetTop: number;
}

/**
 * How much of the layout viewport the on-screen keyboard covers, in CSS px.
 * Pure so the arithmetic is testable. 0 when there is no visual viewport, when
 * the window itself shrank for the keyboard (adjustResize: `innerHeight`
 * already equals `vv.height`), and never negative.
 */
export function computeKeyboardInset(
  innerHeight: number,
  vv: VisualViewportLike | null,
): number {
  if (!vv) return 0;
  return Math.max(0, Math.round(innerHeight - vv.height - vv.offsetTop));
}

function readInset(): number {
  // jsdom and old WebViews have no visualViewport.
  const vv = typeof window !== 'undefined' ? (window.visualViewport ?? null) : null;
  return computeKeyboardInset(window.innerHeight, vv);
}

/** Raw inset, tracking `visualViewport` resize/scroll and window resize. */
const rawInset: Readable<number> = readable(0, (set) => {
  const update = () => set(readInset());
  update();
  const vv = window.visualViewport ?? null;
  vv?.addEventListener('resize', update);
  vv?.addEventListener('scroll', update);
  window.addEventListener('resize', update);
  return () => {
    vv?.removeEventListener('resize', update);
    vv?.removeEventListener('scroll', update);
    window.removeEventListener('resize', update);
  };
});

/**
 * Keyboard height in CSS px. Always 0 off Android: a desktop window that
 * happens to be shorter than `innerHeight` (devtools docked, zoom) must never
 * be mistaken for a keyboard.
 */
export const keyboardInset: Readable<number> = derived(
  [isAndroid, rawInset],
  ([$isAndroid, $raw]) => ($isAndroid ? $raw : 0),
);

/**
 * Mirrors `keyboardInset` into the `--kb-inset` CSS variable on the document
 * root, so stylesheets can pad for the keyboard without a store. Returns the
 * unsubscribe function — call it on teardown.
 */
export function installKeyboardInsetVar(): () => void {
  return keyboardInset.subscribe((px) => {
    document.documentElement.style.setProperty('--kb-inset', `${px}px`);
  });
}
