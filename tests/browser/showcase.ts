import { mount } from 'svelte';
import Showcase from './Showcase.svelte';
import { accounts, capabilitiesByAccount, currentAccount, notes, selectedNote, selectedFolder } from '../../src/lib/stores/notes';
import type { Note } from '../../src/lib/types';
import '../../src/styles/tokens.css';
const fullApp = location.pathname.endsWith('/full-app.html');
const callbacks = new Map<number, Function>();
let callbackId = 0;
const eventHandlers = new Map<number, string>();
const rows: Note[] = ['a', 'b'].map(account => ({ account_id: `demo-${account}`, uuid: 'shared-uuid', id: account, title: `Account ${account.toUpperCase()} notebook`, body_html: `<body>Private contents of account ${account.toUpperCase()}</body>`, label: 'Notes/Jodd-Demo', date: '2026-09-19', local_version: 1 }));
const dirty = new Set<string>();
const calls: {command: string; args: any}[] = [];
let failSearch = false;
let serial = 0;
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: { metadata: {currentWindow: {label: 'main'}, currentWebview: {label: 'main'}},
 transformCallback: (callback: Function) => { callbacks.set(++callbackId, callback); return callbackId; },
 unregisterCallback: (id: number) => callbacks.delete(id),
 invoke: async (command: string, args: any) => {
  calls.push({command, args});
  const n = rows.find(n => n.account_id === args?.accountId && n.uuid === (args?.uuid ?? args?.existingUuid));
  switch (command) {
      case 'list_ai_receipts': return [];
      case 'get_ai_limits': return { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
      case 'get_ai_receipt_retention': return 30;
    case 'plugin:event|listen': eventHandlers.set(args.handler, args.event); return args.handler;
    case 'plugin:event|unlisten': eventHandlers.delete(args.eventId); callbacks.delete(args.eventId); return;
    case 'plugin:window|set_title': case 'flush_sync': return;
    case 'plugin:app|version': return '0.28.3';
    case 'platform_name': return 'macos';
    case 'is_authenticated': return true;
    case 'needs_reindex_after_recovery': return false;
    case 'list_accounts': return ['a', 'b'].map(id => ({id: `demo-${id}`, email: `Account ${id.toUpperCase()} (synthetic)`, added_at: '2026-09-19'}));
    case 'list_folders': return ['Notes', 'Notes/Jodd-Demo'];
    case 'list_folder_kinds': case 'list_note_tags': return [];
    case 'get_dup_stats': return {collapsed: 0, uuids_affected: 0};
    case 'backend_capabilities': return {has_trash: false, writes: {notes: true, folders: true, relocate: true, sidecars: true}};
    case 'sync_pin_state': case 'sync_tag_state': return 0;
    case 'list_cached_notes': case 'list_notes': case 'index_account': return rows.filter(n => n.account_id === args.accountId).map(n => ({...n}));
    case 'list_cached_notes_in_folder': case 'list_notes_in_folder': return rows.filter(n => n.account_id === args.accountId && n.label === (args.path ?? args.folderPath ?? args.label)).map(n => ({...n}));
    case 'get_account_settings': return {notes_label: 'Notes', meta_label: 'Notes-Meta'};
    case 'get_llm_settings': return {provider: 'none', data_allowed: false, http_base_url: null, http_model: null, http_api_key_keychain: null, agent_preset: null, agent_custom: null, disable_thinking: false};
    case 'get_oauth_config': case 'get_ms_oauth_config': return {client_id: '', has_secret: false, credentials_available: false};
    case 'get_log_settings': return {file_logging_enabled: false, log_file_path: '(synthetic preview)', log_file_size_bytes: 0};
    case 'list_agent_cli_presets': return [];
    case 'get_app_llm_config': return {cfg: {llm: {provider: 'none', http_base_url: null, http_model: null, http_api_key_keychain: null, agent_preset: null, agent_custom: null, disable_thinking: false}, apply_to_accounts: false}, has_api_key: false};

    case 'note_persistence': return n ? {account_id: n.account_id, uuid: n.uuid, local_version: n.local_version, sync_state: dirty.has(n.id) ? 'dirty' : 'clean', push_blocked_reason: null} : null;
    case 'save_note': {
      const target = n ?? { account_id: args.accountId, id: `created-${++serial}`, uuid: `created-${serial}`, label: args.label, date: '2026-09-19', local_version: 0 } as Note;
      if (!n) rows.push(target);
      Object.assign(target, {title: args.title, body_html: args.bodyHtml, local_version: target.local_version! + 1});
      dirty.add(target.id); return {id: target.id, uuid: target.uuid, local_version: target.local_version};
    }
    case 'search_notes': {
      if (failSearch) throw new Error('Synthetic search unavailable');
      const result = rows.filter(n => (!args.accountId || n.account_id === args.accountId) && (!args.label || n.label === args.label) && `${n.title} ${n.body_html}`.toLowerCase().includes(args.query.toLowerCase())).map(n => ({...n}));
      if (args.query === 'slow') await new Promise(resolve => setTimeout(resolve, 800));
      return result;
    }
    case 'note_connections': return {outgoing: [], backlinks: []};
    case 'note_citations': case 'get_note_attachments': return [];
    default: throw new Error(`Forbidden synthetic IPC: ${command}`);
  }
}}});
Object.assign(window, { showcase: { calls, rows,
 emit: (event: string, payload: unknown) => { for (const [id, name] of eventHandlers) if (name === event) callbacks.get(id)?.({event, id, payload}); },
 setDirty: (id: string, value: boolean) => { if (value) dirty.add(id); else dirty.delete(id); },
 failSearch: (value: boolean) => { failSearch = value; } } });
accounts.set(['a', 'b'].map(id => ({id: `demo-${id}`, email: `Account ${id.toUpperCase()} (synthetic)`, added_at: '2026-09-19'})));
capabilitiesByAccount.set({'demo-a': {has_trash:false}, 'demo-b': {has_trash:false}});
currentAccount.set('demo-a'); selectedFolder.set('Notes/Jodd-Demo'); notes.set(rows.map(n => ({...n}))); selectedNote.set({...rows[0]});
if (fullApp) {
  // Production entry point, with the fixture seam installed before App mounts.
  localStorage.setItem('jodd:lastSeenVersion', '0.28.3');
  void import('../../src/main');
} else {
  mount(Showcase, {target: document.getElementById('app')!});
}
