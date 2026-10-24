// @vitest-environment jsdom
import { readFileSync } from 'node:fs';
import { describe, it, expect } from 'vitest';
import { renderAnswer, type CitedNote } from './askCitations';

const cited: CitedNote[] = [
  { uuid: 'aabbccdd-0000-0000-0000-000000000000', account_id: 'a@x', title: 'Sync conflicts', slug: 'sync-conflicts-aabbccdd' },
];

describe('renderAnswer', () => {
  it('turns a known citation into a clickable chip', () => {
    const html = renderAnswer('We chose keep-both. [[sync-conflicts-aabbccdd]]', cited);
    expect(html).toContain('data-uuid="aabbccdd-0000-0000-0000-000000000000"');
    expect(html).toContain('data-account="a@x"');
    expect(html).toContain('Sync conflicts');
    expect(html).not.toContain('[[');
  });

  it('leaves an unknown citation as plain text rather than a dead chip', () => {
    const html = renderAnswer('See [[not-a-real-note-ffffffff]]', cited);
    expect(html).not.toContain('data-uuid=');
    expect(html).toContain('not-a-real-note-ffffffff');
  });

  it('escapes HTML in the answer so note content cannot inject markup', () => {
    const html = renderAnswer('<img src=x onerror=alert(1)>', cited);
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img');
  });

  it('escapes HTML in a note title used as chip text', () => {
    const evil: CitedNote[] = [
      { uuid: 'u', account_id: 'a', title: '<img src=x onerror=alert(1)>', slug: 's-aabbccdd' },
    ];
    const html = renderAnswer('[[s-aabbccdd]]', evil);
    // `not.toContain('<script>')` alone would pass against a completely
    // unescaped implementation for any title that doesn't literally say
    // "<script>" — assert on the actual hostile markup (no real `<img`
    // element can form) and on the escaped form actually being present, so
    // a regression that drops escaping (but still doesn't emit the literal
    // string "<script>") is caught.
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;img src=x onerror=alert(1)&gt;');
  });

  it('escapes a hostile uuid so it cannot break out of the data-uuid attribute', () => {
    // Removing escapeHtml from data-uuid (or from data-account) leaves every
    // other test in this file passing — this is the case that actually
    // exercises attribute-breakout, which is the highest-value gap a chip
    // renderer like this can have.
    const evil: CitedNote[] = [
      {
        uuid: 'x" onfocus=alert(1) autofocus="',
        account_id: 'a@x',
        title: 'Sync conflicts',
        slug: 'sync-conflicts-aabbccdd',
      },
    ];
    const html = renderAnswer('[[sync-conflicts-aabbccdd]]', evil);
    // If escapeHtml were dropped for data-uuid, the raw uuid's `"` would
    // close the attribute early and `onfocus=` would become a real,
    // executable attribute on the <button> — this is the exact string that
    // would appear if that happened. It must never appear un-escaped.
    expect(html).not.toContain('data-uuid="x" onfocus=alert(1) autofocus="');
    // The quote that would have closed the attribute early must come out
    // as &quot;, keeping the whole hostile string inside ONE data-uuid
    // attribute value instead of spilling into new attributes.
    expect(html).toContain('data-uuid="x&quot; onfocus=alert(1) autofocus=&quot;"');
  });

  it('renders the same citation twice without duplicating the chip data', () => {
    const html = renderAnswer('[[sync-conflicts-aabbccdd]] and [[sync-conflicts-aabbccdd]]', cited);
    expect(html.match(/data-uuid=/g)?.length).toBe(2);
  });

  it('handles an answer with no citations at all', () => {
    expect(renderAnswer('plain answer', cited)).toContain('plain answer');
  });
});

