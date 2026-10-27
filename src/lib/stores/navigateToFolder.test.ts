import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve([])) }));

import { navigateToFolder, selectedFolder, selectedSmartFolder, selectedTags } from './notes';

// NoteList shows a Smart Folder first, then a tag filter, and only then the
// folder. Opening a folder has to leave both, or the list never changes.
describe('navigateToFolder', () => {
  beforeEach(() => {
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set());
  });

  it('leaves a Smart Folder', () => {
    selectedSmartFolder.set({ account: 'gmail:a', kind: 'unreviewed' });
    navigateToFolder('Notes/Inbox');
    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes/Inbox');
  });

  it('leaves a tag filter', () => {
    selectedTags.set(new Set(['trading', 'ideas']));
    navigateToFolder('Notes/Inbox');
    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes/Inbox');
  });
});
