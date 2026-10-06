/**
 * Whether a visibility transition should trigger an immediate sync flush.
 *
 * Both directions matter on Android. Going hidden is the last moment to push
 * dirty rows before the OS freezes the process; becoming visible is a chance
 * to catch up rather than waiting out the remaining sleep in the 5s worker
 * loop. Driven from the WebView rather than a Rust-side lifecycle event
 * because `document.visibilitychange` is guaranteed by the WebView, whereas
 * Tauri's Resumed/Paused mapping to the Android activity lifecycle is not
 * documented. See spec §5.
 */
export function shouldFlush(visibilityState: string): boolean {
  return visibilityState === 'hidden' || visibilityState === 'visible';
}