// The safety property, stated structurally rather than by string search: parse
// the output the way {@html} will, and require that every element is from the
// fixed tag set and that the only attributes anywhere are the chip's own (and
// an integer `start` on <ol>). A string assertion like `not.toContain('<img')`
// passes for markup the parser would still build, e.g. `<IMG` or `<svg/onload`.
const ALLOWED_TAGS = new Set(['P', 'STRONG', 'EM', 'CODE', 'PRE', 'UL', 'OL', 'LI', 'BR', 'HR', 'BUTTON']);
function assertOnlyAllowedMarkup(html: string) {
  const host = document.createElement('div');
  host.innerHTML = html;
  for (const el of host.querySelectorAll('*')) {
    expect(ALLOWED_TAGS.has(el.tagName), `unexpected <${el.tagName}> in ${html}`).toBe(true);
    for (const attr of el.attributes) {
      if (el.tagName === 'BUTTON') {
        expect(['class', 'data-uuid', 'data-account']).toContain(attr.name);
        if (attr.name === 'class') expect(attr.value).toBe('cite-chip');
      } else if (el.tagName === 'OL' && attr.name === 'start') {
        expect(attr.value).toMatch(/^\d+$/);
      } else {
        throw new Error(`unexpected attribute ${attr.name} on <${el.tagName}> in ${html}`);
      }
    }
  }
  return host;
}

describe('renderAnswer markdown', () => {
  it('renders bold and an unordered list', () => {
    const host = assertOnlyAllowedMarkup(
      renderAnswer('Summary:\n\n- **AI tools and workflows:** two notes\n- plain item', cited),
    );
    const items = host.querySelectorAll('ul > li');
    expect(items.length).toBe(2);
    expect(items[0].querySelector('strong')?.textContent).toBe('AI tools and workflows:');
    expect(host.querySelector('p')?.textContent).toBe('Summary:');
    expect(host.textContent).not.toContain('**');
  });

  it('renders an ordered list of Thai items, keeping the text intact', () => {
    const md = '1. **เครื่องมือ AI และ Claude** — บันทึกสองฉบับ\n2. การประชุม *สัปดาห์นี้*';
    const host = assertOnlyAllowedMarkup(renderAnswer(md, cited));
    const items = host.querySelectorAll('ol > li');
    expect(items.length).toBe(2);
    expect(items[0].querySelector('strong')?.textContent).toBe('เครื่องมือ AI และ Claude');
    expect(items[0].textContent).toBe('เครื่องมือ AI และ Claude — บันทึกสองฉบับ');
    expect(items[1].querySelector('em')?.textContent).toBe('สัปดาห์นี้');
  });

  it('keeps one list across blank lines between items (a loose list)', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('1. first\n\n2. second\n\n3. third', cited));
    expect(host.querySelectorAll('ol').length).toBe(1);
    expect(host.querySelectorAll('ol > li').length).toBe(3);
  });

  it('keeps the numbering when a paragraph interrupts an ordered list', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('1. first\n\nAn aside.\n\n2. second', cited));
    const lists = host.querySelectorAll('ol');
    expect(lists.length).toBe(2);
    expect(lists[0].getAttribute('start')).toBeNull();
    expect(lists[1].getAttribute('start')).toBe('2');
  });

  it('nests an indented list inside the item above it', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('1. Tools\n   - Claude\n   - Codex\n2. People', cited));
    expect(host.querySelectorAll('ol > li').length).toBe(2);
    expect(host.querySelectorAll('ol > li > ul > li').length).toBe(2);
  });

  it('renders inline code without interpreting markdown inside it', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('Run `cargo test --workspace` and `**not bold**`', cited));
    const codes = host.querySelectorAll('code');
    expect(codes[0].textContent).toBe('cargo test --workspace');
    expect(codes[1].textContent).toBe('**not bold**');
    expect(host.querySelector('strong')).toBeNull();
  });

  it('renders a fenced code block verbatim', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('Like this:\n```sh\nnpm run *build*\n<b>x</b>\n```', cited));
    expect(host.querySelector('pre > code')?.textContent).toBe('npm run *build*\n<b>x</b>');
    expect(host.querySelector('em')).toBeNull();
  });

  it('turns single newlines inside a paragraph into line breaks', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('line one\nline two', cited));
    expect(host.querySelectorAll('p').length).toBe(1);
    expect(host.querySelectorAll('br').length).toBe(1);
  });

  it('renders a heading as a bold paragraph rather than leaving the hashes', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('## Findings\nSome text', cited));
    expect(host.querySelector('p > strong')?.textContent).toBe('Findings');
    expect(host.textContent).not.toContain('#');
  });

  it('does not treat underscores inside words as emphasis', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('call push_one_dirty and _this_ one', cited));
    expect(host.textContent).toContain('push_one_dirty');
    expect(host.querySelectorAll('em').length).toBe(1);
    expect(host.querySelector('em')?.textContent).toBe('this');
  });

  it('renders bold italic and nested emphasis without misnesting', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('***both*** and **a *b* c**', cited));
    expect(host.querySelector('strong > em')?.textContent).toBe('both');
    expect(host.querySelectorAll('strong')[1].querySelector('em')?.textContent).toBe('b');
    expect(host.textContent).toBe('both and a b c');
  });

  it('honours backslash escapes', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('a \\*literal\\* star', cited));
    expect(host.querySelector('em')).toBeNull();
    expect(host.textContent).toBe('a *literal* star');
  });
});

