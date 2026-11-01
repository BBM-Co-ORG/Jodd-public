// Live pass 2026-10-21 (S23 FE): on the first launch after an update, What's
// New opens from App's onMount and the phone stack is seeded once
// is_authenticated returns — both before App had added its popstate
// listener, which sat at the END of onMount behind several awaited IPC
// calls. A history jump issued meanwhile (a re-seeding unwind, or Android
// Back on What's New) landed with nobody listening, and phoneNav waited for
// it forever: "‹ Folders" did nothing until the app was force-stopped.
// The listener must be in place before onMount's first await. A source
// guard, because mounting App needs the whole Tauri surface.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

describe('App startup order', () => {
  it('adds the popstate listener before onMount awaits anything', () => {
    const src = readFileSync('src/App.svelte', 'utf8');
    const start = src.indexOf('onMount(async () => {');
    expect(start).toBeGreaterThan(-1);
    // Comments mention "await" too; only code counts.
    const body = src.slice(start).replace(/\/\/.*$/gm, '');
    const listener = body.indexOf("window.addEventListener('popstate', handlePopState)");
    const firstAwait = body.search(/\bawait\b/);
    expect(listener).toBeGreaterThan(-1);
    expect(listener).toBeLessThan(firstAwait);
  });
});
