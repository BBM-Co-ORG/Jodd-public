// The Touched Amber Rule (DESIGN.md): among the editor's states — the save
// state in the toolbar, the phone note header's copy of it, and the status
// chip on the metadata line — amber has one job: another device or an agent
// touched this note. Red is for failures only.
//
// A source check, like designGuards.test.ts: jsdom does not resolve the
// scoped CSS custom properties, so a mounted test could not see the colour.
// The selectors below are the states' own; new states join this list.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

interface Rule { selector: string; body: string }

function rules(path: string): Rule[] {
  const css = readFileSync(path, 'utf8').match(/<style[^>]*>([\s\S]*?)<\/style>/)?.[1] ?? '';
  const out: Rule[] = [];
  const noComments = css.replace(/\/\*[\s\S]*?\*\//g, '');
  for (const m of noComments.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    for (const sel of m[1].split(',')) out.push({ selector: sel.trim(), body: m[2] });
  }
  return out;
}

// Each state selector, with the state it names.
const STATE_SELECTORS: { file: string; re: RegExp }[] = [
  { file: 'src/lib/components/NoteEditor.svelte', re: /^\.save-status\.([\w-]+)$/ },
  { file: 'src/lib/components/EditorMetaLine.svelte', re: /^\.meta-status(?:\[data-kind='(\w+)'\]|\.([\w-]+))(?::hover)?$/ },
  { file: 'src/App.svelte', re: /^\.pane-save-state\.([\w-]+)$/ },
];

// "Touched": another device's edit held back, or a change by someone else
// (an agent, another device) not reviewed yet.
const TOUCHED = new Set(['trust']);
// Failures: a pin push the backend refused, an action that failed. A refused
// note push is said by the editor's notice (its own danger wash, measured in
// tokens.test.ts), not by any of these states: no `.save-status` or
// `.pane-save-state` state is red at all.
const FAILED = new Set(['pinBlocked', 'has-error']);

function stateRules(): { file: string; state: string; selector: string; body: string }[] {
  const found = [];
  for (const { file, re } of STATE_SELECTORS) {
    for (const r of rules(file)) {
      const m = r.selector.match(re);
      if (m) found.push({ file, state: m[1] ?? m[2], selector: r.selector, body: r.body });
    }
  }
  return found;
}

describe('The Touched Amber Rule', () => {
  it('finds the states it guards (the selectors still exist)', () => {
    const states = new Set(stateRules().map((r) => r.state));
    for (const s of ['draft', 'trust', 'pinBlocked']) {
      expect(states, `no rule found for state "${s}"`).toContain(s);
    }
  });

  it('amber on a state means touched, and nothing else', () => {
    const offenders = stateRules()
      .filter((r) => /var\(--accent[\w-]*\)/.test(r.body) && !TOUCHED.has(r.state))
      .map((r) => `${r.file}  ${r.selector}`);
    expect(offenders).toEqual([]);
  });

  it('red on a state means a failure, and nothing else', () => {
    const offenders = stateRules()
      .filter((r) => /var\(--danger[\w-]*\)/.test(r.body) && !FAILED.has(r.state))
      .map((r) => `${r.file}  ${r.selector}`);
    expect(offenders).toEqual([]);
  });

  it('every touched state reads in Amber Ink', () => {
    for (const r of stateRules().filter((x) => TOUCHED.has(x.state))) {
      expect(r.body, `${r.file}  ${r.selector}`).toMatch(/color:\s*var\(--accent-action\)/);
    }
  });

  it('the trust chip carries the Lamp Amber wash (Amber Ink on it is measured in tokens.test.ts)', () => {
    const trust = stateRules().filter((r) => r.state === 'trust');
    expect(trust.find((r) => !r.selector.endsWith(':hover'))?.body).toMatch(/background:\s*var\(--accent-wash\)/);
    expect(trust.find((r) => r.selector.endsWith(':hover'))?.body).toMatch(/background:\s*var\(--accent-wash-strong\)/);
  });
});
