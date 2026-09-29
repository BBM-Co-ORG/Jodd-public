import { mount } from 'svelte';
import { get } from 'svelte/store';
import NoteEditor from '../../src/lib/components/NoteEditor.svelte';
import { notes, selectedNote, currentAccount, accounts } from '../../src/lib/stores/notes';
import { refreshNotePersistence } from '../../src/lib/notePersistence';
import type { Note } from '../../src/lib/types';
import '../../src/styles/tokens.css';
const a: Note = { account_id: 'synthetic-a', uuid: 'same', id: 'a-id', title: 'บันทึก A', body_html: '<body>ข้อความ A</body>', date: '2026-09-19', label: 'Notes/Jodd-Demo', local_version: 1 };
const b: Note = { ...a, account_id: 'synthetic-b', id: 'b-id', title: 'บันทึก B', body_html: '<body>ข้อความ B</body>' };
const rows = new Map([['synthetic-a', { ...a }], ['synthetic-b', { ...b }]]);
const states = new Map([['synthetic-a', 'dirty'], ['synthetic-b', 'clean']]);
let blocked: string | null = null;
const calls: { command: string; args: any }[] = [];
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: { invoke: async (command: string, args: any) => {
  calls.push({ command, args }); const n = rows.get(args?.accountId);
  switch (command) {
    case 'note_persistence': return n ? { account_id: n.account_id, uuid: n.uuid, local_version: n.local_version, sync_state: states.get(args.accountId), push_blocked_reason: args.accountId === 'synthetic-a' ? blocked : null } : null;
    case 'save_note': {
      if (!n) throw new Error('Unknown synthetic account');
      Object.assign(n, { body_html: args.bodyHtml, title: args.title, local_version: n.local_version! + 1 });
      states.set(args.accountId, 'dirty'); return { id: n.id, uuid: n.uuid, local_version: n.local_version };
    }
    case 'note_connections': return { outgoing: [], backlinks: [] };
    case 'note_citations': case 'get_note_attachments': return [];
    default: throw new Error(`Forbidden fixture IPC: ${command}`);
  }
} } });
Object.assign(window, { packageCFixture: {
  calls,
  select: (accountId: string) => selectedNote.set({ ...rows.get(accountId)! }),
  push: async (version: number, uuid?: string) => {
    const n = rows.get('synthetic-a')!;
    if (uuid) n.uuid = uuid;
    if (version === n.local_version) states.set('synthetic-a', 'clean');
    await refreshNotePersistence({ account_id: 'synthetic-a', uuid: 'same' });
  },
  block: async () => { blocked = 'Synthetic permanent refusal'; await refreshNotePersistence(a); },
  unblock: async () => { blocked = null; await refreshNotePersistence(a); },
  snapshot: () => ({ selected: get(selectedNote), notes: get(notes) }),
} });
accounts.set([]); currentAccount.set('synthetic-a'); notes.set([a,b]); selectedNote.set(a);
mount(NoteEditor, { target: document.getElementById('app')! });
