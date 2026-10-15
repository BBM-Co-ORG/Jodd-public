import { invoke } from '@tauri-apps/api/core';
export interface AiLimits {
  automatic_enrichment: boolean;
  max_concurrent: number;
  max_attempts: number;
  workflow_units: number;
  session_units: number;
  output_tokens: number;
  output_parameter: 'max_tokens' | 'max_completion_tokens' | 'unsupported';
}
/** Read for each saved result; fail closed without preventing the primary save. */
export async function automaticEnrichmentEnabled(): Promise<boolean> {
  try { return (await invoke<AiLimits>('get_ai_limits')).automatic_enrichment === true; }
  catch { return false; }
}
