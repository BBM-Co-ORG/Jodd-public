import { mount } from 'svelte';
import Fixture from './PackageE.svelte';
import '../../src/styles/tokens.css';
let settings = { automatic_enrichment: false, max_concurrent: 2, max_attempts: 16, workflow_units: 512000, session_units: 4000000, output_tokens: 4096, output_parameter: 'max_tokens' };
const calls: string[] = [];
Object.defineProperty(window, '__TAURI_INTERNALS__', { value: { invoke: async (command: string, args: any) => {
  calls.push(command);
  if (command === 'get_ai_limits') return structuredClone(settings);
  if (command === 'set_ai_limits') { settings = structuredClone(args.settings); return; }
  throw new Error(`Forbidden synthetic IPC: ${command}`);
} } });
Object.assign(window, { packageEFixture: { calls, settings: () => settings } });
mount(Fixture, { target: document.getElementById('app')! });
