import { describe, it, expect } from 'vitest';
import { cliPrivacyNotes, splitNote, remoteFileAccessNote, FILE_ACCESS_NOTE, FILE_ACCESS_TAIL, ARGV_NOTE } from './cliPrivacy';

describe('cliPrivacyNotes', () => {
  it('says nothing for a tool-less stdin preset', () => {
    expect(cliPrivacyNotes('disabled', false)).toEqual([]);
  });
  it('warns for file access that is enabled or unknown', () => {
    expect(cliPrivacyNotes('enabled', false)).toEqual([FILE_ACCESS_NOTE]);
    expect(cliPrivacyNotes('unknown', false)).toEqual([FILE_ACCESS_NOTE]);
  });
  it('warns for argv delivery, independently', () => {
    expect(cliPrivacyNotes('disabled', true)).toEqual([ARGV_NOTE]);
    expect(cliPrivacyNotes('unknown', true)).toEqual([FILE_ACCESS_NOTE, ARGV_NOTE]);
  });
});

describe('cliPrivacyNotes — remote CLI', () => {
  it('names the server instead of "your computer" for a remote CLI', () => {
    const notes = cliPrivacyNotes('enabled', false, 'vps.example');
    expect(notes).toHaveLength(1);
    expect(notes[0]).toContain('on vps.example');
    expect(notes[0]).not.toContain('your computer');
  });
  it('never shows the argv note for a remote CLI', () => {
    expect(cliPrivacyNotes('disabled', true, 'vps.example')).toEqual([]);
  });
});

describe('splitNote', () => {
  it('keeps the first sentence visible and the rest verbatim', () => {
    const { lead, more } = splitNote(FILE_ACCESS_NOTE);
    expect(lead.startsWith('This provider can read files on your computer by itself.')).toBe(true);
    expect(`This provider can read files on your computer by itself. ${more}`).toBe(FILE_ACCESS_NOTE);
    const remote = splitNote(remoteFileAccessNote('box'));
    expect(remote.lead.startsWith('This provider can read files on box by itself.')).toBe(true);
    expect(`This provider can read files on box by itself. ${remote.more}`).toBe(remoteFileAccessNote('box'));
  });
  // Spec 2026-10-20 §4.4: a warning that changes what the user does stays
  // visible. That hidden folders do not stop the tool is the actionable half.
  it('keeps "Hidden folders do not stop it." visible on the file-access warning', () => {
    expect(FILE_ACCESS_TAIL).toBe('Hidden folders do not stop it.');
    expect(splitNote(FILE_ACCESS_NOTE).lead).toBe(
      'This provider can read files on your computer by itself. Hidden folders do not stop it.',
    );
    expect(splitNote(remoteFileAccessNote('vps.example:2222')).lead).toBe(
      'This provider can read files on vps.example:2222 by itself. Hidden folders do not stop it.',
    );
    // Still two lines at most (§4.4 caps a visible warning there).
    expect(splitNote(FILE_ACCESS_NOTE).lead.length).toBeLessThanOrEqual(120);
  });
  it('does not split a note that already fits two lines', () => {
    expect(splitNote(ARGV_NOTE)).toEqual({ lead: ARGV_NOTE, more: '' });
  });
});
