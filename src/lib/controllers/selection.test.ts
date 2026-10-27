// @vitest-environment jsdom
//
// Spec 2026-10-20 §5.3, "after a selection": on desktop and tablet a pick
// opens the first note in list order, in the same synchronous step as the
// view change (gotcha #28); a listing not in memory yet is waited for through
// the local load only, and a refresh never moves the selection (gotcha #6);
// deleting the open note opens its neighbour. The phone opens nothing on a
// pick and pops to the list on a delete.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, writable, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/platform')>()),
  isAndroid: writable(false),
}));
vi.mock('../stores/viewport', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../stores/viewport')>()),
  androidLayoutMode: writable('phone'),
}));
const invoke = vi.fn(async (_cmd: string, _args?: unknown): Promise<unknown> => []);
vi.mock('@tauri-apps/api/core', () => ({ invoke: (c: string, a?: unknown) => invoke(c, a) }));

import { isAndroid as isAndroidReadonly } from '../stores/platform';
import {
  accounts, currentAccount, notes, pendingDeletedNoteKeys, searchQuery, selectedFolder, selectedNote,
  selectedSmartFolder, selectedTags, smartFolderNotes, tagMatchMode, noteTagsByAccount, setAccountNoteTags,
  selectedUuids, viewSnapshot, tagScope, crossAccountTagNotes,
} from '../stores/notes';
import { activePane, handlePopState, initPhoneNavHistory, navigateToPane, pushOverlay, popOverlay, closeOverlayInto } from '../stores/phoneNav';
import { selectFolder, toggleTag, selectSmartFolder } from './viewSelection';
import {
  firstOpenableNote, neighbourAfterDelete, neighbourAfterDeleting, listingLoaded, openFirstNoteFor,
  autoOpenPending, isEmptyDraft, openFirstNoteAtStartup,
} from './selection';
import { deleteNoteOptimistic } from './noteMutations';
import { noteKey } from '../noteIdentity';
import type { Note } from '../types';
import NoteList from '../components/NoteList.svelte';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const ACCT = 'gmail:a@b.com';

function note(uuid: string, label: string, date: string, extra: Partial<Note> = {}): Note {
  return {
    uuid, id: `id-${uuid}`, account_id: ACCT, title: uuid.toUpperCase(), label, date,
    body_html: `<html><head></head><body><div>${uuid}</div></body></html>`, x_mail_created_date: null,
    ...extra,
  } as Note;
}

// Projects, as NoteList shows it: the pinned OLDER note first, then newest first.
const NEWEST = note('newest', 'Notes/Projects', '2026-10-19T00:00:00Z');
const PINNED_OLD = note('pinned', 'Notes/Projects', '2026-01-01T00:00:00Z', { pinned: true });
const MIDDLE = note('middle', 'Notes/Projects', '2026-06-01T00:00:00Z');
const OPEN = note('open', 'Notes/Work', '2026-10-01T00:00:00Z');

beforeEach(() => {
  isAndroid.set(false);
  accounts.set([{ id: ACCT, email: 'a@b.com', backend_kind: 'gmail', status: 'active', added_at: '' } as never]);
  currentAccount.set(ACCT);
  searchQuery.set('');
  selectedSmartFolder.set(null);
  selectedTags.set(new Set());
  tagMatchMode.set('AND');
  tagScope.set('account');
  crossAccountTagNotes.set({ key: '', notes: [] });
  smartFolderNotes.set([]);
  noteTagsByAccount.set(new Map());
  pendingDeletedNoteKeys.set(new Set());
  selectedUuids.set(new Set());
  selectedFolder.set('Notes/Work');
  notes.set([OPEN, NEWEST, MIDDLE, PINNED_OLD]);
  selectedNote.set(OPEN);
  invoke.mockReset();
  invoke.mockImplementation(async () => []);
});

afterEach(() => {
  // Settle any pending pick so it cannot leak into the next test.
  openFirstNoteFor('not-a-view');
});

