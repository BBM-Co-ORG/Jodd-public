// @vitest-environment jsdom
//
// The editor's one-line metadata header (spec 2026-10-20 §6.1, §6.3): account
// prefix only with more than one active account, folder path as the reveal
// button, the date from formatNoteMetaDate (full timestamp in its tooltip), at most one status indicator whose
// popover holds the full sentence and its action, and the #N tags button.
import './__fixtures__/dialogStub';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount, unmount, flushSync, tick } from 'svelte';
import { type Writable } from 'svelte/store';
import { readFileSync } from 'node:fs';

vi.mock('../stores/platform', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  return { ...(await importOriginal<typeof import('../stores/platform')>()), isAndroid: writable(false) };
});
vi.mock('../stores/viewport', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  return { ...(await importOriginal<typeof import('../stores/viewport')>()), androidLayoutMode: writable('tablet') };
});
vi.mock('../stores/phoneNav', async (importOriginal) => {
  const { writable } = await import('svelte/store');
  return { ...(await importOriginal<typeof import('../stores/phoneNav')>()), backLabel: writable<string | null>(null) };
});

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import EditorMetaLine from './EditorMetaLine.svelte';
import NoteEditor from './NoteEditor.svelte';
import { pickMetaStatus, type MetaStatus } from '../editor/metaStatus';
import { accountPrefixOf } from '../stores/listView';
import { calendarPref, formatNoteDateTime, formatNoteMetaDate } from '../format/dates';
import { isAndroid as isAndroidRO } from '../stores/platform';
import { androidLayoutMode as layoutRO } from '../stores/viewport';
import { backLabel as backLabelRO } from '../stores/phoneNav';
import { accounts, notes, selectedNote, unreviewedTrust } from '../stores/notes';
import type { Account, Note } from '../types';

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const androidLayoutMode = layoutRO as unknown as Writable<string>;
const backLabel = backLabelRO as unknown as Writable<string | null>;

const NOTE = {
  uuid: 'uuid-1', id: 'msg-1', account_id: 'gmail:a@b.com', title: 'T',
  body_html: '<div>T</div><div>body</div>', date: '2026-10-08T09:30:00Z', label: 'Notes/Work',
} as Note;

const s = (kind: MetaStatus['kind']): MetaStatus => ({ kind, label: kind, detail: `${kind} detail` });

describe('pickMetaStatus priority', () => {
  it('readonly > notSynced > trust > pinBlocked, whatever the order given', () => {
    expect(pickMetaStatus([s('pinBlocked'), s('trust'), s('notSynced'), s('readonly')])?.kind).toBe('readonly');
    expect(pickMetaStatus([s('pinBlocked'), s('trust'), s('notSynced')])?.kind).toBe('notSynced');
    expect(pickMetaStatus([s('pinBlocked'), s('trust')])?.kind).toBe('trust');
    expect(pickMetaStatus([s('pinBlocked')])?.kind).toBe('pinBlocked');
  });
  it('skips absent candidates and returns null when none apply', () => {
    expect(pickMetaStatus([null, undefined, false])).toBeNull();
    expect(pickMetaStatus([])).toBeNull();
    expect(pickMetaStatus([null, s('trust'), false])?.kind).toBe('trust');
  });
});

describe('accountPrefixOf', () => {
  const g = { id: 'gmail:a@b.com', email: 'a@b.com', backend_kind: 'gmail', status: 'active' } as Account;
  const m = { id: 'microsoft:c@d.com', email: 'c@d.com', backend_kind: 'microsoft', status: 'active' } as Account;
  const sameAddr = { ...m, id: 'microsoft:a@b.com', email: 'a@b.com' } as Account;
  it('is null with one active account', () => {
    expect(accountPrefixOf(g, [g])).toBeNull();
  });
  it('names the short name and the backend with two', () => {
    expect(accountPrefixOf(g, [g, m])).toBe('a · Gmail');
  });
  it('does not repeat the backend the short name already carries', () => {
    expect(accountPrefixOf(g, [g, sameAddr])).toBe('a@b.com · Gmail');
  });
});

