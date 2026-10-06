import { describe, expect, it } from 'vitest';
import { hideableFolders, inSubtree, isHidden, withHidden } from './agentWorkspace';

const WS = 'Notes/__Agent__';

describe('agent workspace helpers', () => {
  it('matches subtrees on the slash boundary, like Rust folder_scope', () => {
    expect(inSubtree('Notes/Work/X', 'Notes/Work')).toBe(true);
    expect(inSubtree('Notes/Work', 'Notes/Work')).toBe(true);
    expect(inSubtree('Notes/WorkX', 'Notes/Work')).toBe(false);
  });

  it('never offers the root, the workspace or its subfolders for hiding', () => {
    const all = ['Notes', 'Notes/__Agent__', 'Notes/__Agent__/Projects', 'Notes/Personal', 'Notes/BBMedia', 'Notes/__AgentX'];
    expect(hideableFolders(all, WS)).toEqual(['Notes/__AgentX', 'Notes/BBMedia', 'Notes/Personal'].sort((a, b) => a.localeCompare(b)));
  });

  it('a child of a hidden folder reads as hidden', () => {
    const hidden = { 'gmail:a': ['Notes/Personal'] };
    expect(isHidden(hidden, 'gmail:a', 'Notes/Personal/Diary')).toBe(true);
    expect(isHidden(hidden, 'gmail:a', 'Notes/PersonalX')).toBe(false);
    expect(isHidden(hidden, 'gmail:b', 'Notes/Personal')).toBe(false);
  });

  it('toggling returns a new map and never duplicates an entry', () => {
    const before = { 'gmail:a': ['Notes/A'] };
    const after = withHidden(before, 'gmail:a', 'Notes/A', true);
    expect(after['gmail:a']).toEqual(['Notes/A']);
    const added = withHidden(before, 'gmail:a', 'Notes/B', true);
    expect(added['gmail:a']).toEqual(['Notes/A', 'Notes/B']);
    expect(before['gmail:a']).toEqual(['Notes/A']);
    expect(withHidden(added, 'gmail:a', 'Notes/A', false)['gmail:a']).toEqual(['Notes/B']);
  });
});
