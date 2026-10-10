import { describe, it, expect } from 'vitest';
import { cliPrivacyNotes, FILE_ACCESS_NOTE, ARGV_NOTE } from './cliPrivacy';

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
