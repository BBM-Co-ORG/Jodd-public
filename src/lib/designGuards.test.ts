import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const COMPONENT_DIR = 'src/lib/components';

export function sourceFiles(): { path: string; text: string }[] {
  const files = readdirSync(COMPONENT_DIR)
    .filter((f) => f.endsWith('.svelte'))
    .map((f) => join(COMPONENT_DIR, f));
  files.push('src/App.svelte');
  return files.map((path) => ({ path, text: readFileSync(path, 'utf8') }));
}

// Reports "file:line  match" so a failure names the exact site to fix.
export function findAll(re: RegExp): string[] {
  const hits: string[] = [];
  for (const { path, text } of sourceFiles()) {
    text.split('\n').forEach((line, i) => {
      // An attribute selector like [style*='background-color: rgb(255,255,255)']
      // contains colour text, but it is a PATTERN matching content Apple Notes
      // sent us -- not a value this app paints with. Tokenising it would break
      // the match. Strip those before scanning so the colour guards keep their
      // meaning ("do not hardcode a colour you could tokenise") without
      // flagging content-matching selectors.
      const scan = line.replace(/\[style\*='[^']*'( i)?\]/g, '[style-selector]');
      for (const m of scan.matchAll(re)) hits.push(`${path}:${i + 1}  ${m[0]}`);
    });
  }
  return hits;
}

// Finds the index of the '>' that closes the tag opened at `start` (which
// must point at the tag's leading '<'). Tracks quote state and {}
// nesting so it isn't fooled by a '>' inside a string, or inside an
// interpolated attribute expression -- e.g. an arrow function's `=>` or a
// multi-statement handler `onclick={(e) => { ... }}`. A naive
// `[^>]*`-style regex stops at the FIRST '>' it meets, which is exactly
// the '>' inside `=>`, not the real tag close -- that bug is why the
// previous version of the icon-button test only ever inspected 3 of 9
// icon-only buttons in the tree (every button whose only interactive
// attribute was a bare handler reference, with no arrow function, slipped
// past it).
export function findTagEnd(text: string, start: number): number {
  let i = start;
  let braceDepth = 0;
  let quote: string | null = null;
  while (i < text.length) {
    const c = text[i];
    if (quote) {
      if (c === '\\') { i += 2; continue; }
      if (c === quote) quote = null;
      i++;
      continue;
    }
    if (c === '"' || c === "'" || c === '`') { quote = c; i++; continue; }
    if (c === '{') { braceDepth++; i++; continue; }
    if (c === '}') { braceDepth--; i++; continue; }
    if (c === '>' && braceDepth === 0) return i;
    i++;
  }
  return -1;
}

describe('colour discipline', () => {
  it('has no un-spaced rgba() spelling', () => {
    expect(findAll(/rgba\(\d+,\d/g)).toEqual([]);
  });

  it('has no surviving pre-Tier-1 blue in rgba form', () => {
    // The four blues the Tier 1 accent unification was meant to eliminate.
    const olds = /rgba\((?:74, 144, 226|37, 99, 235|74, 107, 138|179, 153, 89), [^)]*\)/g;
    expect(findAll(olds)).toEqual([]);
  });

  it('has no raw accent rgba — --accent-* tokens exist for these', () => {
    expect(findAll(/rgba\(201, 124, 31, [^)]*\)/g)).toEqual([]);
  });

  it('has no raw danger rgba', () => {
    expect(findAll(/rgba\(220, 50, 50, [^)]*\)/g)).toEqual([]);
  });

  it('has no raw rgba() left anywhere in components', () => {
    // Every alpha must be a token, or dark mode cannot flip it.
    expect(findAll(/rgba\([^)]*\)/g)).toEqual([]);
  });
});

