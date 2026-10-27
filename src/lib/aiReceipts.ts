export interface AiUsage {
  input_tokens: number | null;
  output_tokens: number | null;
  provenance: 'actual' | 'estimated' | 'unknown';
}
export interface AiReceipt {
  run_id: string;
  started_ms: number;
  prompt_version: string;
  storage_failed: boolean;
  steps: { id: string; kind: string; stage: string; outcome: string; latency_ms: number; scope_version: string | null; checks: string[]; metrics?: Record<string, number> }[];
  calls: { step_id: string; provider: string; model: string | null; model_source: string; stage: string; outcome: string; latency_ms: number; usage: AiUsage }[];
}
export function usageLabel(usage: AiUsage): string {
  if (usage.provenance === 'unknown') return 'Usage unknown';
  return `${usage.provenance === 'actual' ? 'Reported' : 'Estimated'} tokens: ${usage.input_tokens ?? 'unknown'} in / ${usage.output_tokens ?? 'unknown'} out`;
}
/** When a run started, in the viewer's own locale. The redacted export
 *  carries the same moment as local RFC 3339 (receipts.rs `started_at`). */
export function startedLabel(startedMs: number): string {
  const d = new Date(startedMs);
  if (!startedMs || Number.isNaN(d.getTime())) return 'Start time unknown';
  return `Started ${d.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'medium' })}`;
}
export function stageLabel(stage: string): string {
  const labels: Record<string, string> = {
    admission: 'Checking request', connection_test: 'Testing connection', retrieving: 'Finding sources', selecting_sources: 'Selecting sources',
    answering: 'Generating answer', validating_citations: 'Checking citation IDs', extract: 'Extracting',
    workflow: 'Generating result', summarize: 'Summarizing', action_items: 'Extracting action items', expand_bullets: 'Expanding bullets', clean_transcript: 'Cleaning transcript', fetching_sources: 'Fetching sources', transcribing: 'Transcribing audio', mapping_source: 'Summarizing source',
    synthesizing: 'Combining sources', awaiting_review: 'Draft ready for review', saving_locally: 'Saving on this device',
    suggesting_folder: 'Checking folder suggestions', suggesting_links: 'Checking related notes',
  };
  return labels[stage] ?? 'Processing request';
}

export function providerLabel(provider: string): string {
  if (provider === 'agent_cli') return 'Agent CLI';
  if (provider === 'agent_cli_remote') return 'Agent CLI (server)';
  return 'HTTP';
}