describe('EditorMetaLine', () => {
  let host: HTMLElement;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let cmp: any;

  function render(props: Record<string, unknown> = {}) {
    cmp = mount(EditorMetaLine, {
      target: host,
      props: {
        note: NOTE,
        accountLabel: null,
        folderPath: NOTE.label,
        onRevealFolder: vi.fn(),
        status: null,
        tagCount: 0,
        tagsEditable: false,
        onOpenTags: vi.fn(),
        ...props,
      },
    });
    flushSync();
  }

  beforeEach(() => {
    isAndroid.set(false);
    androidLayoutMode.set('tablet');
    backLabel.set(null);
    calendarPref.set('auto');
    host = document.createElement('div');
    document.body.appendChild(host);
  });
  afterEach(() => {
    if (cmp) unmount(cmp);
    cmp = null;
    host.remove();
    calendarPref.set('auto');
  });

  const line = () => host.querySelector('.meta-line') as HTMLElement;
  const popover = () => host.querySelector('[data-testid="meta-status-popover"]') as HTMLElement | null;

  // Live pass 2026-10-21: "21 Oct 2569 BE, 00:58" became the list's relative
  // scheme; the full timestamp moved to the tooltip and the aria-label.
  it('date uses formatNoteMetaDate, with the full timestamp as its tooltip, and follows the calendar setting', () => {
    render();
    const date = () => host.querySelector('.meta-date') as HTMLElement;
    expect(date().textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
    expect(date().textContent).not.toMatch(/BE|,/);
    expect(date().getAttribute('title')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}`);
    expect(date().getAttribute('aria-label')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}`);
    calendarPref.set('buddhist');
    flushSync();
    expect(date().textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
    expect(date().textContent).toMatch(/2569/);
    expect(date().getAttribute('title')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}`);
  });

  // Live pass 2026-10-21 (S23 FE): a provenance chip ("👁 jodd-ingest · 15…")
  // squeezed the date to "00:5". The date sat inside the account/folder box,
  // which shrinks and clips; it must be a line item of its own that never
  // shrinks, and the chip must cap itself and ellipsize its own label.
  it('the date never shrinks: it is outside the shrinking account/folder box', () => {
    render({ accountLabel: 'a · Gmail', status: { kind: 'trust', label: 'jodd-ingest · 15 Oct', detail: 'd', icon: 'eye' } });
    const date = host.querySelector('.meta-date') as HTMLElement;
    expect(date.parentElement).toBe(line());
    expect(date.closest('.meta-where')).toBeNull();
    const style = readFileSync('src/lib/components/EditorMetaLine.svelte', 'utf8');
    const rule = (sel: string) => {
      const css = style.slice(style.indexOf('<style>'));
      const i = css.indexOf(`\n  ${sel} {`);
      expect(i, `${sel} rule`).toBeGreaterThanOrEqual(0);
      return css.slice(i, css.indexOf('}', i));
    };
    expect(rule('.meta-date')).toMatch(/flex:\s*none/);
    expect(rule('.meta-status')).toMatch(/max-width:\s*min\(/);
    const label = rule('.meta-status-label');
    expect(label).toMatch(/min-width:\s*0/);
    expect(label).toMatch(/text-overflow:\s*ellipsis/);
  });

  it('shows the account prefix only when given one', () => {
    render({ accountLabel: 'a · Gmail' });
    expect(line().querySelector('.meta-account')?.textContent).toContain('a · Gmail');
    unmount(cmp);
    render();
    expect(line().querySelector('.meta-account')).toBeNull();
  });

  it('the folder button keeps .ctx-folder, shows the path and reveals it', () => {
    const onRevealFolder = vi.fn();
    render({ onRevealFolder });
    const btn = line().querySelector('.ctx-folder') as HTMLButtonElement;
    expect(btn.textContent).toContain('Work');
    btn.click();
    expect(onRevealFolder).toHaveBeenCalledOnce();
  });

  // S23 FE screenshots (v0.35.1, landscape): the breadcrumb was left-
  // truncated to "…tes › Inbox". Every folder is under the root "Notes", so
  // the root says nothing: show the path below it ("Inbox · Today 00:58").
  it('the path drops the root "Notes" segment; the tooltip keeps the full path', () => {
    const text = (path: string) => {
      render({ folderPath: path });
      const btn = line().querySelector('.ctx-folder') as HTMLButtonElement;
      const out = { text: btn.textContent?.trim(), title: btn.getAttribute('title') };
      unmount(cmp!);
      cmp = null;
      return out;
    };
    expect(text('Notes/Inbox')).toEqual({ text: 'Inbox', title: 'Reveal folder in sidebar: Notes/Inbox' });
    expect(text('Notes/Projects/Jodd')).toEqual({ text: 'Projects › Jodd', title: 'Reveal folder in sidebar: Notes/Projects/Jodd' });
    // The root itself still names a place.
    expect(text('Notes').text).toBe('Notes');
    // A path not under Notes is shown whole.
    expect(text('Archive/2025').text).toBe('Archive › 2025');
  });

  it('phone: no account prefix, and no folder when it equals the Back label', () => {
    isAndroid.set(true);
    androidLayoutMode.set('phone');
    backLabel.set('Work');
    render({ accountLabel: 'a · Gmail' });
    expect(line().querySelector('.meta-account')).toBeNull();
    expect(line().querySelector('.ctx-folder')).toBeNull();
    backLabel.set('Unreviewed');
    flushSync();
    expect(line().querySelector('.ctx-folder')).not.toBeNull();
  });

  it('tablet keeps the account prefix and the folder', () => {
    isAndroid.set(true);
    androidLayoutMode.set('tablet');
    backLabel.set('Work');
    render({ accountLabel: 'a · Gmail' });
    expect(line().querySelector('.meta-account')).not.toBeNull();
    expect(line().querySelector('.ctx-folder')).not.toBeNull();
  });

  it('the status indicator opens a popover with the full sentence and its action', () => {
    const run = vi.fn();
    render({ status: { kind: 'notSynced', label: 'Not synced', detail: 'Not synced — the reason.', action: { label: 'Try again', run } } });
    const ind = line().querySelector('.meta-status') as HTMLButtonElement;
    expect(ind.textContent).toContain('Not synced');
    expect(ind.textContent).not.toContain('the reason');
    expect(popover()).toBeNull();
    ind.click();
    flushSync();
    expect(popover()?.textContent).toContain('Not synced — the reason.');
    (popover()!.querySelector('.retry-sync-btn') as HTMLButtonElement).click();
    expect(run).toHaveBeenCalledOnce();
  });

  it('the trust indicator is data-testid="trust-chip" and its action honours disabled', () => {
    const run = vi.fn();
    render({ status: { kind: 'trust', label: 'claude-code · 3 min ago', detail: 'claude-code changed this note 3 min ago.', action: { label: 'Mark reviewed', run, disabled: true }, error: 'Could not mark reviewed.' } });
    const chip = host.querySelector('[data-testid="trust-chip"]') as HTMLButtonElement;
    expect(chip.textContent).toContain('claude-code');
    chip.click();
    flushSync();
    const btn = popover()!.querySelector('.mark-reviewed-btn') as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(popover()!.textContent).toContain('Could not mark reviewed.');
  });

  it('no indicator without a status', () => {
    render();
    expect(line().querySelector('.meta-status')).toBeNull();
  });

  it('#N is hidden when there are no tags and tags are not editable', () => {
    render({ tagCount: 0, tagsEditable: false });
    expect(line().querySelector('.meta-tags')).toBeNull();
  });

  it('#N shows the count and hands its button to onOpenTags', () => {
    const onOpenTags = vi.fn();
    render({ tagCount: 3, tagsEditable: false, onOpenTags });
    const btn = line().querySelector('.meta-tags') as HTMLButtonElement;
    expect(btn.textContent).toContain('#3');
    btn.click();
    expect(onOpenTags).toHaveBeenCalledWith(btn);
  });

  it('#N shows with 0 tags when tags are editable', () => {
    render({ tagCount: 0, tagsEditable: true });
    expect(line().querySelector('.meta-tags')?.textContent).toContain('#0');
  });
});

describe('NoteEditor renders one metadata line', () => {
  let host: HTMLElement;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let app: any;

  beforeEach(async () => {
    invoke.mockReset();
    invoke.mockImplementation((cmd: string) => cmd === 'note_connections'
      ? Promise.resolve({ outgoing: [], backlinks: [] })
      : Promise.resolve([]));
    isAndroid.set(false);
    accounts.set([{ id: NOTE.account_id!, email: 'a@b.com', backend_kind: 'gmail', status: 'active' } as Account]);
    unreviewedTrust.set({});
    notes.set([NOTE]);
    selectedNote.set(NOTE);
    host = document.createElement('div');
    document.body.appendChild(host);
    app = mount(NoteEditor, { target: host });
    flushSync();
    await tick();
    flushSync();
  });
  afterEach(() => {
    unmount(app);
    host.remove();
    selectedNote.set(null);
    notes.set([]);
    accounts.set([]);
  });

  it('one line: no .note-context, no .note-date, no .ctx-slug', () => {
    expect(host.querySelectorAll('.meta-line')).toHaveLength(1);
    expect(host.querySelector('.note-context')).toBeNull();
    expect(host.querySelector('.note-date')).toBeNull();
    expect(host.querySelector('.ctx-slug')).toBeNull();
    expect(host.querySelector('.meta-line .ctx-folder')).not.toBeNull();
    expect(host.querySelector('.meta-line .meta-date')?.textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
  });

  it('one active account: no account prefix', () => {
    expect(host.querySelector('.meta-line .meta-account')).toBeNull();
  });

  it('a popover opened on one note is closed in the same pass that opens another', () => {
    const blocked = (uuid: string) => ({ ...NOTE, uuid, id: `msg-${uuid}`, push_blocked_reason: 'permanent: why', push_blocked_by_remote: true }) as Note;
    notes.set([blocked('uuid-1'), blocked('uuid-2')]);
    selectedNote.set(blocked('uuid-1'));
    flushSync();
    (host.querySelector('.meta-line .meta-status') as HTMLButtonElement).click();
    flushSync();
    expect(host.querySelector('[data-testid="meta-status-popover"]')).not.toBeNull();
    selectedNote.set(blocked('uuid-2'));
    flushSync(); // no await: the gap is what gotcha #28 is about
    expect(host.querySelector('.meta-line .meta-status')).not.toBeNull();
    expect(host.querySelector('[data-testid="meta-status-popover"]')).toBeNull();
  });

  // Fix round 1: closed by a switch means closed — coming back does not reopen it.
  it('open on A, switch to B, back to A: no popover', () => {
    const blocked = (uuid: string) => ({ ...NOTE, uuid, id: `msg-${uuid}`, push_blocked_reason: 'permanent: why', push_blocked_by_remote: true }) as Note;
    const A = blocked('uuid-1');
    const B = blocked('uuid-2');
    notes.set([A, B]);
    selectedNote.set(A);
    flushSync();
    (host.querySelector('.meta-line .meta-status') as HTMLButtonElement).click();
    flushSync();
    expect(host.querySelector('[data-testid="meta-status-popover"]')).not.toBeNull();
    selectedNote.set(B);
    flushSync();
    selectedNote.set(A);
    flushSync();
    expect(host.querySelector('.meta-line .meta-status')).not.toBeNull();
    expect(host.querySelector('[data-testid="meta-status-popover"]')).toBeNull();
  });

  // Fix round 1: bodyTags is re-read a tick after a switch; #N must not show
  // the previous note's count in the pass that opens the next one.
  it('#N never shows the previous note\'s count right after a switch', async () => {
    const tagged = { ...NOTE, uuid: 'uuid-t', id: 'msg-t', body_html: '<div>T</div><div>see #alpha #beta</div>' } as Note;
    const plain = { ...NOTE, uuid: 'uuid-p', id: 'msg-p', body_html: '<div>P</div><div>plain</div>' } as Note;
    const one = { ...NOTE, uuid: 'uuid-o', id: 'msg-o', body_html: '<div>O</div><div>see #gamma</div>' } as Note;
    notes.set([tagged, plain, one]);
    selectedNote.set(tagged);
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    flushSync();
    const count = () => host.querySelector('.meta-line .meta-tags')?.textContent?.trim() ?? null;
    expect(count()).toBe('#2');
    selectedNote.set(plain);
    flushSync(); // no await: the pass that opens the note
    expect(count()).not.toBe('#2');
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    flushSync();
    expect(count()).toBe('#0');
    selectedNote.set(tagged);
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    flushSync();
    expect(count()).toBe('#2');
    selectedNote.set(one);
    flushSync();
    expect(count()).not.toBe('#2');
    for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); }
    flushSync();
    expect(count()).toBe('#1');
  });

  it('the title input stays a separate field after the line', () => {
    const lineEl = host.querySelector('.meta-line')!;
    const title = host.querySelector('.title-input') as HTMLInputElement;
    expect(title.value).toBe('T');
    expect(lineEl.compareDocumentPosition(title) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});
