// Live pass 2026-10-21 (Galaxy S23 FE, Android 16, drawn edge to edge):
// - portrait: the editor's bottom row (the Sources / Links disclosure) sat
//   under the navigation bar and could not be tapped;
// - landscape (≥ 700 CSS px, so the tablet layout) with the navigation bar on
//   the right: the editor toolbar's ⋯ and #N were clipped by it.
// PhoneBar (the Folders bottom bar) already cleared its inset. jsdom applies
// no component CSS and has no env(), so the rules are pinned at the source,
// as tabletChrome.test.ts pins the status-bar clearance.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

function rule(file: string, selector: string): string {
  const src = readFileSync(file, 'utf8');
  const css = src.slice(src.indexOf('<style>'));
  const esc = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const m = css.match(new RegExp(`\\n\\s*${esc}\\s*\\{([^}]*)\\}`));
  return m ? m[1] : '';
}

describe('safe-area insets (live pass 2026-10-21)', () => {
  it('the editor pane keeps its last row above the navigation bar', () => {
    const pane = rule('src/lib/components/NoteEditor.svelte', '.editor-pane');
    expect(pane).toMatch(/padding-bottom:\s*env\(safe-area-inset-bottom\)/);
    // height: 100dvh plus padding must not overflow the pane.
    expect(pane).toMatch(/box-sizing:\s*border-box/);
    // With the keyboard up the format bar reserves its own room.
    expect(rule('src/lib/components/NoteEditor.svelte', '.editor-pane.format-bar-shown')).toMatch(/padding-bottom:\s*0/);
  });

  it('the tablet layout clears a side navigation bar for every column', () => {
    const layout = rule('src/lib/components/TabletChrome.svelte', '.app-layout');
    expect(layout).toMatch(/padding-left:\s*env\(safe-area-inset-left\)/);
    expect(layout).toMatch(/padding-right:\s*env\(safe-area-inset-right\)/);
    expect(layout).toMatch(/box-sizing:\s*border-box/);
    // The drawer is fixed, so the layout's padding does not reach it.
    expect(rule('src/lib/components/TabletChrome.svelte', '.tablet-drawer')).toMatch(/left:\s*env\(safe-area-inset-left\)/);
  });

  it('the phone layout clears a side navigation bar too (a narrow phone in landscape)', () => {
    const layout = rule('src/App.svelte', '.phone-layout');
    expect(layout).toMatch(/padding-left:\s*env\(safe-area-inset-left\)/);
    expect(layout).toMatch(/padding-right:\s*env\(safe-area-inset-right\)/);
    expect(layout).toMatch(/box-sizing:\s*border-box/);
  });

  it('the fixed format bar clears the sides itself', () => {
    const bar = rule('src/lib/components/FormatBar.svelte', '.format-bar');
    expect(bar).toMatch(/left:\s*env\(safe-area-inset-left\)/);
    expect(bar).toMatch(/right:\s*env\(safe-area-inset-right\)/);
  });
});