describe('a pick on desktop opens the first note in list order', () => {
  it('opens the first note in list order: the pinned note, not the newest', () => {
    selectFolder(ACCT, 'Notes/Projects');
    expect(get(selectedFolder)).toBe('Notes/Projects');
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });

  it('agrees with the first row NoteList renders', async () => {
    const host = document.createElement('div');
    document.body.appendChild(host);
    const list = mount(NoteList, { target: host });
    try {
      selectFolder(ACCT, 'Notes/Projects');
      flushSync();
      await tick();
      const firstRow = host.querySelector('.note-item .note-title, .note-item');
      expect(firstRow?.textContent).toContain('PINNED');
      expect(host.querySelector('.note-item.active')?.textContent).toContain('PINNED');
      expect(get(selectedNote)?.uuid).toBe('pinned');
    } finally {
      unmount(list);
      host.remove();
    }
  });

  it('skips a pending-deleted first note', () => {
    pendingDeletedNoteKeys.set(new Set([noteKey(PINNED_OLD)]));
    selectFolder(ACCT, 'Notes/Projects');
    expect(get(selectedNote)?.uuid).toBe('newest');
  });

  it('skips an empty tmp: draft', () => {
    const blank = note('tmp:abc', 'Notes/Projects', '2026-10-20T00:00:00Z', {
      title: 'New Note', body_html: '<html><head></head><body></body></html>', pinned: true,
    });
    notes.update((ns) => [blank, ...ns]);
    selectFolder(ACCT, 'Notes/Projects');
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });

  it('a tmp: draft someone typed into is a real note', () => {
    const typed = note('tmp:abc', 'Notes/Projects', '2026-10-20T00:00:00Z', { title: 'Shopping', pinned: true });
    expect(isEmptyDraft(typed)).toBe(false);
    expect(isEmptyDraft({ ...typed, title: '', body_html: '' })).toBe(true);
    expect(firstOpenableNote([typed, NEWEST], new Set())?.uuid).toBe('tmp:abc');
  });

  it('empty folder → null', () => {
    selectFolder(ACCT, 'Notes/Empty');
    expect(get(selectedNote)).toBeNull();
    expect(get(autoOpenPending)).toBe(true); // waits for the folder's local paint
    listingLoaded('folder', viewSnapshot());
    expect(get(selectedNote)).toBeNull();
    expect(get(autoOpenPending)).toBe(false);
  });

  it('folder and note set in the same tick (no null flash)', () => {
    const seen: (string | null)[] = [];
    const stop = selectedNote.subscribe((n) => seen.push(n ? n.uuid : null));
    const folders: string[] = [];
    const stopF = selectedFolder.subscribe((f) => folders.push(f));
    try {
      selectFolder(ACCT, 'Notes/Projects');
    } finally {
      stop();
      stopF();
    }
    expect(seen).toEqual(['open', 'pinned']);
    expect(folders).toEqual(['Notes/Work', 'Notes/Projects']);
  });

  it('opens the first note of a tag view', () => {
    setAccountNoteTags(ACCT, [{ uuid: 'middle', tag: 'study' }, { uuid: 'newest', tag: 'study' }]);
    toggleTag(ACCT, 'study');
    expect(get(selectedNote)?.uuid).toBe('newest');
  });

  it('a tag pick under an active search opens nothing (search typing never auto-opens)', () => {
    searchQuery.set('milk');
    setAccountNoteTags(ACCT, [{ uuid: 'middle', tag: 'study' }]);
    toggleTag(ACCT, 'study');
    expect(get(selectedNote)).toBeNull();
    expect(get(autoOpenPending)).toBe(false);
  });

  it('Recently Deleted opens nothing', () => {
    selectFolder(ACCT, '__TRASH__');
    expect(get(selectedNote)).toBeNull();
    expect(get(autoOpenPending)).toBe(false);
  });
});

