// What the Backup page and the account page share: the destination list and
// the last-backup status of every (account, destination) pair. Both views
// read these stores, so a backup started from one shows on the other, and a
// `backup-finished` event (a run from another view, or the scheduler later)
// updates both without a reload.
import { listen } from '@tauri-apps/api/event';
import { writable } from 'svelte/store';
import { backupDestinations, backupStatuses, NOT_SYNCED_YET, TARGET_NOT_SYNCED_YET } from './api';
import type { BackupFinished, BackupOutcome, DestinationConfig, LastBackup } from './api';

export const destinations = writable<DestinationConfig[]>([]);
export const statuses = writable<LastBackup[]>([]);

/** Reloads both from the backend. Returns the failure, or null. */
export async function refreshBackupState(): Promise<string | null> {
  try {
    const [d, s] = await Promise.all([backupDestinations(), backupStatuses()]);
    destinations.set(d);
    statuses.set(s);
    return null;
  } catch (e) {
    return String(e);
  }
}

/** Replaces the entry for the same account and destination, as the backend does. */
export function upsertStatus(s: LastBackup): void {
  statuses.update((all) => [
    ...all.filter((x) => !(x.accountId === s.accountId && x.destinationId === s.destinationId)),
    s,
  ]);
}

/**
 * Follows `backup-finished` into `statuses`. Returns the unsubscribe; one
 * watcher per view that shows statuses is enough.
 */
export function watchBackupFinished(): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  listen<BackupFinished>('backup-finished', (e) => {
    const p = e.payload;
    upsertStatus({ accountId: p.accountId, destinationId: p.destinationId, atMs: Date.now(), ok: p.ok, message: p.message });
  })
    .then((u) => {
      if (disposed) u();
      else unlisten = u;
    })
    .catch((e) => console.error('backup-finished listener failed', e));
  return () => {
    disposed = true;
    unlisten?.();
    unlisten = null;
  };
}

/** The sentence a status, a failed run or a failed restore step reads as. */
export function friendlyBackupMessage(message: string): string {
  return message === NOT_SYNCED_YET || message === TARGET_NOT_SYNCED_YET ? "Waiting for this account's first full sync" : message;
}

/**
 * The status line of a finished backup, as `backup_summary` in
 * backup_commands.rs words it; change both together. Partial notes are
 * named (spec §11) so the owner sees what the backup could not hold whole.
 */
export function backupSummary(o: BackupOutcome): string {
  const counts: string[] = [];
  if (o.partial > 0) counts.push(`${o.partial} partial`);
  if (o.skipped > 0) counts.push(`${o.skipped} skipped`);
  return `Backed up ${o.notes} notes` + (counts.length > 0 ? ` (${counts.join(', ')})` : '');
}
