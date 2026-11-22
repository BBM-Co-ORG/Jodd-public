import { describe, it, expect, vi, beforeEach } from 'vitest';
import { get } from 'svelte/store';

const invoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { openCreatedNote } from './openCreatedNote';
import { notes } from './stores/notes';
import type { Note } from './types';

const ACCT = 'gmail:a@example.com';
const row = (uuid: string, label: string, body: string) => ({ uuid, account_id: ACCT, id: '', title: uuid, body_html: body, label, date: '' }) as unknown as Note;

describe('openCreatedNote', () => {
  beforeEach(() => {
    invoke.mockReset();
    notes.set([]);
  });

  // Gotcha #6: a link ingest that rewrote a saved copy in place must reach
  // the list too, or the source note shows its old text until a refresh.
  it('repaints the folder of every source note the ingest updated in place', async () => {
    notes.set([row('src-1', 'Notes/__Sources__', 'old copy')]);
    invoke.mockImplementation(async (cmd: string, args: { path?: string }) => {
      if (cmd === 'list_cached_notes_in_folder') {
        return args.path === 'Notes/__Sources__' ? [row('src-1', 'Notes/__Sources__', 'new copy')] : [row('derived', 'Notes/Inbox', 'points')];
      }
      return [];
    });
    await openCreatedNote(ACCT, {
      uuid: 'derived',
      label: 'Notes/Inbox',
      copies: [
        { url: 'https://a.example/', outcome: 'updated', saved_on: '2026-11-18', updated_label: 'Notes/__Sources__' },
        { url: 'https://b.example/', outcome: 'unchanged', saved_on: '2026-11-18', updated_label: null },
      ],
    });
    const painted = invoke.mock.calls.filter(([c]) => c === 'list_cached_notes_in_folder').map(([, a]) => (a as { path: string }).path);
    expect(painted).toEqual(['Notes/Inbox', 'Notes/__Sources__']);
    expect(get(notes).find((n) => n.uuid === 'src-1')?.body_html).toBe('new copy');
  });
});