describe('a listing not loaded yet', () => {
  it('waits for the local load, then opens its first note', () => {
    selectedSmartFolder.set({ account: ACCT, kind: 'stale' });
    smartFolderNotes.set([MIDDLE]); // the Stale folder's rows
    selectSmartFolder(ACCT, 'orphaned');
    // Not Stale's rows under Orphaned's name: nothing opens until it loads.
    expect(get(smartFolderNotes)).toEqual([]);
    expect(get(selectedNote)).toBeNull();
    const view = viewSnapshot();
    smartFolderNotes.set([NEWEST, PINNED_OLD]);
    listingLoaded('smart', view);
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });

  it('view changed before load finished → no selection', () => {
    selectSmartFolder(ACCT, 'orphaned');
    const view = viewSnapshot();
    // The user moves on (a change that is not a pick) while the read runs.
    selectedSmartFolder.set({ account: ACCT, kind: 'stale' });
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', view);
    expect(get(selectedNote)).toBeNull();
  });

  it('leaving the view and coming back does not revive the pick', () => {
    selectSmartFolder(ACCT, 'orphaned');
    const view = viewSnapshot();
    selectedSmartFolder.set(null); // away (not a pick)…
    selectedSmartFolder.set({ account: ACCT, kind: 'orphaned' }); // …and back
    expect(get(autoOpenPending)).toBe(false);
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', view);
    expect(get(selectedNote)).toBeNull();
  });

  it('a search typed while the pick waits cancels it', () => {
    selectSmartFolder(ACCT, 'orphaned');
    const view = viewSnapshot();
    searchQuery.set('milk');
    searchQuery.set('');
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', view);
    expect(get(selectedNote)).toBeNull();
  });

  it('a note the user opened meanwhile wins', () => {
    selectSmartFolder(ACCT, 'orphaned');
    const view = viewSnapshot();
    selectedNote.set(OPEN);
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', view);
    expect(get(selectedNote)?.uuid).toBe('open');
  });

  it('a load begun for another view does not answer the pick', () => {
    selectSmartFolder(ACCT, 'orphaned');
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', JSON.stringify(['other view']));
    expect(get(selectedNote)).toBeNull();
    listingLoaded('folder', viewSnapshot());
    expect(get(selectedNote)).toBeNull();
  });

  it('remote-changed reorder does not change selection', () => {
    // A pick that waited, then its load…
    selectSmartFolder(ACCT, 'orphaned');
    const view = viewSnapshot();
    smartFolderNotes.set([MIDDLE, NEWEST]);
    listingLoaded('smart', view);
    expect(get(selectedNote)?.uuid).toBe('newest');
    // …then a refresh reorders the list and reports again: the open note stays.
    smartFolderNotes.set([MIDDLE, { ...NEWEST, date: '2020-01-01T00:00:00Z' }]);
    listingLoaded('smart', view);
    expect(get(selectedNote)?.uuid).toBe('newest');
  });

  it('a refresh that reorders a loaded folder does not change selection', () => {
    selectFolder(ACCT, 'Notes/Projects');
    expect(get(selectedNote)?.uuid).toBe('pinned');
    notes.update((ns) => ns.map((n) => (n.uuid === 'pinned' ? { ...n, pinned: false } : n)));
    listingLoaded('folder', viewSnapshot());
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });
});

describe('delete selects next, else previous, else null', () => {
  const list = [PINNED_OLD, NEWEST, MIDDLE];

  it('next, else previous, else null', () => {
    expect(neighbourAfterDelete(list, noteKey(NEWEST))?.uuid).toBe('middle');
    expect(neighbourAfterDelete(list, noteKey(MIDDLE))?.uuid).toBe('newest');
    expect(neighbourAfterDelete([MIDDLE], noteKey(MIDDLE))).toBeNull();
    expect(neighbourAfterDelete(list, 'not-listed')).toBeNull();
  });

  it('a batch opens the first note left below the deleted block, else the nearest above', () => {
    const five = ['a', 'b', 'c', 'd', 'e'].map((u, i) => note(u, 'Notes/X', `2026-01-0${9 - i}T00:00:00Z`));
    const keys = (...us: string[]) => new Set(us.map((u) => noteKey(five.find((n) => n.uuid === u)!)));
    expect(neighbourAfterDeleting(five, keys('b', 'c'))?.uuid).toBe('d');
    expect(neighbourAfterDeleting(five, keys('b', 'd'))?.uuid).toBe('e');
    expect(neighbourAfterDeleting(five, keys('d', 'e'))?.uuid).toBe('c');
    expect(neighbourAfterDeleting(five, keys('a', 'b', 'c', 'd', 'e'))).toBeNull();
  });

  it('deleting the open note opens the next one, in the same step', async () => {
    selectFolder(ACCT, 'Notes/Projects');
    selectedNote.set(get(notes).find((n) => n.uuid === 'newest')!);
    const seen: (string | null)[] = [];
    const stop = selectedNote.subscribe((n) => seen.push(n ? n.uuid : null));
    await deleteNoteOptimistic(NEWEST, ACCT);
    stop();
    expect(seen).toEqual(['newest', 'middle']);
    expect(invoke).toHaveBeenCalledWith('delete_note', expect.objectContaining({ uuid: 'newest' }));
  });

  it('the last note opens the previous one; the only note leaves the empty state', async () => {
    selectFolder(ACCT, 'Notes/Projects');
    selectedNote.set(get(notes).find((n) => n.uuid === 'middle')!);
    await deleteNoteOptimistic(MIDDLE, ACCT);
    expect(get(selectedNote)?.uuid).toBe('newest');
    selectFolder(ACCT, 'Notes/Work');
    expect(get(selectedNote)?.uuid).toBe('open');
    await deleteNoteOptimistic(OPEN, ACCT);
    expect(get(selectedNote)).toBeNull();
  });

  it('a refused delete reopens the note, not its neighbour', async () => {
    selectFolder(ACCT, 'Notes/Projects');
    selectedNote.set(get(notes).find((n) => n.uuid === 'newest')!);
    invoke.mockImplementation(async (cmd: string) => {
      if (cmd === 'delete_note') throw new Error('refused');
      return [];
    });
    await deleteNoteOptimistic(NEWEST, ACCT);
    expect(get(selectedNote)?.uuid).toBe('newest');
  });

  it('deleting a note that is not open leaves the open note alone', async () => {
    selectFolder(ACCT, 'Notes/Projects');
    await deleteNoteOptimistic(NEWEST, ACCT);
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });
});

