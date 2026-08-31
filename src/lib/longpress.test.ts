// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { shouldCancelLongPress } from './longpress';

describe('shouldCancelLongPress', () => {
  it('does not cancel for a touch that stayed within the threshold', () => {
    expect(shouldCancelLongPress(100, 100, 105, 103, 10)).toBe(false);
  });

  it('cancels once the touch moves past the threshold on either axis', () => {
    expect(shouldCancelLongPress(100, 100, 115, 100, 10)).toBe(true); // x moved
    expect(shouldCancelLongPress(100, 100, 100, 115, 10)).toBe(true); // y moved
  });

  it('treats the threshold as exclusive (exactly-at-threshold does not cancel)', () => {
    expect(shouldCancelLongPress(100, 100, 110, 100, 10)).toBe(false);
  });
});