describe('renderAnswer citations inside markdown', () => {
  it('makes a citation inside bold text a chip', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('**Keep both [[sync-conflicts-aabbccdd]]**', cited));
    const chip = host.querySelector('strong > button.cite-chip') as HTMLElement | null;
    expect(chip?.dataset.uuid).toBe('aabbccdd-0000-0000-0000-000000000000');
    expect(chip?.textContent).toBe('Sync conflicts');
  });

  it('makes a citation inside a list item a chip', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('- We chose keep-both [[sync-conflicts-aabbccdd]]', cited));
    expect((host.querySelector('ul > li > button.cite-chip') as HTMLElement | null)?.dataset.account).toBe('a@x');
  });

  it('does not let emphasis characters in a title or account id reach the markup', () => {
    const odd: CitedNote[] = [
      { uuid: 'u_1_', account_id: 'a_b_c@x', title: 'my_*odd*_title', slug: 'odd-aabbccdd' },
    ];
    const host = assertOnlyAllowedMarkup(renderAnswer('_see [[odd-aabbccdd]] here_', odd));
    const chip = host.querySelector('button.cite-chip') as HTMLElement;
    expect(chip.textContent).toBe('my_*odd*_title');
    expect(chip.dataset.uuid).toBe('u_1_');
    expect(chip.dataset.account).toBe('a_b_c@x');
  });

  it('leaves an unknown citation inside a list as plain text', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('- see [[not-a-real-note-ffffffff]]', cited));
    expect(host.querySelector('button')).toBeNull();
    expect(host.querySelector('li')?.textContent).toBe('see not-a-real-note-ffffffff');
  });

  it('leaves a citation inside inline code as literal text', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('`[[sync-conflicts-aabbccdd]]`', cited));
    expect(host.querySelector('button')).toBeNull();
    expect(host.querySelector('code')?.textContent).toBe('[[sync-conflicts-aabbccdd]]');
  });

  it('cannot be tricked by a NUL placeholder lookalike in the answer', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('x \u00000\u0000 [[sync-conflicts-aabbccdd]]', cited));
    expect(host.querySelectorAll('button.cite-chip').length).toBe(1);
  });
});

