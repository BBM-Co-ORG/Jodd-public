import { invoke } from '@tauri-apps/api/core';
import { get } from 'svelte/store';
import { unreviewedTrust, unreviewedKey, type Trust } from './stores/notes';

const AGENT_ROOT = 'Notes/__Agent__'; // mcp_scope::AGENT_WORKSPACE

/** The agent workspace is the agents' own space — its notes are never "unreviewed". */
export function isAgentWorkspace(label: string): boolean {
  return label === AGENT_ROOT || label.startsWith(`${AGENT_ROOT}/`);
}

/** "3 h ago" for an ISO timestamp; '' when it does not parse. */
export function relativeAgo(iso: string, now: number = Date.now()): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '';
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}

/**
 * Replace each account's unreviewed set with the backend's current one. An
 * account whose call fails keeps what it had (stale beats absent) and does not
 * stop the others.
 */
export async function refreshUnreviewed(accountIds: string[]): Promise<void> {
  await Promise.all(
    accountIds.map(async (accountId) => {
      let rows: { uuid: string; trust: Trust }[];
      try {
        rows = await invoke<{ uuid: string; trust: Trust }[]>('list_unreviewed_trust', { accountId });
      } catch (e) {
        console.error(`list_unreviewed_trust failed for ${accountId}`, e);
        return;
      }
      unreviewedTrust.update((m) => {
        const next = Object.fromEntries(Object.entries(m).filter(([, v]) => v.account !== accountId));
        for (const r of rows) next[unreviewedKey(accountId, r.uuid)] = { account: accountId, trust: r.trust };
        return next;
      });
    }),
  );
}

/**
 * Optimistic: the chip disappears before the IPC; restored on failure, replaced on stale.
 *
 * `renderedLocalVersion` is the version of the body the editor shows. It is
 * sent instead of the chip's own `note_local_version` (read whenever the trust
 * was), so `Db::verify_note` refuses as stale an event newer than what is on
 * screen — the shown event coalesced forward, or rule 2b's gap — even if the
 * editor's gate on Mark reviewed is ever raced.
 */
export async function markReviewed(
  accountId: string,
  uuid: string,
  renderedLocalVersion?: number,
): Promise<'verified' | 'stale' | 'failed'> {
  const key = unreviewedKey(accountId, uuid);
  const shown = get(unreviewedTrust)[key];
  if (!shown) return 'verified';
  unreviewedTrust.update((m) => {
    const n = { ...m };
    delete n[key];
    return n;
  });
  try {
    const out = await invoke<{ kind: 'verified' | 'stale'; trust: Trust }>('verify_note', {
      accountId,
      uuid,
      seenEventId: shown.trust.event_id,
      seenLocalVersion: renderedLocalVersion ?? shown.trust.note_local_version,
    });
    if (out.kind === 'stale') unreviewedTrust.update((m) => ({ ...m, [key]: { account: accountId, trust: out.trust } }));
    return out.kind;
  } catch (e) {
    console.error('verify_note failed', e);
    unreviewedTrust.update((m) => ({ ...m, [key]: shown }));
    return 'failed';
  }
}
