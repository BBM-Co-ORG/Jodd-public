// ✦ (`sparkle`) means "Jodd's AI runs here" (spec 2026-10-28 §9, addendum
// 2026-11-18). Publishing a folder as a Claude Code skill runs none of
// Jodd's AI — it hands the notes to another tool — so its row must not wear
// the mark. Read from source: the row sits inside the folder menu snippet,
// which is costly to mount just to read one icon name.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

describe('sparkle means Jodd AI runs here', () => {
  it('Publish as Claude Code skill uses the terminal icon, not sparkle', () => {
    const src = readFileSync('src/lib/components/Sidebar.svelte', 'utf8');
    const at = src.indexOf('Publish as Claude Code skill…');
    expect(at).toBeGreaterThan(0);
    // The row's icon is the <Icon> just before its label.
    const row = src.slice(src.lastIndexOf('<button', at), at);
    expect(row).toContain('<Icon name="terminal" />');
    expect(row).not.toContain('sparkle');
  });
});
