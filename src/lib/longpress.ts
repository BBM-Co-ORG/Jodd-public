export const LONGPRESS_DELAY_MS = 500;
export const LONGPRESS_MOVE_THRESHOLD_PX = 10;

/**
 * Pure so the "did the finger move too far to still count as a long-press"
 * decision is testable without simulating real TouchEvents (jsdom's
 * TouchEvent/Touch support is unreliable across versions — see the
 * `longpress` action below, which is deliberately thin DOM glue around this).
 */
export function shouldCancelLongPress(
  startX: number,
  startY: number,
  x: number,
  y: number,
  thresholdPx: number,
): boolean {
  return Math.abs(x - startX) > thresholdPx || Math.abs(y - startY) > thresholdPx;
}

export interface LongPressOptions {
  delayMs?: number;
  moveThresholdPx?: number;
}

/**
 * Svelte action: replaces right-click with a touch long-press on elements
 * that already have `oncontextmenu`. Dispatches a real `contextmenu`
 * MouseEvent at the touch point on trigger — the existing handler fires
 * exactly as it does for a real right-click, so every menu (move-to-folder,
 * delete, pin, refetch, tag actions) works unmodified.
 *
 * Not gated on Android — a touch-capable desktop simply gets long-press as a
 * bonus; it can't fire without touch events, so desktop mouse behavior is
 * unaffected.
 */
export function longpress(node: HTMLElement, options: LongPressOptions = {}) {
  const delayMs = options.delayMs ?? LONGPRESS_DELAY_MS;
  const moveThresholdPx = options.moveThresholdPx ?? LONGPRESS_MOVE_THRESHOLD_PX;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let startX = 0;
  let startY = 0;

  function clear() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
  }

  function onTouchStart(e: TouchEvent) {
    if (e.touches.length !== 1) {
      clear();
      return;
    }
    const touch = e.touches[0];
    startX = touch.clientX;
    startY = touch.clientY;
    clear();
    timer = setTimeout(() => {
      timer = null;
      node.dispatchEvent(
        new MouseEvent('contextmenu', {
          bubbles: true,
          cancelable: true,
          clientX: startX,
          clientY: startY,
        }),
      );
    }, delayMs);
  }

  function onTouchMove(e: TouchEvent) {
    if (timer === null) return;
    const touch = e.touches[0];
    if (!touch) return;
    if (shouldCancelLongPress(startX, startY, touch.clientX, touch.clientY, moveThresholdPx)) {
      clear();
    }
  }

  function onTouchEnd() {
    clear();
  }

  node.addEventListener('touchstart', onTouchStart, { passive: true });
  node.addEventListener('touchmove', onTouchMove, { passive: true });
  node.addEventListener('touchend', onTouchEnd);
  node.addEventListener('touchcancel', onTouchEnd);

  return {
    destroy() {
      clear();
      node.removeEventListener('touchstart', onTouchStart);
      node.removeEventListener('touchmove', onTouchMove);
      node.removeEventListener('touchend', onTouchEnd);
      node.removeEventListener('touchcancel', onTouchEnd);
    },
  };
}
