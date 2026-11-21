import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

// The note-list search field sets `outline: none` on its input, which also
// beats App.svelte's global :focus-visible ring (a component-scoped class is
// more specific), so focusing it showed nothing. The ring moved to the pill
// that wraps it. jsdom paints nothing, so this pins the stylesheet: the
// input may drop its outline only while the wrapper draws one.
const css = readFileSync('src/lib/components/NoteList.svelte', 'utf8').split('<style>')[1] ?? '';
const rule = (selector: string) => {
  const at = css.indexOf(`${selector} {`);
  return at < 0 ? null : css.slice(at, css.indexOf('}', at));
};

describe('note-list search focus', () => {
  it('the input drops its outline, so the wrapper must draw one', () => {
    expect(rule('.search-input')).toMatch(/outline:\s*none/);
    const ring = rule('.search-bar:focus-within');
    expect(ring).not.toBeNull();
    expect(ring).toMatch(/outline:\s*2px solid var\(--focus\)/);
  });
});
