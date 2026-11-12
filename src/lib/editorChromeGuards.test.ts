// Source checks from the editor design pass's final re-critique (step 7).
// jsdom resolves neither scoped CSS custom properties nor layout, so the
// rules that size and colour the editor's chrome are pinned in the source,
// as designGuards.test.ts pins the app-wide ones.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

// Every declaration block whose selector list names `selector` (a selector
// can appear in more than one rule), joined. A whole selector list also
// matches as written, for `:global(a, b)`, whose commas are not a list's.
function rule(path: string, selector: string): string {
  const css = readFileSync(path, 'utf8');
  const style = css.slice(css.indexOf('<style'));
  const noComments = style.replace(/\/\*[\s\S]*?\*\//g, '');
  const bodies: string[] = [];
  for (const m of noComments.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const sel = m[1].trim();
    if (sel === selector || sel.split(',').map((x) => x.trim()).includes(selector)) bodies.push(m[2]);
  }
  if (!bodies.length) throw new Error(`${path}: no rule for ${selector}`);
  return bodies.join('\n');
}

const EDITOR = 'src/lib/components/NoteEditor.svelte';
const META = 'src/lib/components/EditorMetaLine.svelte';

describe('desktop hit areas are at least 28px (DESIGN.md)', () => {
  // Measured before: toolbar buttons 23px, find ‹ › 22px, notice and
  // metadata-line buttons 18px.
  for (const [path, sel] of [
    [EDITOR, '.fmt-btn'],
    [EDITOR, '.meta-notice > button'],
    [EDITOR, '.find-close'],
    [META, '.ctx-folder'],
    [META, '.meta-status'],
    [META, '.meta-tags'],
  ] as const) {
    it(`${sel} in ${path.split('/').pop()}`, () => {
      expect(rule(path, sel)).toMatch(/min-height:\s*var\(--hit-target-desktop\)/);
    });
  }
  it('.find-btn and .find-opt are 28px boxes', () => {
    expect(rule(EDITOR, '.find-btn')).toMatch(/height:\s*var\(--hit-target-desktop\)/);
    expect(rule(EDITOR, '.find-opt')).toMatch(/height:\s*var\(--hit-target-desktop\)/);
  });
});

describe('focus is blue (DESIGN.md, Blue Means Focus)', () => {
  it('the find input shows the shared focus ring, not a grey border', () => {
    const r = rule(EDITOR, '.find-input:focus');
    expect(r).toMatch(/border-color:\s*var\(--focus\)/);
    expect(r).toMatch(/box-shadow:\s*var\(--ring-focus\)/);
  });
  it('the link prompt input focuses in blue, not amber', () => {
    expect(rule(EDITOR, '.link-prompt-input:focus')).toMatch(/border-color:\s*var\(--focus\)/);
  });
  it('sources and connections draw focus in --focus', () => {
    expect(rule('src/lib/components/SourceList.svelte', 'a:focus-visible')).toMatch(/var\(--focus\)/);
    expect(rule('src/lib/components/ConnectionsDisclosure.svelte', '.retry:focus-visible')).toMatch(/var\(--focus\)/);
  });
});

describe('buttons use the app\'s face and the type scale', () => {
  it('form controls inherit the font family, not the user agent\'s Arial', () => {
    expect(rule('src/App.svelte', ':global(button, input, select, textarea)')).toMatch(/font-family:\s*inherit/);
  });
  it('⋯ menu rows are body size', () => {
    expect(rule('src/lib/components/NoteMoreMenu.svelte', '.item')).toMatch(/font-size:\s*var\(--size\)/);
  });
});

describe('the touched notices are the loudest notices, not the quietest', () => {
  it('body-size ink on the touched wash, wrapping', () => {
    const r = rule(EDITOR, '.touched-notice');
    expect(r).toMatch(/font-size:\s*var\(--size-sm\)/);
    expect(r).toMatch(/color:\s*var\(--text\)/);
    expect(r).toMatch(/background:\s*var\(--accent-wash\)/);
    expect(r).toMatch(/white-space:\s*normal/);
    expect(r).not.toMatch(/border-left/);
  });
});

describe('the find bar fits a narrow desktop editor (final re-critique)', () => {
  it('is its own size container, and wraps its row below 560px', () => {
    const src = readFileSync(EDITOR, 'utf8');
    expect(rule(EDITOR, '.find-bar')).toMatch(/container-type:\s*inline-size/);
    const q = src.match(/@container \(max-width: 560px\) \{([\s\S]*?)\n {2}\}/)?.[1] ?? '';
    expect(q).toMatch(/\.find-row \{\s*flex-wrap: wrap;/);
    expect(q).toMatch(/\.find-input \{\s*flex: 1 1 12em;/);
  });
  it('does not make the editor pane a size container (it would trap the pane\'s fixed panels)', () => {
    expect(rule(EDITOR, '.editor-pane')).not.toMatch(/container-type/);
  });
  it('its remaining controls meet 28px on desktop', () => {
    expect(rule(EDITOR, '.find-toggle')).toMatch(/min-height:\s*var\(--hit-target-desktop\)/);
    expect(rule(EDITOR, '.find-btn-text')).toMatch(/min-height:\s*var\(--hit-target-desktop\)/);
    expect(rule(EDITOR, '.find-opt')).toMatch(/min-width:\s*var\(--hit-target-desktop\)/);
    expect(rule(EDITOR, '.link-prompt-btn')).toMatch(/min-height:\s*var\(--hit-target-desktop\)/);
  });
});