describe('icon discipline', () => {
  // Platform marks on the auth screen: full colour is the point.
  const ALLOWED_FILES = new Set(['src/lib/components/AuthScreen.svelte']);

  // Sites where an <Icon> component physically cannot go. Each is commented
  // in place. Keyed file -> glyph so the exemption cannot silently widen.
  //
  // NOTE: the NoteEditor entry below (ⓘ, ●) is documentary only, not
  // enforced — neither glyph is in the CHROME set (both are in the KEEP
  // list), so the guard below never flags them and this entry exempts
  // nothing. Only the LlmProviderSettings ✓ entry is load-bearing (✓ IS in
  // CHROME). Kept for documentation so a future reader can find all three
  // documented exemptions in one place, but do not mistake the first two
  // for proof those sites are covered by the guard.
  const EXEMPT: Record<string, string[]> = {
    // ⓘ inside a data-placeholder ATTRIBUTE; ● inside a CSS content: rule.
    'src/lib/components/NoteEditor.svelte': ['ⓘ', '●'],
    // ✓ built by JS string concatenation, not markup.
    'src/lib/components/LlmProviderSettings.svelte': ['✓'],
  };

  // The exact replace-set from the plan's Icon inventory. Nothing else counts:
  // → ← ● ○ ⓘ ─ are typographic marks and stay.
  const CHROME = new Set([
    '\u{1F4C1}', '\u{1F4A1}', '\u{1F50D}', '\u{1F570}', '\u{1F5D1}', '\u{1F4CC}',
    '\u{1F517}', '\u{1F578}', '\u{1F4CE}', '\u{1F464}', '\u{1F441}', '\u{1F3F7}',
    '\u{1F4DD}', '\u{1F648}', '✕', '✓', '❝', '☐',
    '↩', '▸', '▾', '⚙', '✎', '✏',
    '⎘', '↻',
  ]);

  it('has no emoji or control glyphs left in UI chrome', () => {
    const hits: string[] = [];
    for (const { path, text } of sourceFiles()) {
      if (ALLOWED_FILES.has(path)) continue;
      const exempt = EXEMPT[path] ?? [];
      text.split('\n').forEach((line, i) => {
        const t = line.trim();
        if (t.startsWith('//') || t.startsWith('*') || t.startsWith('/*')) return;
        for (const ch of [...line]) {
          if (CHROME.has(ch) && !exempt.includes(ch)) hits.push(`${path}:${i + 1}  ${ch}`);
        }
      });
    }
    expect(hits).toEqual([]);
  });

  // Returns every icon-only <button>...</button> found in `text`: the open
  // tag is located by brace-aware scanning (see findTagEnd), then the body
  // between the tag close and the next </button> must be nothing but a
  // single self-closing <Icon .../> (whitespace-trimmed).
  function findIconOnlyButtons(text: string): { openTag: string; full: string }[] {
    const results: { openTag: string; full: string }[] = [];
    const openRe = /<button\b/g;
    let m: RegExpExecArray | null;
    while ((m = openRe.exec(text))) {
      const tagStart = m.index;
      const tagEnd = findTagEnd(text, tagStart);
      if (tagEnd === -1) continue;
      const closeIdx = text.indexOf('</button>', tagEnd + 1);
      if (closeIdx === -1) continue;
      const openTag = text.slice(tagStart, tagEnd + 1);
      const body = text.slice(tagEnd + 1, closeIdx).trim();
      if (/^<Icon\b[^>]*\/>$/.test(body)) {
        results.push({ openTag, full: text.slice(tagStart, closeIdx + '</button>'.length) });
      }
      openRe.lastIndex = tagEnd + 1; // resume scanning after this tag's open
    }
    return results;
  }

  it('gives every icon-only button an accessible name', () => {
    const hits: string[] = [];
    for (const { path, text } of sourceFiles()) {
      for (const { openTag, full } of findIconOnlyButtons(text)) {
        if (!/aria-label=|title=/.test(openTag)) hits.push(`${path}  ${full.slice(0, 80)}`);
      }
    }
    expect(hits).toEqual([]);
  });
});

