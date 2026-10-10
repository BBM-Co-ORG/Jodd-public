// Mirror of the backend's AI boundary (spec 2026-10-08 §4.4): folders hidden
// from agents are hidden from Jodd's AI too. The backend enforces; this only
// lets the UI explain a disabled button instead of failing after a click.
import { writable } from 'svelte/store';
import { invoke } from '@tauri-apps/api/core';
import { isHidden, type AgentWorkspaceStatus } from '../agentWorkspace';

export const AI_HIDDEN_HINT = 'This folder is hidden from AI. Unhide it in Settings → Agent workspace.';

export type AiSource = { kind: 'note'; uuid: string } | { kind: 'pasted' };

export const hiddenFromAi = writable<Record<string, string[]>>({});

export async function refreshHiddenFromAi(): Promise<void> {
  try {
    const s = await invoke<AgentWorkspaceStatus>('agent_workspace_status');
    hiddenFromAi.set(s.hidden ?? {});
  } catch {
    // Keep the last known list; the backend still refuses hidden notes.
  }
}

export function aiHiddenFor(
  hidden: Record<string, string[]>,
  accountId: string | null | undefined,
  label: string | null | undefined,
): boolean {
  return !!accountId && !!label && isHidden(hidden, accountId, label);
}

export function aiSourceFor(mode: 'paste' | 'existing', note: { uuid: string } | null | undefined): AiSource {
  return mode === 'existing' && note ? { kind: 'note', uuid: note.uuid } : { kind: 'pasted' };
}
