import { describe, expect, it, vi } from 'vitest';
import { createFrameGuard } from './frameGuard';

/** A hand-cranked animation-frame queue and clock. */
function fakeFrames() {
  let queue: Array<() => void> = [];
  let now = 0;
  let hidden = false;
  return {
    deps: {
      raf: (cb: () => void) => { queue.push(cb); },
      now: () => now,
      hidden: () => hidden,
    },
    /** Run every callback queued so far, as one frame, after `ms` passed. */
    frame(ms: number) {
      now += ms;
      const run = queue;
      queue = [];
      run.forEach((cb) => cb());
    },
    setHidden(h: boolean) { hidden = h; },
    get pending() { return queue.length; },
  };
}

describe('createFrameGuard', () => {
  it('trips when the frame after a paint takes longer than the threshold', () => {
    const f = fakeFrames();
    const onTrip = vi.fn();
    const g = createFrameGuard(500, onTrip, f.deps);
    g.watch();
    f.frame(16); // first callback: before the rendering update that carries the paint
    f.frame(2_400); // second: after it — 2.4 s later
    expect(onTrip).toHaveBeenCalledWith(2_400);
    expect(g.tripped).toBe(true);
  });

  it('does not trip on a normal frame', () => {
    const f = fakeFrames();
    const onTrip = vi.fn();
    const g = createFrameGuard(500, onTrip, f.deps);
    g.watch();
    f.frame(16);
    f.frame(16);
    expect(onTrip).not.toHaveBeenCalled();
    expect(g.tripped).toBe(false);
  });

  it('ignores a gap while the window is hidden — frames stop, nothing rendered slowly', () => {
    const f = fakeFrames();
    const onTrip = vi.fn();
    const g = createFrameGuard(500, onTrip, f.deps);
    g.watch();
    f.frame(16);
    f.setHidden(true);
    f.frame(60_000);
    expect(onTrip).not.toHaveBeenCalled();
  });

  it('watches once at a time: many paints in one frame queue one measurement', () => {
    const f = fakeFrames();
    const g = createFrameGuard(500, vi.fn(), f.deps);
    g.watch();
    g.watch();
    g.watch();
    expect(f.pending).toBe(1);
  });

  it('stays tripped and stops watching until reset', () => {
    const f = fakeFrames();
    const onTrip = vi.fn();
    const g = createFrameGuard(500, onTrip, f.deps);
    g.watch();
    f.frame(16);
    f.frame(900);
    g.watch();
    expect(f.pending).toBe(0);
    g.reset();
    expect(g.tripped).toBe(false);
    g.watch();
    expect(f.pending).toBe(1);
    expect(onTrip).toHaveBeenCalledTimes(1);
  });

  it('a reset during a measurement drops that measurement (it belonged to the last note)', () => {
    const f = fakeFrames();
    const onTrip = vi.fn();
    const g = createFrameGuard(500, onTrip, f.deps);
    g.watch();
    f.frame(16);
    g.reset();
    f.frame(5_000);
    expect(onTrip).not.toHaveBeenCalled();
  });
});