describe('renderAnswer hostile input never produces live markup', () => {
  const hostile = [
    '<script>alert(1)</script>',
    '<img src=x onerror=alert(1)>',
    '<IMG SRC=x OnError=alert(1)>',
    '<svg/onload=alert(1)>',
    '<div style="position:fixed;inset:0">\nraw HTML block\n</div>',
    '- <b onclick=alert(1)>item</b>',
    '**<img src=x onerror=alert(1)>**',
    '[x](javascript:alert(1))',
    '![tracker](https://evil.example/beacon.png)',
    '<https://evil.example/autolink>',
    'https://evil.example/bare',
    '<a href="javascript:alert(1)">click</a>',
    '`<script>alert(1)</script>`',
    '```\n<script>alert(1)</script>\n```',
    '&lt;script&gt; already-escaped &amp; text',
    '1. [[sync-conflicts-aabbccdd]]" onmouseover="alert(1)',
  ];
  for (const md of hostile) {
    it(`renders ${JSON.stringify(md)} as text`, () => {
      const host = assertOnlyAllowedMarkup(renderAnswer(md, cited));
      expect(host.querySelector('a, img, script, svg, div, b, iframe, style')).toBeNull();
    });
  }

  it('shows a markdown link as its literal text rather than a link', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('[x](javascript:alert(1))', cited));
    expect(host.textContent).toBe('[x](javascript:alert(1))');
  });

  it('round-trips already-escaped entities as visible text, not as markup', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('&lt;b&gt;', cited));
    expect(host.textContent).toBe('&lt;b&gt;');
  });
});

describe('renderAnswer edge cases found in review', () => {
  it('keeps backslashes inside inline code', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('Open `\\\\server\\share` or `\\d+\\.\\d`', cited));
    const codes = host.querySelectorAll('code');
    expect(codes[0].textContent).toBe('\\\\server\\share');
    expect(codes[1].textContent).toBe('\\d+\\.\\d');
  });

  it('never leaks a placeholder when code sits inside double brackets', () => {
    const html = renderAnswer('[[see `x`]]', cited);
    expect(html).not.toContain('\0');
    const host = assertOnlyAllowedMarkup(html);
    expect(host.querySelector('code')?.textContent).toBe('x');
    expect(host.querySelector('button')).toBeNull();
  });

  it('treats ```code``` on one line as inline code, not a fence that swallows the rest', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('```npm ci``` first\n\n**then** build', cited));
    expect(host.querySelector('pre')).toBeNull();
    expect(host.querySelector('code')?.textContent).toBe('npm ci');
    expect(host.querySelector('strong')?.textContent).toBe('then');
  });

  it('strips the list indent from a fenced block inside an item and keeps its tabs', () => {
    const host = assertOnlyAllowedMarkup(renderAnswer('1. Run:\n   ```\n   make\n   \tindented\n   ```\n2. Done', cited));
    expect(host.querySelector('ol > li > pre > code')?.textContent).toBe('make\n\tindented');
    expect(host.querySelectorAll('ol > li').length).toBe(2);
  });

  it('renders list and heading lines that contain U+2028 quickly and correctly', () => {
    const t0 = performance.now();
    const host = assertOnlyAllowedMarkup(
      renderAnswer('# ' + ' '.repeat(3000) + 'x\u2028y\n- ' + ' '.repeat(20000) + 'a\u2028b', cited),
    );
    expect(performance.now() - t0).toBeLessThan(500);
    expect(host.querySelector('p > strong')?.textContent).toBe('x\u2028y');
    expect(host.querySelector('ul > li')?.textContent).toBe('a\u2028b');
  });

  it('stays fast on long runs of delimiter characters', () => {
    for (const md of ['`'.repeat(5000), '*'.repeat(50000), '_'.repeat(50000), '[['.repeat(25000), '- '.repeat(20000)]) {
      const t0 = performance.now();
      assertOnlyAllowedMarkup(renderAnswer(md, cited));
      expect(performance.now() - t0, md.slice(0, 4)).toBeLessThan(500);
    }
  });

  it('uses no regex lookbehind, which older macOS WebKit cannot parse', () => {
    // A lookbehind is a SyntaxError on Safari < 16.4, and this module is in
    // the main bundle — the failure would not stay inside Ask Jodd.
    const src = readFileSync('src/lib/components/askCitations.ts', 'utf8');
    expect(src).not.toMatch(/\(\?<[!=]/);
  });
});
