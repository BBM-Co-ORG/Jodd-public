// Agent folders (spec 2026-10-28 §6): one rule for the Sidebar's "Agents"
// group and for which folders are protected from rename/move/delete.
import { describe, it, expect } from 'vitest';
import { agentFolderRoot, isAgentFolder, isAgentLeaf, isProtectedAgentFolder, stripWorkflowMarkers } from './folderTree';

const KINDS: Record<string, string> = {
  'Notes/__Agent__': 'system_workflow',
  'Notes/__Claude__': 'system_workflow',
  'Notes/__Extracts__': 'system_workflow', // retired
  'Notes/Lessons': 'system_workflow', // legacy plain name, never demoted
  'Notes/__Agent__/__Scratch__': 'system_workflow', // nested agent folder
};

describe('agent folders', () => {
  it.each([
    // [path, root, in the group, protected]
    ['Notes', null, false, false],
    ['Notes/Work', null, false, false],
    ['Notes/__Agent__', 'Notes/__Agent__', true, true],
    ['Notes/__Agent__/Projects', 'Notes/__Agent__', true, false],
    ['Notes/__Agent__/Projects/EV', 'Notes/__Agent__', true, false],
    ['Notes/__Agent__/__Scratch__', 'Notes/__Agent__/__Scratch__', true, true],
    ['Notes/__Claude__', 'Notes/__Claude__', true, true],
    ['Notes/__Extracts__', null, false, false],
    ['Notes/__Extracts__/old', null, false, false],
    ['Notes/Lessons', null, false, false],
  ] as const)('%s', (path, root, inGroup, isProtected) => {
    expect(agentFolderRoot(KINDS, path)).toBe(root);
    expect(isAgentFolder(KINDS, path)).toBe(inGroup);
    expect(isProtectedAgentFolder(KINDS, path)).toBe(isProtected);
  });

  it('needs the kind as well as the leaf: a __name__ path the kinds map has not stamped yet is ordinary', () => {
    expect(isAgentFolder({}, 'Notes/__Claude__')).toBe(false);
  });

  it('isAgentLeaf reads the leaf pattern alone, retired names excluded', () => {
    expect(isAgentLeaf('__Claude__')).toBe(true);
    expect(isAgentLeaf('__Extracts__')).toBe(false);
    expect(isAgentLeaf('Claude')).toBe(false);
    expect(isAgentLeaf('____')).toBe(false);
  });

  it('stripWorkflowMarkers keeps a name without markers', () => {
    expect(stripWorkflowMarkers('__Agent__')).toBe('Agent');
    expect(stripWorkflowMarkers('Work')).toBe('Work');
    expect(stripWorkflowMarkers('____')).toBe('____');
  });
});
