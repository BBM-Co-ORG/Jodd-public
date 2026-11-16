// @vitest-environment jsdom
//
// A note's AI actions are one list (controllers/noteActions.ts
// `noteAiActions`, spec 2026-10-28 §8): the note-list context menu and the
// editor's ⋯ menu offer the same items, gated the same way, and run the
// same controller functions.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import './__fixtures__/dialogStub';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get, type Writable } from 'svelte/store';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/platform')>();
  return { ...orig, isAndroid: writable(false), isMacos: writable(false), supportsIcloud: writable(true) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  const orig = await importOriginal<typeof import('../stores/viewport')>();
  return { ...orig, androidLayoutMode: writable('tablet') };
});
vi.mock('../controllers/noteActions', async (importOriginal) => {
  const orig = await importOriginal<typeof import('../controllers/noteActions')>();
  return {
    ...orig,
    linkIntoWiki: vi.fn(async () => {}),
    suggestFolder: vi.fn(async () => {}),
    reExtractLessons: vi.fn(async () => {}),
  };
});
const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteMoreMenu from './NoteMoreMenu.svelte';
import ContextMenuHost from './__fixtures__/ContextMenuHost.svelte';
import * as actions from '../controllers/noteActions';
import { noteAiActions, hasSourceBlock } from '../controllers/noteActions';
import { accounts, currentAccount, capabilitiesByAccount } from '../stores/notes';
import { hiddenFromAi, AI_HIDDEN_HINT } from '../stores/aiScope';
import { linkSuggestions, showLinkSuggestions } from '../stores/ui';
import { isAndroid as isAndroidReadonly } from '../stores/platform';
import type { Note } from '../types';

const isAndroid = isAndroidReadonly as unknown as Writable<boolean>;
const ACCOUNT_ID = 'gmail:a@b.com';
const NOTE: Note = {
  uuid: 'abcdef12-3456-7890-abcd-ef1234567890', id: 'msg-1', account_id: ACCOUNT_ID, title: 'Made with AI',
  body_html: '<h2>Point</h2><details><summary>Source (verbatim)</summary><pre>src</pre></details>',
  date: '2026-10-20T00:00:00Z', label: 'Notes/Work',
} as Note;
const NONE = { notes: false, relocate: false, folders: false, sidecars: false };

let host: HTMLElement;
let mounted: ReturnType<typeof mount>[] = [];

beforeEach(() => {
  vi.clearAllMocks();
  invoke.mockReset();
  invoke.mockImplementation(() => Promise.resolve([]));
  accounts.set([{ id: ACCOUNT_ID, email: 'a@b.com', added_at: '2026-01-01T00:00:00Z', backend_kind: 'gmail', status: 'active' } as never]);
  currentAccount.set(ACCOUNT_ID);
  capabilitiesByAccount.set({});
  hiddenFromAi.set({});
  linkSuggestions.set(null);
  isAndroid.set(false);
  host = document.createElement('div');
  document.body.appendChild(host);
});
afterEach(() => {
  for (const m of mounted) unmount(m);
  mounted = [];
  host.remove();
  document.body.innerHTML = '';
});

/** The AI items each menu shows: label, disabled, and the menu's reason line. */
async function bothMenus() {
  const ctxHost = document.createElement('div');
  host.appendChild(ctxHost);
  mounted.push(mount(ContextMenuHost, { target: ctxHost, props: { note: NOTE } }));
  const anchor = document.createElement('button');
  host.appendChild(anchor);
  const moreHost = document.createElement('div');
  host.appendChild(moreHost);
  mounted.push(mount(NoteMoreMenu, {
    target: moreHost,
    props: {
      note: NOTE, anchor, open: true, trustPending: false, showTags: false,
      onclose: () => {}, onFind: () => {}, onShowGraph: () => {}, onShortcuts: () => {}, onMarkReviewed: () => {}, onTags: () => {},
    },
  }));
  flushSync();
  await tick();
  const AI = new Set(noteAiActions({ saved: true, canWriteNotes: true, canRelocate: true, hidden: false, body: NOTE.body_html }).map((a) => a.label));
  const read = (root: ParentNode) => ({
    items: Array.from(root.querySelectorAll<HTMLButtonElement>('button.item'))
      .filter((b) => AI.has(b.querySelector('.label')?.textContent?.trim() ?? ''))
      .map((b) => `${b.querySelector('.label')!.textContent!.trim()}${b.disabled ? ' (disabled)' : ''}`),
    reasons: Array.from(root.querySelectorAll('.menu-reason')).map((p) => p.textContent),
  });
  return { context: read(ctxHost), more: read(document.querySelector('[data-testid="note-more-menu"]')!) };
}

