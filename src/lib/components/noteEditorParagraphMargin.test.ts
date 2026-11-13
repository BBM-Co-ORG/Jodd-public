// A <p> in a note has a gap above and below it, like the browser default and
// like Gmail. App.svelte's global `* { margin: 0 }` reset removes every
// margin, so NoteEditor has to give <p> its own back — and must keep doing so.
//
// Read from the stylesheets themselves: jsdom does not lay out, so the check is
// that the reset exists, that NoteEditor's rule is scoped to the body and is
// not a zero, and that nothing later in the file takes it away again.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const editor = readFileSync('src/lib/components/NoteEditor.svelte', 'utf8');
const app = readFileSync('src/App.svelte', 'utf8');

function rule(css: string, selector: string): string | null {
  const at = css.indexOf(selector + ' {');
  return at < 0 ? null : css.slice(at, css.indexOf('}', at) + 1);
}

describe('paragraph margin in the editor body', () => {
  it('the reset it works against is still there', () => {
    expect(app).toMatch(/:global\(\*, \*::before, \*::after\) \{[^}]*margin: 0;/);
  });

  it('<p> gets a one-line gap above and below, in em', () => {
    const r = rule(editor, '.editor-body :global(p)');
    expect(r).not.toBeNull();
    expect(r).toMatch(/margin: 1em 0;/);
  });

  it('the first <p> sits flush under the title', () => {
    expect(rule(editor, '.editor-body :global(p:first-child)')).toMatch(/margin-top: 0;/);
  });

  it('nothing else in the editor zeroes a <p> margin', () => {
    const zeroed = [...editor.matchAll(/\.editor-body[^{]*\bp\b[^{]*\{[^}]*margin(?:-top|-bottom)?: ?0[^}]*\}/g)]
      .map((m) => m[0])
      .filter((r) => !r.includes(':first-child'));
    expect(zeroed).toEqual([]);
  });
});
