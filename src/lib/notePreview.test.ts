import { describe, it, expect } from 'vitest';
import { notePreview, PREVIEW_MAX_CHARS } from './notePreview';

describe('notePreview', () => {
  it('decodes &nbsp; and collapses it into ordinary whitespace', () => {
    // The reported defect (2026-08-14, Microsoft/Exchange account): a note whose
    // body carries Apple's routine &nbsp; rendered the entity literally in the
    // list preview while the title line looked fine.
    expect(notePreview('<div>2be-throwaway in&nbsp; JODD-PROBE-B</div>')).toBe(
      '2be-throwaway in JODD-PROBE-B',
    );
  });

  it('decodes &amp; without re-decoding its payload', () => {
    // Single pass, deliberately: `&amp;nbsp;` is the author writing the literal
    // text "&nbsp;", not a hard space. An iterative decode would eat it.
    expect(notePreview('<p>Tom &amp; Jerry</p>')).toBe('Tom & Jerry');
    expect(notePreview('<p>literal &amp;nbsp; entity</p>')).toBe('literal &nbsp; entity');
  });

  it('decodes the rest of the core named set', () => {
    expect(notePreview('a &lt;b&gt; c &quot;d&quot; e &apos;f&apos;')).toBe(`a <b> c "d" e 'f'`);
  });

  it('decodes numeric character references, decimal and hex', () => {
    expect(notePreview('it&#39;s &#8212; fine')).toBe("it's — fine");
    expect(notePreview('emoji &#x1F600; and &#X2014; upper-X')).toBe('emoji 😀 and — upper-X');
  });

  it('strips tags BEFORE decoding, so encoded markup stays inert text', () => {
    // If decoding ran first, `&lt;script&gt;` would become `<script>` and the
    // tag stripper would then delete it along with the text after it.
    expect(notePreview('<div>see &lt;script&gt;alert(1)&lt;/script&gt; here</div>')).toBe(
      'see <script>alert(1)</script> here',
    );
  });

  it('keeps a word boundary where a block tag was', () => {
    // Tags collapse to a space, not to nothing, so adjacent blocks do not
    // fuse into one word in the preview.
    expect(notePreview('<div>first</div><div>second</div>')).toBe('first second');
  });

  it('leaves unknown named entities untouched rather than dropping them', () => {
    expect(notePreview('<p>&notarealentity; stays</p>')).toBe('&notarealentity; stays');
  });

  it('survives out-of-range and malformed numeric references', () => {
    // String.fromCodePoint throws above 0x10FFFF; the note list renders every
    // note, so one malformed body must not take the whole pane down.
    expect(notePreview('bad &#99999999; and &#xFFFFFFFF; refs')).toBe(
      'bad &#99999999; and &#xFFFFFFFF; refs',
    );
    // Lone surrogates are equally invalid as text.
    expect(notePreview('surrogate &#xD800; ref')).toBe('surrogate &#xD800; ref');
  });

  it('truncates after decoding, so entities do not consume the budget', () => {
    // 40 &nbsp; entities are 240 source chars but only 40 characters of text.
    const body = `<div>${'&nbsp;x'.repeat(40)}</div>`;
    const out = notePreview(body);
    expect(out.length).toBeLessThanOrEqual(PREVIEW_MAX_CHARS);
    expect(out).not.toContain('&nbsp;');
  });

  it('handles empty and tag-only bodies without throwing', () => {
    expect(notePreview('')).toBe('');
    expect(notePreview('<div></div>')).toBe('');
    expect(notePreview('   &nbsp;&nbsp;   ')).toBe('');
  });
});