describe('the two note menus offer the same AI actions', () => {
  it('in the same order', async () => {
    const { context, more } = await bothMenus();
    expect(context.items).toEqual(['Ask Jodd about this note', 'Find related notes', 'Suggest a folder', 'Redo key points']);
    expect(more).toEqual(context);
  });

  it('disabled the same way, with one reason line each, in a folder hidden from AI', async () => {
    hiddenFromAi.set({ [ACCOUNT_ID]: ['Notes/Work'] });
    const { context, more } = await bothMenus();
    expect(context.items.every((i) => i.endsWith('(disabled)'))).toBe(true);
    expect(context.reasons).toEqual([AI_HIDDEN_HINT]);
    expect(more).toEqual(context);
  });

  it('a read-only account keeps only Ask in both', async () => {
    capabilitiesByAccount.set({ [ACCOUNT_ID]: { has_trash: true, writes: NONE } as never });
    const { context, more } = await bothMenus();
    expect(context.items).toEqual(['Ask Jodd about this note']);
    expect(more).toEqual(context);
  });
});

describe('the editor ⋯ runs the context menu\'s own functions', () => {
  async function openMore() {
    const anchor = document.createElement('button');
    host.appendChild(anchor);
    mounted.push(mount(NoteMoreMenu, {
      target: host,
      props: {
        note: NOTE, anchor, open: true, trustPending: false, showTags: false,
        onclose: () => {}, onFind: () => {}, onShowGraph: () => {}, onShortcuts: () => {}, onMarkReviewed: () => {}, onTags: () => {},
      },
    }));
    flushSync();
    await tick();
  }
  const click = (label: string) => {
    Array.from(document.querySelectorAll<HTMLButtonElement>('[data-testid="note-more-menu"] button.item'))
      .find((b) => b.querySelector('.label')?.textContent?.trim() === label)!.click();
    flushSync();
  };

  it('Find related notes hands its proposals to the app-level review', async () => {
    await openMore();
    click('Find related notes');
    expect(actions.linkIntoWiki).toHaveBeenCalledWith(NOTE, showLinkSuggestions);
  });

  it('Suggest a folder and Redo key points', async () => {
    await openMore();
    click('Suggest a folder');
    expect(actions.suggestFolder).toHaveBeenCalledWith(NOTE);
    await openMore();
    click('Redo key points');
    expect(actions.reExtractLessons).toHaveBeenCalledWith(NOTE);
  });

  it('showLinkSuggestions opens the review with the account and its proposals', () => {
    const appends = [{ uuid: 'u2', title: 'Other', addition_text: 'see also' }];
    showLinkSuggestions(ACCOUNT_ID, appends);
    expect(get(linkSuggestions)).toEqual({ accountId: ACCOUNT_ID, appends });
  });
});

describe('noteAiActions', () => {
  const base = { saved: true, canWriteNotes: true, canRelocate: true, hidden: false, body: '<p>x</p>' };
  const ids = (o: Partial<typeof base>) => noteAiActions({ ...base, ...o }).map((a) => a.id);

  it('offers Redo key points only for a note with a Source block', () => {
    expect(ids({})).toEqual(['ask', 'related', 'suggest']);
    expect(ids({ body: NOTE.body_html })).toEqual(['ask', 'related', 'suggest', 'redo']);
    expect(hasSourceBlock(NOTE.body_html)).toBe(true);
  });

  it('offers nothing for a note not saved yet', () => {
    expect(ids({ saved: false })).toEqual([]);
  });

  it('follows each action\'s own write: Suggest a folder needs relocate, the rest notes', () => {
    expect(ids({ canRelocate: false })).toEqual(['ask', 'related']);
    expect(ids({ canWriteNotes: false })).toEqual(['ask', 'suggest']);
  });
});