describe('type discipline', () => {
  it('uses size tokens, not literal px font sizes', () => {
    const hits = findAll(/font-size: *\d+px/g).filter(
      // The auth screen logo row sizes the platform-mark emoji, not text.
      (h) => !h.startsWith('src/lib/components/AuthScreen.svelte') || !h.includes('32px'),
    );
    expect(hits).toEqual([]);
  });

  it('never sets line-height below the Thai floor where user text can land', () => {
    // line-height: 1 clips Thai tone marks. Chrome-only rows may use
    // --leading-tight; anything else must be >= 1.5.
    expect(findAll(/line-height: *(?:1|1\.[0-4]\d*) *;/g)).toEqual([]);
  });

  it('uses the mono token, not an ad-hoc monospace stack', () => {
    // Matches any font-family whose value is not `inherit` or a var().
    // NOTE: `[^;]+` alone lets the engine backtrack the ` *` down to zero
    // spaces so the lookahead is checked *before* the space, then the space
    // itself gets swallowed by `[^;]+` -- silently defeating the negative
    // lookahead and flagging `font-family: inherit;` and `var(--font-mono)`
    // as violations. `\S[^;]*` forces the value to start on a non-space
    // character at the exact position the lookahead examined.
    expect(findAll(/font-family: *(?!inherit|var\()\S[^;]*;/g)).toEqual([]);
  });
});

describe('round-trip safety', () => {
  it('never strips inline style from body_html on the frontend', () => {
    // body_html round-trips to Apple Notes. Rewriting it is a correctness
    // change, not a styling one. Inline styles are neutralised at RENDER
    // time via a scoped .editor-body override -- never by mutating content.
    const hits: string[] = [];
    for (const { path, text } of sourceFiles()) {
      for (const m of text.matchAll(/\.replace\([^)]*style[^)]*\)/gi)) {
        hits.push(`${path}  ${m[0].slice(0, 100)}`);
      }
    }
    expect(hits).toEqual([]);
  });
});

