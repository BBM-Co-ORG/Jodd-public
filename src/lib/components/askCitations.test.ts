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
