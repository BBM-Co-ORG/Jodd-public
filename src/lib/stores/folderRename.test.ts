import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import type { Note } from '../types';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve([])) }));

import { notes, selectedNote, notesRewriteOnFolderRename, restoreOpenNoteFolder } from './notes';

const ACCT = 'gmail:a@b.com';
const OLD = 'Notes/Influencers/ลุงจืด';
const NEW = 'Notes/Influencers/ลุงจืด (พี่จืด)';

const note = (uuid: string, label: string, account_id = ACCT): Note =>
  ({ uuid, id: `m-${uuid}`, account_id, title: 'T', body_html: '<p>x</p>', date: '', label }) as Note;

// Measured live 2026-10-08: renaming the folder of the open note rewrote
// `notes` but not `selectedNote`, so the editor's next autosave sent the old
// path and filed the note back into a folder of the old name.
describe('notesRewriteOnFolderRename', () => {
  beforeEach(() => {
    notes.set([note('in', OLD), note('child', `${OLD}/sub`), note('sibling', `${OLD}x`), note('other', OLD, 'gmail:z@b.com')]);
    selectedNote.set(note('in', OLD));
  });

  it('moves the open note with its folder', () => {
    notesRewriteOnFolderRename(ACCT, OLD, NEW);
    expect(get(selectedNote)?.label).toBe(NEW);
  });

  it('moves the open note when its folder is under the renamed one', () => {
    selectedNote.set(note('child', `${OLD}/sub`));
    notesRewriteOnFolderRename(ACCT, OLD, NEW);
    expect(get(selectedNote)?.label).toBe(`${NEW}/sub`);
  });

  it('rewrites the folder and its subtree in the list, nothing else', () => {
    notesRewriteOnFolderRename(ACCT, OLD, NEW);
    expect(get(notes).map((n) => n.label)).toEqual([NEW, `${NEW}/sub`, `${OLD}x`, OLD]);
  });

  it('leaves an open note of another account alone', () => {
    selectedNote.set(note('other', OLD, 'gmail:z@b.com'));
    notesRewriteOnFolderRename(ACCT, OLD, NEW);
    expect(get(selectedNote)?.label).toBe(OLD);
  });
});

// A refused rename puts the open note's folder back — and only its folder:
// the user may have typed into it while the rename was in flight.
describe('restoreOpenNoteFolder', () => {
  it('puts the folder back and keeps edits made meanwhile', () => {
    const before = note('in', OLD);
    selectedNote.set(before);
    notesRewriteOnFolderRename(ACCT, OLD, NEW);
    selectedNote.update((n) => (n ? { ...n, body_html: '<p>typed</p>' } : n));
    restoreOpenNoteFolder(before);
    expect(get(selectedNote)?.label).toBe(OLD);
    expect(get(selectedNote)?.body_html).toBe('<p>typed</p>');
  });

  it('leaves a note opened meanwhile alone', () => {
    const before = note('in', OLD);
    selectedNote.set(note('elsewhere', 'Notes/Work'));
    restoreOpenNoteFolder(before);
    expect(get(selectedNote)?.label).toBe('Notes/Work');
  });
});