// Galaxy Tab S7 live pass of v0.35.2 (2026-10-21): a cold start showed
// "Select a note" beside a full list. The starting view counts as a pick.
describe('cold start opens the starting view\'s first note', () => {
  beforeEach(() => {
    selectedNote.set(null);
    selectedFolder.set('Notes/Projects');
  });

  it('desktop and tablet: the first note in list order', () => {
    openFirstNoteAtStartup();
    expect(get(selectedNote)?.uuid).toBe('pinned');
  });

  it('a note opened before the cache paint landed stays open', () => {
    selectedNote.set(OPEN);
    openFirstNoteAtStartup();
    expect(get(selectedNote)?.uuid).toBe('open');
  });

  it('a search typed meanwhile opens nothing', () => {
    searchQuery.set('x');
    openFirstNoteAtStartup();
    expect(get(selectedNote)).toBeNull();
  });

  it('an empty starting folder leaves the empty state', () => {
    selectedFolder.set('Notes/Empty');
    openFirstNoteAtStartup();
    expect(get(selectedNote)).toBeNull();
  });

  it('phone: nothing opens — the stack starts at the list', () => {
    isAndroid.set(true);
    history.replaceState(null, '', location.href);
    initPhoneNavHistory('Projects');
    openFirstNoteAtStartup();
    expect(get(selectedNote)).toBeNull();
  });
});

