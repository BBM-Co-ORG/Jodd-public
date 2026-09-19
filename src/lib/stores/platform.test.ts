import { describe, it, expect } from 'vitest';
import { deriveIsAndroid, deriveIsMacos, deriveSupportsIcloud } from './platform';

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
    expect(deriveIsMacos('')).toBe(false);
    expect(deriveIsMacos(null)).toBe(false);
  });
});

describe('deriveSupportsIcloud', () => {
  it('is true on the platforms a probe has run on, not just macOS', () => {
    // This used to be `deriveIsMacos`, on the premise that the per-webview
    // data store was a macOS-only API. Windows has a counterpart
    // (`data_directory` against `data_store_identifier`), and a live probe on
    // WebView2 answered every question identically to WKWebView on
    // 2026-09-09 — sign-in, cookie harvest, script injection, jar isolation,
    // silent re-auth, and session survival across a quit.
    expect(deriveSupportsIcloud('macos')).toBe(true);
    expect(deriveSupportsIcloud('windows')).toBe(true);
  });

  it('is true on android too, on a device pass rather than an argument', () => {
    // Android holds the session in the PROCESS's cookie jar
    // (`CookieManager.getInstance()`), not in a webview, so it needs no hidden
    // session window — which is what the old reasoning here assumed it could
    // not have. Measured on a Galaxy S23 FE, 2026-09-10: sign-in, harvest
    // after the sign-in webview closed, read, write past the delayed-merge
    // window, session survival across a restart, and a removal that genuinely
    // empties the jar.
    expect(deriveSupportsIcloud('android')).toBe(true);
  });

  it('is false on platforms nobody has measured', () => {
    // Linux plausibly works — wry gives WebKitGTK the same lever — but
    // plausibly is not measured, and Jodd ships no Linux target. iOS has
    // neither a target nor a device. Add either by running the probe there,
    // not by reasoning about it.
    expect(deriveSupportsIcloud('linux')).toBe(false);
    expect(deriveSupportsIcloud('ios')).toBe(false);
  });

  it('fails CLOSED when the platform is unknown', () => {
    // The inverse of deriveIsAndroid's rule, and deliberately so. Offering
    // sign-in on a platform nobody has measured produces a window that may
    // never hold a session, with nothing on screen to explain why — whereas
    // deriveIsAndroid fails OPEN so an unknown desktop keeps its features.
    expect(deriveSupportsIcloud('')).toBe(false);
    expect(deriveSupportsIcloud(null)).toBe(false);
  });
});
