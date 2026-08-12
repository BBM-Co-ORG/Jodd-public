// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { deriveAndroidLayoutMode, ANDROID_TABLET_BREAKPOINT } from './viewport';

describe('deriveAndroidLayoutMode', () => {
  it('is phone below the tablet breakpoint', () => {
    expect(deriveAndroidLayoutMode(ANDROID_TABLET_BREAKPOINT - 1)).toBe('phone');
  });

  it('is tablet at or above the tablet breakpoint', () => {
    expect(deriveAndroidLayoutMode(ANDROID_TABLET_BREAKPOINT)).toBe('tablet');
    expect(deriveAndroidLayoutMode(1200)).toBe('tablet');
  });

  it('is phone for a narrow phone width', () => {
    expect(deriveAndroidLayoutMode(360)).toBe('phone');
  });
});
