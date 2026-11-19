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
import { syncBlocks } from '../syncBlocks';

/** A chip state for the popover tests: a refused pin (a refused note push is
 * said by its notice, not the chip — the One Voice Rule). */
function pinBlocked(n: Note): Note {
  syncBlocks.update((m) => {
    const a = m[n.account_id!] ?? { pins: {}, folders: {} };
    return { ...m, [n.account_id!]: { ...a, pins: { ...a.pins, [n.uuid]: 'refused' } } };
  });
  return n;
}

const isAndroid = isAndroidRO as unknown as Writable<boolean>;
const androidLayoutMode = layoutRO as unknown as Writable<string>;
const backLabel = backLabelRO as unknown as Writable<string | null>;

const NOTE = {
  uuid: 'uuid-1', id: 'msg-1', account_id: 'gmail:a@b.com', title: 'T',
  body_html: '<div>T</div><div>body</div>', date: '2026-10-08T09:30:00Z', label: 'Notes/Work',
} as Note;

const STATS = '3 words · 6 characters · 2 lines';
// Title "T" (1 byte) + '<div>T</div><div>body</div>' (27): the cache's UTF-8.
const SIZE = '28 bytes on this device';

const s = (kind: MetaStatus['kind']): MetaStatus => ({ kind, label: kind, detail: `${kind} detail` });

