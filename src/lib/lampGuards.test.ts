// @vitest-environment jsdom
//
// The Lamp Marks Here Rule (DESIGN.md, 2026-11-09 workspace review: "lamp
// unlit, hierarchy flat"). The open folder and the open note carry a Lamp
// Mark rail over the neutral active overlay; multi-select moved off the rail
// to a checkbox plus the amber wash, so the rail means one thing.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { mount, unmount, flushSync, tick } from 'svelte';
import { get } from 'svelte/store';
import { parseTokens, parseColor, composite, contrast, type RGB } from './color';
import {
  notes, selectedNote, selectedFolder, currentAccount, accounts, noteIndex, hydratedFolders,
  selectedTags, selectedSmartFolder, searchQuery, selectedUuids, capabilitiesByAccount,
} from './stores/notes';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import NoteList from './components/NoteList.svelte';
import { noteKey } from './noteIdentity';

const CSS = readFileSync('src/styles/tokens.css', 'utf8');
const NOTELIST = readFileSync('src/lib/components/NoteList.svelte', 'utf8');
const SIDEBAR = readFileSync('src/lib/components/Sidebar.svelte', 'utf8');

/** The body of the first rule whose selector is exactly `sel`. */
function rule(src: string, sel: string): string {
  const i = src.indexOf(`\n  ${sel} {`);
  if (i < 0) throw new Error(`no rule ${sel}`);
  return src.slice(src.indexOf('{', i) + 1, src.indexOf('}', i));
}

describe('Lamp Mark contrast (WCAG 1.4.11: a state mark holds 3:1)', () => {
  const themes: [string, Record<string, string>][] = [
    ['light', parseTokens(CSS, ':root')],
    ['dark', parseTokens(CSS, '@media (prefers-color-scheme: dark)')],
  ];
  for (const [label, t] of themes) {
    const solid = (k: string): RGB => parseColor(t[k])!.rgb;
    const over = (k: string, bg: RGB): RGB => { const c = parseColor(t[k])!; return composite(c.rgb, c.alpha, bg); };
    const backdrops: Record<string, RGB> = {
      'open folder': over('--surface-active', solid('--surface-sidebar')),
      'open note': over('--surface-active', solid('--surface-list')),
      'open and selected note': over('--accent-wash-strong', over('--surface-active', solid('--surface-list'))),
    };
    for (const [where, bg] of Object.entries(backdrops)) {
      it(`${label}: --accent-mark on the ${where} clears 3:1`, () => {
        expect(contrast(solid('--accent-mark'), bg)).toBeGreaterThanOrEqual(3);
      });
    }
  }
});

describe('the rail marks the open row, and only it', () => {
  it('the open note and the open folder draw the rail in Lamp Mark', () => {
    expect(rule(NOTELIST, '.note-item.active .note-btn::before')).toMatch(/background:\s*var\(--accent-mark\)/);
    expect(rule(SIDEBAR, '.folder-item.active::before')).toMatch(/background:\s*var\(--accent-mark\)/);
  });

  it('multi-select no longer draws a rail', () => {
    expect(rule(NOTELIST, '.note-item.multi-selected .note-btn')).not.toMatch(/box-shadow|accent-rail/);
  });
});

describe('list header hierarchy', () => {
  it('the list title is Headline size and New note is the Amber Ink control', () => {
    expect(rule(NOTELIST, 'h2')).toMatch(/font-size:\s*var\(--size-xl\)/);
    expect(rule(NOTELIST, '.icon-btn.new-note')).toMatch(/color:\s*var\(--accent-action\)/);
    expect(NOTELIST).toMatch(/class="icon-btn new-note"/);
  });
});

describe('multi-select shows checkboxes', () => {
  const ACCOUNT = 'localfs:vault';
  const note = (k: number) => ({
    uuid: `u${k}`, id: `m${k}`, account_id: ACCOUNT, label: 'Notes', title: `Note ${k}`,
    body_html: `<div>Note ${k}</div>`, date: `2026-01-0${10 - k}T10:00:00Z`, pinned: false, local_version: 1,
  }) as any;
  let host: HTMLElement;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let component: any;
  async function settle() { for (let i = 0; i < 4; i++) { await tick(); await Promise.resolve(); } flushSync(); }

  beforeEach(async () => {
    invoke.mockReset();
    invoke.mockResolvedValue([]);
    const ns = [note(1), note(2), note(3)];
    notes.set(ns);
    selectedNote.set(ns[0]);
    selectedFolder.set('Notes');
    currentAccount.set(ACCOUNT);
    accounts.set([]);
    noteIndex.set(new Map());
    hydratedFolders.set(new Map());
    selectedTags.set(new Set());
    selectedSmartFolder.set(null);
    selectedUuids.set(new Set());
    searchQuery.set('');
    capabilitiesByAccount.set({});
    host = document.createElement('div');
    document.body.appendChild(host);
    component = mount(NoteList, { target: host, props: { width: 240 } });
    await settle();
  });
  afterEach(() => { unmount(component); host.remove(); selectedUuids.set(new Set()); });

  const checks = () => Array.from(host.querySelectorAll<HTMLElement>('.note-check'));

  it('no checkboxes outside multi-select', () => {
    expect(checks()).toHaveLength(0);
  });

  it('every row shows one while any note is selected; selected rows are checked and say so', async () => {
    selectedUuids.set(new Set([noteKey(note(1)), noteKey(note(2))]));
    await settle();
    expect(checks()).toHaveLength(3);
    expect(checks().filter((c) => c.classList.contains('checked'))).toHaveLength(2);
    const selectedRows = Array.from(host.querySelectorAll('.note-item.multi-selected'));
    expect(selectedRows).toHaveLength(2);
    for (const r of selectedRows) expect(r.querySelector('.sr-only')?.textContent).toBe('Selected');
  });

  it('a tap on a checkbox adds or removes that note, without opening it', async () => {
    selectedUuids.set(new Set([noteKey(note(1))]));
    await settle();
    checks()[2].click();
    await settle();
    expect(get(selectedUuids).has(noteKey(note(3)))).toBe(true);
    expect(get(selectedNote)?.uuid).toBe('u1');
    checks()[2].click();
    await settle();
    expect(get(selectedUuids).has(noteKey(note(3)))).toBe(false);
  });
});
