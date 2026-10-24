import { mount } from 'svelte';
import AskJoddModal from '../../src/lib/components/AskJoddModal.svelte';
import { currentAccount, selectedFolder } from '../../src/lib/stores/notes';
import '../../src/styles/tokens.css';
// No App.svelte, credentials, remote calls or live account data.
let serial = 0;
let pending: ((answer: unknown) => void) | undefined;
const callbacks = new Map<number, (event: unknown) => void>();
const calls: { command: string; args: any }[] = [];
const syntheticAnswer = { markdown: 'Synthetic answer', cited: [], notes_in_scope: 1,
  notes_considered: 1, notes_used: 1, trimmed: false, dropped_citations: 0 };
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: {
  transformCallback: (callback: (event: unknown) => void) => { const id = ++serial; callbacks.set(id, callback); return id; },
  unregisterCallback: (id: number) => callbacks.delete(id),
  invoke: async (command: string, args: any) => {
    calls.push({ command, args });
    switch (command) {
      case 'list_ai_receipts': return [];
      case 'get_ai_limits': return { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
      case 'get_ai_receipt_retention': return 30;
      case 'plugin:event|listen': return args.handler;
      case 'plugin:event|unlisten': callbacks.delete(args.eventId); return;
      case 'begin_ask': return { session_id: `session-${++serial}`,
        destination: 'Agent CLI synthetic · destination/model follow the CLI configuration; may use cloud services',
        scope: args.scope.kind === 'folder' ? 'synthetic · Notes/Demo and subfolders' : 'All AI-allowed active accounts (1); disabled accounts excluded' };
      case 'end_ask': case 'cancel_ask': return;
      case 'ask_jodd': return new Promise(resolve => { pending = resolve; });
      default: throw new Error(`Forbidden fixture IPC: ${command}`);
    }
  },
} });
Object.assign(window, { packageBFixture: {
  calls,
  resolve: () => { pending?.(syntheticAnswer); pending = undefined; },
  revoke: () => { for (const callback of callbacks.values()) callback({ event: 'ai-policy-changed', payload: null }); },
} });
currentAccount.set('synthetic'); selectedFolder.set('Notes/Demo');
mount(AskJoddModal, { target: document.getElementById('app')!, props: { open: true } });