describe('pickMetaStatus priority', () => {
  it('trust > pinBlocked, whatever the order given (a refused push is its notice, never the chip)', () => {
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
    const full = () => date().querySelector('.meta-date-full')!;
    expect(full().textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
    expect(full().textContent).not.toMatch(/BE|,/);
    expect(date().getAttribute('title')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}\n${STATS}\n${SIZE}`);
    // The sentence as hidden text (an aria-label on a plain span is ignored);
    // the visible forms are hidden from the accessibility tree.
    expect(date().querySelector('.meta-visually-hidden')!.textContent).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}. ${STATS}. ${SIZE}`);
    expect(full().getAttribute('aria-hidden')).toBe('true');
    expect(date().querySelector('.meta-date-short')!.getAttribute('aria-hidden')).toBe('true');
    calendarPref.set('buddhist');
    flushSync();
    expect(full().textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
    expect(full().textContent).toMatch(/2569/);
    expect(date().getAttribute('title')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}\n${STATS}\n${SIZE}`);
  });

  // The creation date (x_mail_created_date, kept by every backend) was stored
  // and never shown: it joins the tooltip and the hidden sentence as a second
  // line, under the calendar setting like the first.
  it('adds the creation date to the tooltip and the hidden sentence when the note has one', () => {
    const CREATED = 'Thu, 4 Jun 2026 01:19:50 +0700';
    render({ note: { ...NOTE, x_mail_created_date: CREATED } });
    const date = () => host.querySelector('.meta-date') as HTMLElement;
    const updated = `Last updated ${formatNoteDateTime(NOTE.date)}`;
    const created = `Created ${formatNoteDateTime(CREATED)}`;
    expect(date().getAttribute('title')).toBe(`${updated}\n${created}\n${STATS}\n${SIZE}`);
    expect(date().querySelector('.meta-visually-hidden')!.textContent).toBe(`${updated}. ${created}. ${STATS}. ${SIZE}`);
    calendarPref.set('buddhist');
    flushSync();
    expect(date().getAttribute('title')).toMatch(/Created .*2569/);
  });

  it('leaves the creation line out when the date is missing, empty or unreadable', () => {
    for (const x of [undefined, null, '', 'not a date']) {
      render({ note: { ...NOTE, x_mail_created_date: x } });
      const date = host.querySelector('.meta-date') as HTMLElement;
      expect(date.getAttribute('title')).toBe(`Last updated ${formatNoteDateTime(NOTE.date)}\n${STATS}\n${SIZE}`);
      unmount(cmp);
      cmp = null;
    }
  });

  // Words, characters and lines (noteLines.ts) as the tooltip's last line.
  // NOTE: title "T", body <div>T</div><div>body</div> → T, T, body.
  it('ends the tooltip with the note\'s words, characters and lines, and follows edits', () => {
    render();
    const date = () => host.querySelector('.meta-date') as HTMLElement;
    expect(date().getAttribute('title')!.split('\n').at(-2)).toBe('3 words · 6 characters · 2 lines');
    unmount(cmp);
    render({ note: { ...NOTE, title: 'ประชุม', body_html: '<div>ประชุมวางแผน</div><div><br></div><div>ok</div>' } });
    expect(date().getAttribute('title')!.split('\n').at(-2)).toBe('4 words · 18 characters · 3 lines');
  });

  // The counts are what the reader sees; the size is what the note takes on
  // this device, its attachments included once the editor has read them.
  it('ends the tooltip with the size on this device, attachments included when known', () => {
    const date = () => host.querySelector('.meta-date') as HTMLElement;
    const last = () => date().getAttribute('title')!.split('\n').at(-1);
    render();
    expect(last()).toBe('28 bytes on this device');
    unmount(cmp);
    render({ attachments: { count: 2, bytes: 3 * 1024 * 1024 } });
    expect(last()).toBe('3.0 MB on this device (note 28 bytes · 2 attachments 3.0 MB)');
    expect(date().querySelector('.meta-visually-hidden')!.textContent).toMatch(/\. 3\.0 MB on this device \(note 28 bytes · 2 attachments 3\.0 MB\)$/);
    unmount(cmp);
    render({ attachments: { count: 0, bytes: 0 } });
    expect(last()).toBe('28 bytes on this device');
  });

  // Live pass 2026-10-21 (S23 FE): a provenance chip ("👁 jodd-ingest · 15…")
  // squeezed the date to "00:5". The date sat inside the account/folder box,
  // which shrinks and clips; it must be a line item of its own that never
  // shrinks, and the chip must cap itself and ellipsize its own label.
  // Re-critique after #296: the chip's name was its short label
  // ("claude-code · 2 h ago"), which never said the change is unreviewed;
  // and opening its dialog left focus on the chip.
  it('the chip is named by its full sentence, and opening it moves focus into the dialog', async () => {
    render({ status: { kind: 'trust', label: 'claude-code · 2 h ago', detail: 'Changed by claude-code 2 h ago and not reviewed yet.', icon: 'eye', action: { label: 'Mark reviewed', run: () => {} } } });
    const chip = host.querySelector<HTMLButtonElement>('.meta-status')!;
    expect(chip.getAttribute('aria-label')).toBe('Changed by claude-code 2 h ago and not reviewed yet.');
    chip.click();
    flushSync();
    await tick();
    await tick();
    expect(document.activeElement?.textContent?.trim()).toBe('Mark reviewed');
  });

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

  // "#0" read as a tag named 0 (re-critique, step 7): at zero it says what it does.
  it('with 0 tags and tags editable, the button reads "Add tag"', () => {
    render({ tagCount: 0, tagsEditable: true });
    const btn = line().querySelector('.meta-tags')!;
    expect(btn.textContent!.trim()).toBe('Add tag');
    expect(btn.getAttribute('aria-label')).toBe('Add tags');
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
    syncBlocks.set({});
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
    expect(host.querySelector('.meta-line .meta-date-full')?.textContent?.trim()).toBe(formatNoteMetaDate(NOTE.date));
  });

  // The editor reads attachments after a note renders; the size it got must
  // show for THAT note, and never in the pass that opens another (gotcha #28).
  it('the size line takes the note\'s attachments, and never another note\'s', async () => {
    const withAtt = { ...NOTE, uuid: 'uuid-a', id: 'msg-a' } as Note;
    const plain = { ...NOTE, uuid: 'uuid-p', id: 'msg-p' } as Note;
    invoke.mockImplementation((cmd: string, args?: { uuid?: string }) => {
      if (cmd === 'note_connections') return Promise.resolve({ outgoing: [], backlinks: [] });
      if (cmd === 'get_note_attachments') {
        return Promise.resolve(args?.uuid === 'uuid-a'
          ? [{ content_id: 'c1', mime_type: 'application/pdf', data_uri: '', size: 2048 }]
          : []);
      }
      return Promise.resolve([]);
    });
    const last = () => (host.querySelector('.meta-line .meta-date') as HTMLElement).getAttribute('title')!.split('\n').at(-1);
    const settle = async () => { for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); } flushSync(); };
    notes.set([withAtt, plain]);
    selectedNote.set(withAtt);
    await settle();
    expect(last()).toBe('2.0 KB on this device (note 28 bytes · 1 attachment 2.0 KB)');
    selectedNote.set(plain);
    flushSync(); // no await: the pass that opens the note
    expect(last()).toBe('28 bytes on this device');
    await settle();
    expect(last()).toBe('28 bytes on this device');
  });

  it('one active account: no account prefix', () => {
    expect(host.querySelector('.meta-line .meta-account')).toBeNull();
  });

  it('a popover opened on one note is closed in the same pass that opens another', () => {
    const blocked = (uuid: string) => pinBlocked({ ...NOTE, uuid, id: `msg-${uuid}`, pinned: true } as Note);
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
    const blocked = (uuid: string) => pinBlocked({ ...NOTE, uuid, id: `msg-${uuid}`, pinned: true } as Note);
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
    expect(count()).toBe('Add tag');
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