describe('copy discipline', () => {
  it('has no Thai in the shipped UI', () => {
    // One language, decided 2026-07-29. Not a style preference: the UI was
    // accidentally bilingual, so an English button opened a Thai screen.
    expect(findAll(/[฀-๿]+/gu)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// form-control theming: the rule, as a pure function over source text.
//
// Exported and text-in/string-out on purpose. The previous version of this
// guard lived entirely inside its `it()` and could only ever be pointed at the
// real component tree, so there was no way to demonstrate it catching the bug
// it existed for -- and it did not. See the fixture tests below.
// ---------------------------------------------------------------------------

// A form control with no `background` falls back to the USER AGENT's white,
// which does not follow the theme. Inheriting is not an option here: the UA
// stylesheet beats inheritance for input/select/textarea. So a control that
// sets `color` (themed, light in dark mode) but no background renders light
// text on a white field and becomes invisible.
//
// This shipped twice. First: Sidebar's new-folder prompt (no background at
// all). Then, with the first version of this guard already in the tree, three
// bare `<input placeholder=… bind:value=… />` in AppSettings' LLM section --
// no class AND no type. The guard's element regex required `class="…"` to even
// see a control, so those were invisible to it; and the stylesheet rule meant
// to catch them, `input[type="text"], input[type="password"]`, is an ATTRIBUTE
// selector, so it did not match an input that never declares `type` (defaulting
// to text at runtime is a DOM behaviour, not an attribute). Guard and
// stylesheet shared the same blind spot, which is why agreement between them
// proved nothing.
//
// So the rule is now: every control must be matched by a selector in its own
// component's <style> that ACTUALLY applies to it -- class, bare element, or
// attribute selector whose conditions the markup satisfies -- and that selector
// must declare a background.

// UA-drawn widgets. The browser paints the box itself; `background` on them is
// either ignored or cosmetic, and there is no text inside to lose contrast
// with. These are the one exemption, and it is keyed on a LITERAL type
// attribute -- a control whose type is an interpolated expression is not
// assumed to be one of these.
const UA_PAINTED_TYPES = new Set(['checkbox', 'radio', 'hidden', 'range', 'color', 'file']);

type Control = { tag: string; line: number; open: string; classes: string[]; type: string | null };

function styleBlock(text: string): string {
  const m = /<style>([\s\S]*)<\/style>/.exec(text);
  return m ? m[1].replace(/\/\*[\s\S]*?\*\//g, '') : '';
}

/** Literal value of `name="…"` / `name='…'`, or null when absent or interpolated. */
function attrLiteral(openTag: string, name: string): string | null {
  const m = new RegExp(`(?:^|[\\s])${name}=(?:"([^"]*)"|'([^']*)')`).exec(openTag);
  return m ? (m[1] ?? m[2]) : null;
}

/**
 * `text` with <script>, <style> and HTML-comment regions blanked out, keeping
 * every newline so reported line numbers still match the file.
 *
 * Needed because these components discuss `<input>` in prose far more often
 * than they render one: five of the first run's hits were sentences in comments
 * ("Clicking a checkbox shifts focus to the <input>") and JS strings that build
 * checkbox markup. A guard whose failure list is mostly commentary teaches
 * people to skim past it.
 */
function markupOnly(text: string): string {
  const blank = (s: string) => s.replace(/[^\n]/g, ' ');
  return text
    .replace(/<script[\s\S]*?<\/script>/g, blank)
    .replace(/<style[\s\S]*?<\/style>/g, blank)
    .replace(/<!--[\s\S]*?-->/g, blank);
}

/**
 * Every input/select/textarea open tag in the MARKUP of `text`. Brace-aware
 * (findTagEnd), so a `type={cond ? 'text' : 'password'}` or an inline arrow
 * handler does not cut the tag short and hide later attributes.
 */
export function findFormControls(source: string): Control[] {
  const text = markupOnly(source);
  const out: Control[] = [];
  const openRe = /<(input|select|textarea)\b/g;
  let m: RegExpExecArray | null;
  while ((m = openRe.exec(text))) {
    const end = findTagEnd(text, m.index);
    if (end === -1) continue;
    const open = text.slice(m.index, end + 1);
    // Static class tokens only. An interpolated token (`class="a {x}"`) cannot
    // be resolved here, so it is not counted as coverage.
    const classAttr = attrLiteral(open, 'class') ?? '';
    out.push({
      tag: m[1],
      line: text.slice(0, m.index).split('\n').length,
      open,
      classes: classAttr.split(/\s+/).filter((c) => c && !c.includes('{')),
      type: attrLiteral(open, 'type'),
    });
    openRe.lastIndex = end + 1;
  }
  return out;
}

/** `selector { decls }` pairs. Rules nested in @media are picked up too. */
function cssRules(css: string): { selector: string; decls: string }[] {
  return [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({
    selector: m[1].trim(),
    decls: m[2],
  }));
}

function declaresBackground(decls: string): boolean {
  return /(?:^|[;{\s])background(?:-color)?\s*:/.test(decls);
}

/**
 * Does `selector` apply to `ctl` in its BASE state?
 *
 * Only the rightmost compound is checked, so an ancestor requirement
 * (`.toggle-row input`) is taken on trust: a background there counts as
 * coverage even though we cannot confirm the control really sits inside
 * `.toggle-row`. Deliberately lenient -- the strict alternative needs a real
 * DOM, and a guard that cries wolf gets muted. The tight half is the part that
 * matters: attribute and class conditions on the control ITSELF must be
 * satisfied by the markup.
 *
 * Pseudo-classes and pseudo-elements disqualify the rule: a background on
 * `:focus` or `::placeholder` does not paint the resting field.
 */
function selectorApplies(selector: string, ctl: Control): boolean {
  return selector.split(',').some((alt) => {
    const compound = alt.trim().split(/[\s>+~]+/).filter(Boolean).pop();
    if (!compound || compound.includes(':')) return false;

    const element = /^[a-zA-Z][\w-]*/.exec(compound)?.[0] ?? null;
    if (element && element !== ctl.tag) return false;

    const classes = [...compound.matchAll(/\.([\w-]+)/g)].map((m) => m[1]);
    if (!classes.every((c) => ctl.classes.includes(c))) return false;

    // [type="text"] and friends: the markup must literally declare the
    // attribute with that value. This is the check the old guard lacked.
    for (const a of compound.matchAll(/\[([\w-]+)(?:([~|^$*]?=)"?([^\]"]*)"?)?\]/g)) {
      const [, name, op, want] = a;
      const have = attrLiteral(ctl.open, name);
      if (have === null) return false;
      if (op && op !== '=') return false; // ~= etc: not worth modelling, stay strict
      if (op && have !== want) return false;
    }

    // A rule with no element, class or attribute is not something we can bind
    // to this control (e.g. a bare `*`).
    return Boolean(element || classes.length || compound.includes('['));
  });
}

/**
 * Sites in `text` whose background would come from the user agent.
 *
 * `globalCss` is App.svelte's style block, where the app-wide control rules
 * live. Today it only grants form controls `color: inherit`, so it never
 * satisfies anything -- but a future `:global(input) { background: … }` there
 * would legitimately cover every control in the tree, and a guard that flagged
 * all of them anyway would be wrong in the loudest possible way.
 */
export function uncoveredFormControls(text: string, globalCss = ''): string[] {
  // `:global(button, input, …)` is Svelte syntax, not CSS -- unwrap it so the
  // selector list inside is matched like any other.
  const css = styleBlock(text) + '\n' + globalCss.replace(/:global\(([^)]*)\)/g, '$1');
  const rules = cssRules(css).filter((r) => declaresBackground(r.decls));
  const hits: string[] = [];
  for (const ctl of findFormControls(text)) {
    if (ctl.type && UA_PAINTED_TYPES.has(ctl.type)) continue;
    if (/(?:^|\s)style="[^"]*background/.test(ctl.open)) continue;
    if (rules.some((r) => selectorApplies(r.selector, ctl))) continue;
    const desc = ctl.classes.length
      ? `class="${ctl.classes.join(' ')}"`
      : ctl.type
        ? `type="${ctl.type}"`
        : 'no class, no type';
    hits.push(`:${ctl.line}  <${ctl.tag} ${desc}>`);
  }
  return hits;
}

describe('form-control theming', () => {
  it('gives every input/select/textarea an explicit background', () => {
    const files = sourceFiles();
    const globalCss = styleBlock(files.find((f) => f.path === 'src/App.svelte')!.text);
    const hits: string[] = [];
    for (const { path, text } of files) {
      for (const h of uncoveredFormControls(text, globalCss)) hits.push(path + h);
    }
    expect(hits).toEqual([]);
  });

  // The regression proof. Verbatim shape of the three AppSettings inputs added
  // in a521078 (branch worktree-ask-jodd) that rendered themed near-white text
  // on the UA's white field in dark mode, alongside the two controls in the
  // same section that were fine. If this test ever passes, the guard has
  // stopped being able to see the bug it exists for.
  const PRE_FIX = `
    <label>Preset
      <input placeholder="claude" bind:value={appLlm.llm.agent_preset} />
    </label>
    <label>Provider
      <select bind:value={appLlm.llm.provider}><option value="none">None</option></select>
    </label>
    <label>API key
      <input type="password" placeholder="sk-…" bind:value={appLlmKeyInput} />
    </label>
    <label class="checkbox">
      <input type="checkbox" bind:checked={appLlm.apply_to_accounts} /> Also use for Extract
    </label>
    <style>
      select {
        width: 100%; padding: 6px 8px; background: var(--surface-panel); color: var(--text);
      }
      input[type="text"], input[type="password"] {
        width: 100%; padding: 6px 8px; background: var(--surface-panel); color: var(--text);
      }
      .checkbox input { margin: 0; }
    </style>`;

  it('flags a bare input that no stylesheet selector actually matches', () => {
    // The bare <input> only. `select` is covered by a bare element rule, the
    // password field literally declares the type its selector requires, and the
    // checkbox is UA-painted.
    expect(uncoveredFormControls(PRE_FIX)).toEqual([':3  <input no class, no type>']);
  });

  it('passes once that input carries a class the stylesheet backgrounds', () => {
    // The actual fix applied on worktree-ask-jodd: class="field".
    const fixed = PRE_FIX
      .replace('<input placeholder="claude"', '<input class="field" placeholder="claude"')
      .replace('<style>', '<style>\n      .field { background: var(--surface-panel); color: var(--text); }');
    expect(uncoveredFormControls(fixed)).toEqual([]);
  });

  it('accepts an app-wide :global control background as coverage', () => {
    const bare = '<input />';
    expect(uncoveredFormControls(bare)).toEqual([':1  <input no class, no type>']);
    expect(
      uncoveredFormControls(bare, ':global(input, textarea) { background: var(--surface-panel); }'),
    ).toEqual([]);
  });

  it('does not accept a :focus-only background as covering the resting field', () => {
    // A background that only exists on focus leaves the field UA-white until
    // the user clicks it — the same invisible-text bug, one interaction later.
    const focusOnly = `
      <input class="field" type="text" />
      <style>.field:focus { background: var(--surface-panel); }</style>`;
    expect(uncoveredFormControls(focusOnly)).toEqual([':2  <input class="field">']);
  });

  // The mirror of the rule above, and the half that shipped the second wave of
  // dark-mode bugs: `body` never set a colour, so everything that did not set
  // its own inherited the user agent's BLACK. Light mode hid it by coincidence.
  // App.svelte now sets `color` on body AND `color: inherit` on form controls
  // (which do not inherit by default). This asserts both stay.
  it('sets a themed colour on body and makes form controls inherit it', () => {
    const app = sourceFiles().find((f) => f.path === 'src/App.svelte')!.text;
    const body = /:global\(body\)\s*\{([^}]*)\}/.exec(app);
    expect(body, ':global(body) rule not found in App.svelte').toBeTruthy();
    expect(body![1], ':global(body) must set a themed colour').toMatch(/color:\s*var\(--/);

    const controls = /:global\(\s*button\s*,\s*input\s*,\s*select\s*,\s*textarea\s*\)\s*\{([^}]*)\}/.exec(app);
    expect(controls, 'form controls must be told to inherit colour').toBeTruthy();
    expect(controls![1]).toMatch(/color:\s*(inherit|var\(--)/);
  });

});

describe('truncation tooltips', () => {
  // The connections-graph modal (NoteEditor.svelte) renders node labels
  // through gtrunc(), which hard-clips to 16 chars -- there is no CSS
  // ellipsis to inspect, and `title=` as an SVG attribute is inert (it must
  // be a child <title> element for the browser's native tooltip to fire).
  // This is the one truncation site in this batch that's precisely
  // guardable: every `<g class="g-node ...">` block must carry a <title>
  // child immediately, or hovering a truncated Thai/long label shows
  // nothing. See docs/superpowers/sdd/2026-07-29-tier2-identity/tooltips-report.md
  // for why the ~19 CSS text-overflow:ellipsis sites elsewhere are NOT
  // guarded here: telling a dynamic value (note title, folder name) apart
  // from a static English label (e.g. "-> Links to") needs markup context
  // a regex over raw source can't reliably see, and a guard that flags
  // static labels as missing tooltips would just teach people to ignore it.
  it('gives every graph node group a <title> child for its hover tooltip', () => {
    const hits: string[] = [];
    for (const { path, text } of sourceFiles()) {
      if (!text.includes('g-node')) continue;
      for (const m of text.matchAll(/<g class="g-node[^>]*>[\s\S]*?<\/g>/g)) {
        if (!/<title>/.test(m[0])) {
          hits.push(`${path}  ${m[0].slice(0, 80).replace(/\n/g, ' ')}`);
        }
      }
    }
    expect(hits).toEqual([]);
  });
});

describe('graph node interaction', () => {
  // Every node in the connections graph is actionable — notes open, folders
  // navigate, tags filter. Previously only note nodes were, which read as a
  // dead click on two thirds of the graph.
  const editor = () => sourceFiles().find((f) => f.path === 'src/lib/components/NoteEditor.svelte')!.text;

  it('makes every graph node clickable, not just the note ones', () => {
    // Capture to the <title> child, NOT with [^>]* — the attribute list holds
    // an arrow function, and its `=>` would end a [^>] match early.
    const g = /<g\s+class="g-node g-\{gn\.kind\}([\s\S]*?)<title>/.exec(editor());
    expect(g, 'graph node <g> not found').toBeTruthy();
    // The old form gated interactivity on a note being present.
    expect(g![1]).not.toMatch(/class:clickable=\{!!gn\.note\}/);
    expect(g![1]).toMatch(/role="button"/);
    expect(g![1]).toMatch(/tabindex="0"/);
    expect(g![1]).toMatch(/onkeydown=/);
    expect(g![1]).toMatch(/aria-label=/);
  });

  it('mirrors the sidebar mutual-exclusion rules when navigating from a node', () => {
    // Folder / tag / Smart Folder views are mutually exclusive. A folder click
    // must clear tags AND the Smart Folder AND search; a tag click must clear
    // the Smart Folder. Diverging from Sidebar.selectFolder/selectTag would
    // give the same destination two different behaviours.
    const fn = /function openGraphNode\(gn: any\) \{[\s\S]*?\n  \}/.exec(editor());
    expect(fn, 'openGraphNode not found').toBeTruthy();
    const body = fn![0];
    expect(body).toMatch(/selectedSmartFolder\.set\(null\)/);
    expect(body).toMatch(/clearSelectedTags\(\)/);
    expect(body).toMatch(/searchQuery\.set\(''\)/);
    expect(body).toMatch(/selectedFolder\.set\(/);
    expect(body).toMatch(/toggleSelectedTag\(/);
    // Tag labels carry a display '#'; the store holds bare tags.
    expect(body).toMatch(/replace\(\/\^#\/, ''\)/);
  });
});

describe('icon accents and body-background narrowing', () => {
  const icon = () => sourceFiles().find((f) => f.path === 'src/lib/components/Icon.svelte')!.text;
  const editorText = () => sourceFiles().find((f) => f.path === 'src/lib/components/NoteEditor.svelte')!.text;

  it('sets semantic icon colours centrally, not at the call sites', () => {
    // 13 call sites use trash/pin/tag. Colouring them individually means the
    // 14th drifts. The map lives in Icon.svelte so the value is stated once.
    const t = icon();
    expect(t).toMatch(/const ACCENT/);
    expect(t).toMatch(/trash:\s*'var\(--icon-danger\)'/);
    expect(t).toMatch(/pin:\s*'var\(--icon-pin\)'/);
    expect(t).toMatch(/tag:\s*'var\(--icon-tag\)'/);
    // Must actually reach the rendered SVG, not just be declared.
    expect(t).toMatch(/stroke=\{stroke\}/);
  });

  it('does not reuse --graph-tag as the tag icon colour', () => {
    // --graph-tag is picked for Lab dE separation inside the graph and
    // measures 2.12:1 as a stroke on a selected sidebar row — it fails
    // WCAG 1.4.11's 3:1 for a non-text graphic. Same hue family, different
    // job. Reusing it would silently ship a failing contrast.
    expect(icon()).not.toMatch(/tag:\s*'var\(--graph-tag\)'/);
  });

  it('neutralises only near-white note backgrounds, never every background', () => {
    // The blanket [style*='background-color'] rule also erased a highlight the
    // user deliberately applied in Apple Notes — content, by the same argument
    // that preserves `color`. Enumerated because CSS cannot do luminance
    // maths, and JS is not an option: .editor-body is the contenteditable
    // whose innerHTML is saved, so mutating its DOM would write back to
    // body_html and out to Apple Notes.
    const t = editorText();
    expect(t, 'blanket background-color override must not return')
      .not.toMatch(/\[style\*='background-color'\]/);
    for (const v of ['rgb(255, 255, 255)', 'rgb(253, 253, 252)', 'rgba(10, 10, 10, 0.0']) {
      expect(t, `near-white ${v} must still be neutralised`).toContain(`background-color: ${v}`);
    }
  });
});


/**
 * A dialog whose content outgrows the window must be able to scroll.
 *
 * The overlay is `position: fixed; inset: 0` and centres its child, so a panel
 * taller than the viewport does not push the page down and is not clipped into
 * any scrollable ancestor: it overflows at BOTH ends at once and the wheel has
 * nothing to act on. The user cannot reach the top of the form or the Save
 * button at the bottom, and nothing on screen explains why. App Settings hit
 * exactly this once it grew an OAuth section, an LLM-provider form and a
 * diagnostics block — content grows, so this is a guard, not a one-line fix.
 *
 * Deliberately checked per COMPONENT, not per element. Where the containment
 * belongs varies legitimately: AskJoddModal bounds `.modal` and scrolls the
 * turn list inside it; LinkSuggestionsModal bounds the panel and scrolls
 * `.body`; three components put `role="dialog"` on the full-bleed overlay
 * itself, which correctly has no max-height at all. Pinning a specific element
 * would flag all of those while catching nothing. What is never legitimate is
 * a dialog component with no bound and no scroller anywhere — that panel can
 * only grow until it is unusable.
 */
describe('modal scrolling', () => {
  it('gives every dialog component somewhere to scroll', () => {
    const unscrollable: string[] = [];
    for (const { path, text } of sourceFiles()) {
      if (!/role="dialog"/.test(markupOnly(text))) continue;
      const css = styleBlock(text);
      const bounded = /max-height\s*:/.test(css);
      // The `overflow` shorthand counts; `overflow-x` alone does not.
      const scrolls = /overflow(-y)?\s*:\s*(auto|scroll)/.test(css);
      if (!bounded || !scrolls) {
        const missing = [!bounded && 'max-height', !scrolls && 'overflow-y']
          .filter(Boolean)
          .join(' + ');
        unscrollable.push(`${path}  no ${missing}`);
      }
    }
    expect(unscrollable).toEqual([]);
  });
});
