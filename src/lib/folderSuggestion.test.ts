// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import { folderSuggestions, folderSuggestionKey, clearFolderSuggestion } from './stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { recordFolderSuggestion, explicitOutcomeMessage, explicitErrorMessage } from './folderSuggestion';

describe('folder suggestion outcomes', () => {
  beforeEach(() => folderSuggestions.set({}));

  it('records only a suggestion, keyed by account and uuid', () => {
    recordFolderSuggestion('gmail:a@b.com', 'u1', { kind: 'none_fits' });
    expect(get(folderSuggestions)).toEqual({});
    recordFolderSuggestion('gmail:a@b.com', 'u1', { kind: 'suggested', uuid: 'u1', path: 'Notes/Trading', reason: 'r' });
    expect(get(folderSuggestions)[folderSuggestionKey('gmail:a@b.com', 'u1')]).toEqual({ path: 'Notes/Trading', reason: 'r' });
  });

  // gotcha #16: a backend (Microsoft) can rekey a note's uuid between the
  // moment the frontend asked for a suggestion and the moment the answer
  // comes back. The outcome carries the note's CURRENT uuid, which a
  // refreshed row holds; the open editor may still hold the uuid asked
  // about, because nothing moves `selectedNote` on a rekey. Both must find it.
  it('stores a suggestion under both the outcome\'s uuid and the uuid asked about, after a rekey', () => {
    recordFolderSuggestion('gmail:a@b.com', 'old-uuid', {
      kind: 'suggested',
      uuid: 'new-uuid',
      path: 'Notes/Trading',
      reason: 'r',
    });
    const m = get(folderSuggestions);
    expect(m[folderSuggestionKey('gmail:a@b.com', 'new-uuid')]).toEqual({ path: 'Notes/Trading', reason: 'r' });
    expect(m[folderSuggestionKey('gmail:a@b.com', 'old-uuid')]).toEqual({ path: 'Notes/Trading', reason: 'r' });
  });

  it('clearing a proposal through either uuid removes its twin', () => {
    for (const via of ['old-uuid', 'new-uuid']) {
      folderSuggestions.set({});
      recordFolderSuggestion('gmail:a@b.com', 'old-uuid', { kind: 'suggested', uuid: 'new-uuid', path: 'Notes/T', reason: null });
      clearFolderSuggestion('gmail:a@b.com', via);
      expect(get(folderSuggestions), `cleared via ${via}`).toEqual({});
    }
  });

  it('clearing one note never removes another note\'s equal-looking proposal', () => {
    recordFolderSuggestion('gmail:a@b.com', 'u1', { kind: 'suggested', uuid: 'u1', path: 'Notes/T', reason: null });
    recordFolderSuggestion('gmail:a@b.com', 'u2', { kind: 'suggested', uuid: 'u2', path: 'Notes/T', reason: null });
    recordFolderSuggestion('other:x@y.com', 'u1', { kind: 'suggested', uuid: 'u1', path: 'Notes/T', reason: null });
    clearFolderSuggestion('gmail:a@b.com', 'u1');
    const m = get(folderSuggestions);
    expect(Object.keys(m).sort()).toEqual([
      folderSuggestionKey('gmail:a@b.com', 'u2'),
      folderSuggestionKey('other:x@y.com', 'u1'),
    ].sort());
  });

  it('words each explicit outcome the way the spec table does', () => {
    expect(explicitOutcomeMessage({ kind: 'no_candidates' }, true)).toBe('No folders to choose from yet');
    expect(explicitOutcomeMessage({ kind: 'none_fits' }, true)).toBe('No better folder found');
    expect(explicitOutcomeMessage({ kind: 'suggested', uuid: 'u1', path: 'Notes/T', reason: null }, true)).toBeNull();
    expect(
      explicitOutcomeMessage({ kind: 'suggested', uuid: 'u1', path: 'Notes/T', reason: null }, false),
    ).toContain('Notes/T');
  });

  it('names a missing provider, and stays quiet on a cancel', () => {
    expect(explicitErrorMessage('provider not configured: no LLM provider configured for this account'))
      .toBe('No LLM provider is configured for this account');
    expect(explicitErrorMessage('provider not configured: AI data access is disabled. Review Account Settings.'))
      .toBe('AI data access is disabled. Review Account Settings.');
    expect(explicitErrorMessage('cancelled')).toBeNull();
    expect(explicitErrorMessage('transport error: timeout')).toBe('Suggest folder failed: transport error: timeout');
  });
});
