import { mount } from 'svelte';
import AiReceipts from '../../src/lib/components/AiReceipts.svelte';
import type { AiReceipt } from '../../src/lib/aiReceipts';
import '../../src/styles/tokens.css';
let rows: AiReceipt[] = [{ run_id: 'synthetic-run', started_ms: 0, prompt_version: 'synthetic-v1', storage_failed: false,
  steps: [{ id: 'primary', kind: 'ingest_sources', stage: 'mapping_source', outcome: 'running', latency_ms: 100, scope_version: 'opaque', checks: ['permission_before_dispatch'] }],
  calls: [{ step_id: 'primary', provider: 'http', model: 'gpt-4o-mini', model_source: 'configured', stage: 'mapping_source', outcome: 'succeeded', latency_ms: 80, usage: { input_tokens: 120, output_tokens: 24, provenance: 'actual' } },
    { step_id: 'primary', provider: 'agent_cli', model: null, model_source: 'unknown', stage: 'mapping_source', outcome: 'running', latency_ms: 0, usage: { input_tokens: null, output_tokens: null, provenance: 'unknown' } }] }];
let retention = 30;
const calls: string[] = [];
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: { invoke: async (command: string, args: any) => {
  calls.push(command);
  switch (command) {
    case 'list_ai_receipts': return structuredClone(rows);
    case 'get_ai_receipt_retention': return retention;
    case 'set_ai_receipt_retention': retention = args.days; return;
    case 'delete_ai_receipts': rows = args.runId ? rows.filter(r => r.run_id !== args.runId) : []; return;
    case 'export_ai_receipts': return JSON.stringify([{ run: 1, calls: 2, usage: 'unknown' }]);
    default: throw new Error(`Forbidden synthetic IPC: ${command}`);
  }
} } });
Object.assign(window, { packageDFixture: { calls, finish: () => { rows[0].steps[0].outcome = 'cancelled'; rows[0].calls[1].outcome = 'cancelled'; } } });
mount(AiReceipts, { target: document.getElementById('app')!, props: { history: true } });
