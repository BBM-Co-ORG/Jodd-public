import { describe, it, expect } from 'vitest';
import { aiHiddenFor, aiSourceFor } from './aiScope';

describe('aiHiddenFor', () => {
  const hidden = { 'gmail:a@x.com': ['Notes/Private'] };
  it('hides the folder and its subfolders, not prefix siblings', () => {
    expect(aiHiddenFor(hidden, 'gmail:a@x.com', 'Notes/Private')).toBe(true);
    expect(aiHiddenFor(hidden, 'gmail:a@x.com', 'Notes/Private/Bank')).toBe(true);
    expect(aiHiddenFor(hidden, 'gmail:a@x.com', 'Notes/PrivateX')).toBe(false);
    expect(aiHiddenFor(hidden, 'gmail:b@x.com', 'Notes/Private')).toBe(false);
  });
  it('is false without an account or label', () => {
    expect(aiHiddenFor(hidden, null, 'Notes/Private')).toBe(false);
    expect(aiHiddenFor(hidden, 'gmail:a@x.com', undefined)).toBe(false);
  });
});

describe('aiSourceFor', () => {
  it('names the picked note in existing mode, otherwise pasted', () => {
    expect(aiSourceFor('existing', { uuid: 'u1' })).toEqual({ kind: 'note', uuid: 'u1' });
    expect(aiSourceFor('existing', null)).toEqual({ kind: 'pasted' });
    expect(aiSourceFor('paste', { uuid: 'u1' })).toEqual({ kind: 'pasted' });
  });
});
