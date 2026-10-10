import { describe, it, expect } from 'vitest';
import { folderCounts } from './folderCounts';
import type { MessageIndex, Note } from './types';

const A = 'gmail:a@b.com';
const note = (label: string): Note => ({ uuid: label, id: '', account_id: A, label } as Note);
const stub = (id: string, label: string): MessageIndex => ({ id, label });

describe('folderCounts', () => {
  it('counts an unloaded folder from the index', () => {
    const out = folderCounts([], new Map([[A, [stub('m1', 'Notes/X'), stub('m2', 'Notes/X')]]]), new Map());
    expect(out[A]).toEqual({ 'Notes/X': 2 });
  });

  it('counts a loaded folder from the notes it holds', () => {
    const out = folderCounts([note('Notes/X')], new Map([[A, [stub('m1', 'Notes/X'), stub('m2', 'Notes/X')]]]), new Map([[A, new Set(['Notes/X'])]]));
    expect(out[A]).toEqual({ 'Notes/X': 1 });
  });

  // Measured live 2026-10-08: a note saved before its first push, then saved
  // again after the worker gave it a new id, left index stubs that no delete
  // could find by id. After the note and its folder were deleted, the folder
  // stayed in the sidebar with a count of 0 — and a second delete did nothing.
  // A loaded folder's notes are the truth; its stale stubs must not keep it.
  it('drops a loaded folder that holds no notes, whatever the index says', () => {
    const stale = [stub('', 'Notes/rename-test-2'), stub('', 'Notes/rename-test-2'), stub('1a11c3d3216043d7', 'Notes/rename-test-2')];
    const out = folderCounts([], new Map([[A, stale]]), new Map([[A, new Set(['Notes/rename-test-2'])]]));
    expect(out[A] ?? {}).not.toHaveProperty('Notes/rename-test-2');
  });
});
