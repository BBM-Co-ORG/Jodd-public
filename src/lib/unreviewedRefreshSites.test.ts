// Ruling R15: an in-app AI write (Extract, ingest, a workflow, auto-link,
// wiki-link appends, action items, Organize) makes a note `unreviewed` with no
// remote-changed behind it, so App.loadNotes never runs and the chip stayed
// missing until focus or the 10-minute poll. Each call site must fire
// refreshUnreviewed itself once the invoke resolves. A source guard, because
// these components are modals whose full flows need a provider to mount.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const SITES: [file: string, command: string][] = [
  ['src/lib/components/LessonExtractModal.svelte', 'extract_note'],
  ['src/lib/components/LessonExtractModal.svelte', 'run_llm_workflow'],
  ['src/lib/components/LessonExtractModal.svelte', 'append_extract_note'],
  ['src/lib/components/LessonExtractModal.svelte', 'append_llm_workflow_note'],
  ['src/lib/components/LessonExtractModal.svelte', 'ingest_sources'],
  ['src/lib/components/LessonExtractModal.svelte', 'apply_action_items'],
  ['src/lib/components/LessonExtractModal.svelte', 'save_note'], // the auto-link "Related" line
  ['src/lib/components/NoteContextMenu.svelte', 'save_note'], // Duplicate, and the auto-link "Related" line
  ['src/lib/components/LinkSuggestionsModal.svelte', 'apply_wiki_link_appends'],
  ['src/lib/components/CurateReview.svelte', 'curate_apply'],
];

/** Every `invoke(... 'command'` in `src`, with the text that follows it. */
function invokesOf(src: string, command: string): string[] {
  const out: string[] = [];
  const re = new RegExp(`invoke(<[^(]*>)?\\(\\s*'${command}'`, 'g');
  for (let m = re.exec(src); m; m = re.exec(src)) out.push(src.slice(m.index, m.index + 1800));
  return out;
}

describe('in-app AI writes refresh the unreviewed set (R15)', () => {
  for (const [file, command] of SITES) {
    it(`${file.split('/').pop()}: ${command}`, () => {
      const sites = invokesOf(readFileSync(file, 'utf8'), command);
      expect(sites.length, `no invoke('${command}') in ${file}`).toBeGreaterThan(0);
      for (const after of sites) expect(after).toContain('refreshUnreviewed([');
    });
  }
});
