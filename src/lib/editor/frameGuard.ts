// A safety switch for highlight painting. Painting highlights is cheap in
// JavaScript; the cost lands later, in WebKit's own rendering update, where
// no script can interrupt it. What a script CAN do is notice afterwards:
// the gap between the animation frame before that update and the one after
// it is the update's length. Measured after a paint, a gap past the threshold
// means the highlights made the page slow — the caller takes them all down
// and stops painting, instead of the page freezing on every repaint
// (2026-11-18: a frozen webview still left short gaps between its long
// rendering updates, which is where this runs).
//
// A gap while the window is hidden is not a slow frame: frames simply stop.

export interface FrameGuardDeps {
  raf: (cb: () => void) => void;
  now: () => number;
  hidden: () => boolean;
}

export interface FrameGuard {
  /** Measure the next rendering update. Many calls before it lands queue one measurement. */
  watch(): void;
  /** True once a slow frame was seen, until `reset`. */
  readonly tripped: boolean;
  /** Start over (a different note). Drops a measurement in flight. */
  reset(): void;
}

const browserDeps = (): FrameGuardDeps => ({
  raf: (cb) => {
    if (typeof requestAnimationFrame !== 'undefined') requestAnimationFrame(() => cb());
  },
  now: () => performance.now(),
  hidden: () => typeof document !== 'undefined' && document.visibilityState === 'hidden',
});

/** Frames slower than this after a paint pause highlights (a smooth frame is 16 ms). */
export const SLOW_FRAME_MS = 500;

export function createFrameGuard(thresholdMs: number, onTrip: (ms: number) => void, deps: FrameGuardDeps = browserDeps()): FrameGuard {
  let tripped = false;
  let watching = false;
  let epoch = 0;
  return {
    watch() {
      if (tripped || watching) return;
      watching = true;
      const mine = epoch;
      deps.raf(() => {
        if (mine !== epoch) return;
        const start = deps.now();
        const hiddenAtStart = deps.hidden();
        deps.raf(() => {
          if (mine !== epoch) return;
          watching = false;
          const ms = deps.now() - start;
          if (hiddenAtStart || deps.hidden()) return;
          if (ms > thresholdMs) {
            tripped = true;
            onTrip(ms);
          }
        });
      });
    },
    get tripped() {
      return tripped;
    },
    reset() {
      epoch++;
      tripped = false;
      watching = false;
    },
  };
}