describe('phone: picks open nothing, a delete pops to the list', () => {
  beforeEach(() => {
    isAndroid.set(true);
    history.replaceState(null, '', location.href);
    initPhoneNavHistory('Work');
  });

  it('a pick clears the note and opens nothing', () => {
    selectFolder(ACCT, 'Notes/Projects');
    expect(get(selectedNote)).toBeNull();
    expect(get(autoOpenPending)).toBe(false);
    selectSmartFolder(ACCT, 'orphaned');
    smartFolderNotes.set([NEWEST]);
    listingLoaded('smart', viewSnapshot());
    expect(get(selectedNote)).toBeNull();
  });

  it('deleting the open note pops the note pane to the list', async () => {
    navigateToPane('note', 'OPEN');
    expect(get(activePane)).toBe('note');
    const back = vi.spyOn(history, 'back').mockImplementation(() => {
      history.replaceState({ pane: 'list', depth: 1 }, '', location.href);
      handlePopState(new PopStateEvent('popstate', { state: history.state }));
    });
    let backs = 0;
    try {
      await deleteNoteOptimistic(OPEN, ACCT);
      backs = back.mock.calls.length;
    } finally {
      back.mockRestore();
    }
    expect(backs).toBe(1);
    expect(get(activePane)).toBe('list');
    expect(get(selectedNote)).toBeNull();
  });

  it('under an open modal, the pop waits for the modal to leave first', async () => {
    navigateToPane('note', 'OPEN');
    pushOverlay('confirm');
    const back = vi.spyOn(history, 'back').mockImplementation(() => {
      const d = (history.state?.depth ?? 1) - 1;
      history.replaceState(d === 1 ? { pane: 'list', depth: 1 } : { pane: 'note', depth: 2 }, '', location.href);
      handlePopState(new PopStateEvent('popstate', { state: history.state }));
    });
    let backs = 0;
    try {
      await deleteNoteOptimistic(OPEN, ACCT);
      // The modal's entry is on top: nothing popped yet.
      expect(back.mock.calls.length).toBe(0);
      expect(get(activePane)).toBe('note');
      popOverlay('confirm'); // the dialog closes: its back, then the pane's
      backs = back.mock.calls.length;
    } finally {
      back.mockRestore();
    }
    expect(backs).toBe(2);
    expect(get(activePane)).toBe('list');
  });

  // A small model of the browser's stack, so Back (ours or the user's) lands
  // on the entry that really is beneath: jsdom's own traversal is async.
  function modelHistory() {
    // Called on the note pane the beforeEach seeded: folders → list → note.
    const stack: unknown[] = [{ pane: 'folders', depth: 0 }, { pane: 'list', depth: 1 }, history.state];
    const realPush = history.pushState.bind(history);
    const realReplace = history.replaceState.bind(history);
    const push = vi.spyOn(history, 'pushState').mockImplementation((st, t, u) => { stack.push(st); realPush(st, t, u); });
    const replace = vi.spyOn(history, 'replaceState').mockImplementation((st, t, u) => { stack[stack.length - 1] = st; realReplace(st, t, u); });
    const land = () => {
      stack.pop();
      realReplace(stack[stack.length - 1], '', location.href);
      handlePopState(new PopStateEvent('popstate', { state: history.state }));
    };
    const back = vi.spyOn(history, 'back').mockImplementation(land);
    return { userBack: land, back, restore: () => { push.mockRestore(); replace.mockRestore(); back.mockRestore(); } };
  }

  it('a waiting pop the user overtook with Back never pops a note opened afterwards', async () => {
    navigateToPane('note', 'OPEN');
    const h = modelHistory();
    try {
      pushOverlay('confirm');
      await deleteNoteOptimistic(OPEN, ACCT); // waits under the modal
      h.userBack(); // the user's own Back takes the modal's entry away
      h.userBack(); // …and the note's: the list
      expect(get(activePane)).toBe('list');
      navigateToPane('note', 'NEWEST'); // a different note, at the same depth
      const before = h.back.mock.calls.length;
      pushOverlay('other'); // an unrelated modal opens and closes itself
      popOverlay('other');
      expect(h.back.mock.calls.length - before).toBe(1); // its own entry only
      expect(get(activePane)).toBe('note');
      expect(history.state).toMatchObject({ pane: 'note', depth: 2 });
    } finally {
      h.restore();
    }
  });

  it('a modal closed into a destination forgets the waiting pop', async () => {
    navigateToPane('note', 'OPEN');
    const h = modelHistory();
    try {
      pushOverlay('confirm');
      await deleteNoteOptimistic(OPEN, ACCT);
      closeOverlayInto('confirm', 'list', 'Work'); // replaces the modal's entry
      navigateToPane('note', 'NEWEST');
      const before = h.back.mock.calls.length;
      pushOverlay('other');
      popOverlay('other');
      expect(h.back.mock.calls.length - before).toBe(1);
      expect(get(activePane)).toBe('note');
    } finally {
      h.restore();
    }
  });

  it('the waiting pop still runs when the modal closes itself', async () => {
    navigateToPane('note', 'OPEN');
    const h = modelHistory();
    try {
      pushOverlay('confirm');
      await deleteNoteOptimistic(OPEN, ACCT);
      popOverlay('confirm');
      expect(h.back).toHaveBeenCalledTimes(2);
      expect(get(activePane)).toBe('list');
      expect(history.state).toMatchObject({ pane: 'list', depth: 1 });
    } finally {
      h.restore();
    }
  });
});
