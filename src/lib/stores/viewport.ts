import { readable, derived } from 'svelte/store';
import type { Readable } from 'svelte/store';

/**
 * Below this width, Android gets the single-pane phone stack; at or above
 * it, the two-pane tablet layout. Provisional — verify against real
 * landscape-tablet/DeX hardware (see the implementation plan's Task 7)
 * rather than trusting this number blind.
 */
export const ANDROID_TABLET_BREAKPOINT = 700;

/** Pure so the breakpoint is testable without a DOM. */
export function deriveAndroidLayoutMode(width: number): 'phone' | 'tablet' {
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
export const androidLayoutMode: Readable<'phone' | 'tablet'> = derived(
  viewportWidth,
  deriveAndroidLayoutMode,
);
