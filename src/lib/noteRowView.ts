// A note row's text, computed once per row object.
//
// NoteList re-renders every row whenever `$notes` changes, which is every
// editor keystroke, and as a legacy-mode component it treats each row object
// as changed even when it is the same one. `notePreview` strips the whole
// body's HTML, so computing it per render cost ~300 ms per keystroke on 3,000
// notes (listRowCost.test.ts). `listedNotes` keeps an unchanged note's row
// object stable, so this cache misses only for the row that really changed.
// A WeakMap keeps nothing alive: a row that leaves the list takes its entry
// with it.
import type { Note } from './types';
import { notePreview } from './notePreview';
import { displayTitle } from './textDisplay';

export interface RowView {
  /** Display title, or 'Untitled'. */
  title: string;
  preview: string;
}

const cache = new WeakMap<Note, RowView>();

export function rowView(note: Note): RowView {
  let v = cache.get(note);
  if (!v) {
    v = { title: displayTitle(note.title) || 'Untitled', preview: notePreview(note.body_html) };
    cache.set(note, v);
  }
  return v;
}
