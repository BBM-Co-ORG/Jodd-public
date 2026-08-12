import { describe, it, expect } from 'vitest';
import { deriveIsAndroid } from './platform';

describe('deriveIsAndroid', () => {
  it('is true for the android platform string', () => {
    expect(deriveIsAndroid('android')).toBe(true);
  });

  it('is false for desktop platforms', () => {
    expect(deriveIsAndroid('macos')).toBe(false);
    expect(deriveIsAndroid('windows')).toBe(false);
    expect(deriveIsAndroid('linux')).toBe(false);
  });

  it('defaults to false when the platform is unknown', () => {
    // Gating must fail OPEN on desktop: an unknown platform should keep
    // every feature visible rather than silently hiding LocalFS vaults.
    expect(deriveIsAndroid('')).toBe(false);
    expect(deriveIsAndroid(null)).toBe(false);
  });
});
