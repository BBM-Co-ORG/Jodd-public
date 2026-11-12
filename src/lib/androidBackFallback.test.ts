// The other half of phoneNav's hardwareBackFallback (see its tests): Android
// Back that the WebView refuses (canGoBack() false — at the root, or over
// entries Chromium's history intervention marked skippable) reaches
// MainActivity, which must ask the page before it backgrounds Jodd, and must
// still background — never finish() — when the page declines (gotcha #31).
// A source guard: neither the Kotlin nor App's mount runs under vitest.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const MAIN_ACTIVITY = 'src-tauri/gen/android/app/src/main/java/co/bbmedia/jodd/MainActivity.kt';

describe('Android Back fallback contract', () => {
  it('MainActivity asks the page, then backgrounds — never finish()', () => {
    const kt = readFileSync(MAIN_ACTIVITY, 'utf8').replace(/\/\/.*$/gm, '');
    expect(kt).toContain('evaluateJavascript');
    expect(kt).toContain('__joddHardwareBack');
    expect(kt).toContain('moveTaskToBack(true)');
    expect(kt).not.toMatch(/\bfinish\(\)/);
  });

  it('App installs the page side before onMount awaits anything', () => {
    const src = readFileSync('src/App.svelte', 'utf8');
    const body = src.slice(src.indexOf('onMount(async () => {')).replace(/\/\/.*$/gm, '');
    const install = body.indexOf('__joddHardwareBack');
    expect(install).toBeGreaterThan(-1);
    expect(install).toBeLessThan(body.search(/\bawait\b/));
  });
});
