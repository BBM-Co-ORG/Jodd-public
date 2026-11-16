import { describe, it, expect } from 'vitest';
import { shouldFlush } from './lifecycle';

describe('shouldFlush', () => {
  it('flushes when the app is being hidden', () => {
    // Last chance to push dirty rows before Android freezes the process.
    expect(shouldFlush('hidden')).toBe(true);
  });

  it('flushes when the app becomes visible again', () => {
    // Catch up immediately instead of waiting out the remaining 5s sleep.
    expect(shouldFlush('visible')).toBe(true);
  });

  it('does not flush for any other state', () => {
    expect(shouldFlush('prerender')).toBe(false);
    expect(shouldFlush('')).toBe(false);
  });
});
