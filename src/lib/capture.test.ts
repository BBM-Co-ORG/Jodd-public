import { describe, it, expect, vi } from 'vitest';
import { get } from 'svelte/store';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(() => Promise.resolve([])) }));

import { writableAccounts, defaultAccount, prefillText, sourceLabel, AI_MODES, BOOKMARKLET, openCommand, showSavedCapture, type PendingCapture } from './capture';
import { currentAccount, selectedFolder, selectedSmartFolder, selectedTags } from './stores/notes';
import type { Account } from './types';

const acct = (id: string): Account => ({ id, email: id, added_at: '' });

function capture(payload: Partial<PendingCapture['payload']>): PendingCapture {
  return {
    id: 'c1',
    payload: { url: null, text: null, title: null, ...payload },
    links: [],
    default_title: 'T',
    received_at_ms: 0,
  };
}

describe('writableAccounts', () => {
  it('keeps accounts that can write notes, and those whose capabilities have not loaded', () => {
    const list = [acct('gmail:a'), acct('microsoft:b'), acct('icloud:c')];
    const caps = {
      'gmail:a': { has_trash: true, writes: { notes: true, relocate: true, folders: true, sidecars: true } },
      'microsoft:b': { has_trash: false, writes: { notes: false, relocate: false, folders: false, sidecars: false } },
    };
    expect(writableAccounts(list, caps).map((a) => a.id)).toEqual(['gmail:a', 'icloud:c']);
  });
});

describe('defaultAccount', () => {
  it('prefers the current account when it can take the note', () => {
    const w = [acct('a'), acct('b')];
    expect(defaultAccount(w, 'b')).toBe('b');
    expect(defaultAccount(w, 'zzz')).toBe('a');
    expect(defaultAccount(w, null)).toBe('a');
    expect(defaultAccount([], 'a')).toBeNull();
  });
});

describe('prefillText', () => {
  it('puts the link above the text only when the text does not already carry it', () => {
    expect(prefillText(capture({ url: 'https://a.example/', text: 'note' }))).toBe('https://a.example/\n\nnote');
    expect(prefillText(capture({ url: 'https://a.example/', text: 'see https://a.example/' }))).toBe('see https://a.example/');
    expect(prefillText(capture({ url: 'https://a.example/' }))).toBe('https://a.example/');
    expect(prefillText(capture({ text: 'just text' }))).toBe('just text');
  });
});

describe('sourceLabel', () => {
  it('names the first link\'s site, else says it is text', () => {
    expect(sourceLabel({ ...capture({ url: 'https://www.youtube.com/watch?v=x' }), links: ['https://www.youtube.com/watch?v=x'] })).toBe('from youtube.com');
    expect(sourceLabel(capture({ text: 'hi' }))).toBe('text');
  });
});

describe('AI_MODES', () => {
  it('is the Extract modal\'s own list', () => {
    expect(AI_MODES.map((m) => m.value)).toEqual(['extract', 'summarize', 'action_items', 'expand_bullets', 'transcript']);
  });
});

describe('share snippets', () => {
  it('the bookmarklet opens a jodd://capture link with url, title and selection', () => {
    expect(BOOKMARKLET.startsWith('javascript:')).toBe(true);
    for (const key of ['jodd://capture?url=', '&title=', '&text=', 'encodeURIComponent']) {
      expect(BOOKMARKLET).toContain(key);
    }
  });

  it('the launcher command uses the platform opener', () => {
    expect(openCommand(false)).toMatch(/^open "jodd:\/\/capture\?/);
    expect(openCommand(true)).toMatch(/^start "" "jodd:\/\/capture\?/);
  });

  it('the launcher command carries a placeholder that says to replace it', () => {
    for (const cmd of [openCommand(false), openCommand(true)]) {
      const text = new URL(cmd.match(/"(jodd:[^"]+)"/)![1]).searchParams.get('text');
      expect(text).toBe('Replace this text with yours (URL-encoded)');
    }
  });
});

describe('showSavedCapture', () => {
  // A capture saved while a Smart Folder is open must leave it for the folder
  // the note was filed in — NoteList shows a Smart Folder first, so setting
  // only selectedFolder left the list on the Smart Folder (PR #140 review).
  it('leaves a Smart Folder for the folder the note was filed in', async () => {
    currentAccount.set('gmail:a');
    selectedFolder.set('Notes');
    selectedSmartFolder.set({ account: 'gmail:a', kind: 'unreviewed' });

    await showSavedCapture('gmail:a', { uuid: 'u1', label: 'Notes/Inbox' });

    expect(get(selectedSmartFolder)).toBeNull();
    expect(get(selectedFolder)).toBe('Notes/Inbox');
  });

  it('leaves a tag filter for the folder the note was filed in', async () => {
    currentAccount.set('gmail:a');
    selectedFolder.set('Notes');
    selectedSmartFolder.set(null);
    selectedTags.set(new Set(['trading']));

    await showSavedCapture('gmail:a', { uuid: 'u1', label: 'Notes/Inbox' });

    expect(get(selectedTags).size).toBe(0);
    expect(get(selectedFolder)).toBe('Notes/Inbox');
  });
});
