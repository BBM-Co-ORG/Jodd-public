import { describe, it, expect } from 'vitest';
import { deriveIsAndroid, deriveIsMacos } from './platform';

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

describe('deriveIsMacos', () => {
  it('is true for the macos platform string', () => {
    expect(deriveIsMacos('macos')).toBe(true);
  });

  it('is false for every other platform', () => {
    expect(deriveIsMacos('windows')).toBe(false);
    expect(deriveIsMacos('linux')).toBe(false);
    expect(deriveIsMacos('android')).toBe(false);
  });

  it('defaults to false when the platform is unknown', () => {
    // The inverse of deriveIsAndroid's rule, and deliberately so: this gate
    // fails CLOSED. An unknown platform must not be offered iCloud sign-in,
    // because the per-webview data store the session lives in is a macOS 14+
    // API — offering it elsewhere produces a window that can never hold a
    // session, with nothing on screen to explain why.
    expect(deriveIsMacos('')).toBe(false);
    expect(deriveIsMacos(null)).toBe(false);
  });
});
