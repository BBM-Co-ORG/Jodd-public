// @vitest-environment jsdom
import { describe, it, expect } from 'vitest';
import { extractHashtags, normalizeTag, tagsInEditor, tagsEditable, tagModelOf, suggestedTagsBecomeKeywords, keywordsHintShown } from './bodyTags';

function editor(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  return el;
}

const span = (ref: string, text: string) =>
  `<span data-jodd-inline="hashtag" data-ref="${ref}" contenteditable="false">${text}</span>`;

describe('normalizeTag (mirror of db::normalize_tag)', () => {
  it('trims, lowercases, drops whitespace and every #, keeps Thai and emoji', () => {
    expect(normalizeTag('  #Work ')).toBe('work');
    expect(normalizeTag('#LAB😀')).toBe('lab😀');
    expect(normalizeTag('#แท็ก ทดลอง')).toBe('แท็กทดลอง');
    expect(normalizeTag('#')).toBe('');
  });
});

describe('extractHashtags (mirror of db::extract_hashtags)', () => {
  it('reads #words at a word boundary, lowercased, de-duplicated, needing a letter', () => {
    expect(extractHashtags('a #Work item #urgent then #work, #2024, a#mid')).toEqual(['work', 'urgent']);
    expect(extractHashtags('#database-migrations and #งาน')).toEqual(['database-migrations', 'งาน']);
  });
});

// Gotcha #38: on iCloud a tag is a hashtag span, never `#word` text — which
// is exactly what Apple's Delete Tag leaves behind.
describe('tagsInEditor', () => {
  // The newline between blocks stands in for the one innerText adds in a real
  // webview; jsdom has only textContent, which would glue "lab#LabLower".
  const body = `<div>Tag lab</div>\n<div>${span('ia-1', '#LabLower')} #LabUpper</div>`;

  it('counts only hashtag spans on an inline_objects account', () => {
    expect(tagsInEditor(editor(body), 'inline_objects')).toEqual(['lablower']);
  });

  it('counts every #word on a body_text account, spans included as text', () => {
    expect(tagsInEditor(editor(body), 'body_text')).toEqual(['lablower', 'labupper']);
  });

  it('reads an unresolved model as body_text', () => {
    expect(tagsInEditor(editor(body), undefined)).toEqual(['lablower', 'labupper']);
  });

  it('treats a span as one tag whatever its characters, and adjacent spans as two', () => {
    const el = editor(`<div>${span('a', '#lab😀')}${span('b', '#123')}${span('c', '#LAB😀')}${span('d', '#')}</div>`);
    expect(tagsInEditor(el, 'inline_objects')).toEqual(['lab😀', '123']);
  });
});

describe('tagsEditable', () => {
  it('is false only where Jodd cannot make a real tag', () => {
    expect(tagsEditable('inline_objects')).toBe(false);
    expect(tagsEditable('body_text')).toBe(true);
    expect(tagsEditable(undefined)).toBe(true);
  });
});

// The model must be known on the note's FIRST render: while capabilities load,
// an iCloud note used to fall back to the text rule — showing a tag Apple had
// deleted and an add-tag field that writes `#tag` text into Apple's note.
describe('tagModelOf (mirror of db::tag_model_for_account)', () => {
  it('reads the backend from the account id while capabilities are loading', () => {
    expect(tagModelOf(undefined, 'icloud:someone@me.com')).toBe('inline_objects');
    expect(tagModelOf(undefined, 'gmail:someone@gmail.com')).toBe('body_text');
    expect(tagModelOf(undefined, 'microsoft:a@live.com')).toBe('body_text');
    expect(tagModelOf(undefined, 'someone@gmail.com')).toBe('body_text'); // legacy bare id
    expect(tagModelOf(undefined, null)).toBe('body_text');
  });

  it('takes the capability once it has arrived', () => {
    expect(tagModelOf({ tag_model: 'inline_objects' }, 'gmail:x@gmail.com')).toBe('inline_objects');
    expect(tagModelOf({ tag_model: 'body_text' }, 'icloud:x@me.com')).toBe('body_text');
    expect(tagModelOf({}, 'icloud:x@me.com')).toBe('inline_objects'); // cached before tag_model existed
  });
});

describe('suggestedTagsBecomeKeywords', () => {
  it('is true only where a tag is an object Jodd cannot create', () => {
    expect(suggestedTagsBecomeKeywords('inline_objects')).toBe(true);
    expect(suggestedTagsBecomeKeywords('body_text')).toBe(false);
    // Capabilities still loading: say nothing rather than guess.
    expect(suggestedTagsBecomeKeywords(undefined)).toBe(false);
  });
});

describe('keywordsHintShown', () => {
  it('shows for every workflow that asks the LLM for tags, on iCloud only', () => {
    for (const w of ['extract', 'summarize', 'expand_bullets', 'transcript'] as const) {
      expect(keywordsHintShown('inline_objects', w, true)).toBe(true);
      expect(keywordsHintShown('body_text', w, true)).toBe(false);
      expect(keywordsHintShown(undefined, w, true)).toBe(false);
    }
  });

  it('never shows for Action items, whose envelope carries no tags', () => {
    expect(keywordsHintShown('inline_objects', 'action_items', true)).toBe(false);
  });

  it('never shows for a raw transcript ingest, which makes no LLM call', () => {
    expect(keywordsHintShown('inline_objects', 'transcript', false)).toBe(false);
    // `clean` only matters for the transcript workflow.
    expect(keywordsHintShown('inline_objects', 'extract', false)).toBe(true);
  });
});
