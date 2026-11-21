// The frontend face of backup and restore: the serde shapes of
// src-tauri/src/backup_commands.rs and a typed wrapper per command. The
// shapes mirror the Rust types field for field (camelCase where the struct
// says so); change one side and the other in the same commit.
//
// Tauri maps a snake_case Rust parameter to a camelCase invoke argument
// (`destination_id` -> `destinationId`), and a struct parameter named `req`
// is passed as `{ req: {...} }`.
import { invoke } from '@tauri-apps/api/core';

/** `backup/config.rs` DestinationConfig; the flattened `kind` tag is `local` for now. */
export type DestinationConfig = { id: string; name: string; kind: 'local'; path: string };

/** `backup/config.rs` LastBackup: how the latest backup of one account to one destination went. */
export type LastBackup = {
  accountId: string;
  destinationId: string;
  atMs: number;
  ok: boolean;
  message: string;
};

/** `backup/run.rs` BackupOutcome. */
export type BackupOutcome = {
  stamp: string;
  notes: number;
  partial: number;
  skipped: number;
  bytes: number;
};

/** The `backup-finished` event payload (backup_commands.rs BackupFinished). */
export type BackupFinished = {
  accountId: string;
  destinationId: string;
  ok: boolean;
  message: string;
};

/** What started a backup (`backup/mod.rs` Trigger, kebab-case). */
export type Trigger = 'manual' | 'scheduled' | 'pre-restore';

/** `backup/manifest.rs` ManifestNote. */
export type ManifestNote = {
  uuid: string;
  title: string;
  folder: string;
  modified: string | null;
  contentSha256: string;
  attachments: number;
  partial: string | null;
};

/** `backup/manifest.rs` Manifest. `skipped` is `[uuid, reason]` pairs. */
export type Manifest = {
  joddBackup: number;
  accountId: string;
  backend: string;
  rootLabel: string;
  createdMs: number;
  appVersion: string;
  device: string;
  trigger: Trigger;
  keep: boolean;
  zipBytes: number;
  zipSha256: string;
  notes: ManifestNote[];
  folders: string[];
  skipped: [string, string][];
};

/** `backup/destination.rs` VersionEntry. */
export type VersionEntry = { slug: string; stamp: string; manifest: Manifest };

/** `backup/restore/transform.rs` Placement. */
export type Placement = 'original' | { underNewFolder: string };

/** `backup/restore/plan.rs` ExistingChoice. */
export type ExistingChoice = 'keepBoth' | 'replace' | 'skip';

/** `backup/restore/plan.rs` RestoreRequest. */
export type RestoreRequest = {
  destinationId: string;
  slug: string;
  stamp: string;
  sourceUuids: string[];
  targetAccount: string;
  placement: Placement;
  existing: ExistingChoice;
};

/** `backup/restore/transform.rs` Loss: something a restore could not carry over faithfully. */
export type Loss =
  | { kind: 'folderFlattened'; from: string }
  | { kind: 'attachment'; filename: string }
  | { kind: 'pin' }
  | { kind: 'tagsAsKeywords' }
  | { kind: 'partial'; reason: string };

/** `backup/restore/plan.rs` Action. */
export type Action =
  | { kind: 'create' }
  | { kind: 'skipAlreadyRestored' }
  | { kind: 'skipContentMatch' }
  | { kind: 'keepBoth'; existingUuid: string }
  | { kind: 'replace'; existingUuid: string }
  | { kind: 'skipExisting' }
  | { kind: 'refuse'; reason: string };

/** `backup/restore/plan.rs` PlannedNote. */
export type PlannedNote = {
  sourceUuid: string;
  title: string;
  targetFolder: string;
  action: Action;
  losses: Loss[];
};

/** `backup/restore/plan.rs` RestorePlan. */
export type RestorePlan = { notes: PlannedNote[]; foldersToCreate: string[] };

/** `backup/restore/apply.rs` RestoreReport. `refused` is `[uuid, reason]`, `losses` is `[uuid, loss]`. */
export type RestoreReport = {
  created: number;
  replaced: number;
  skipped: number;
  refused: [string, string][];
  losses: [string, Loss][];
};

/** What `backup_run` rejects with before an account's first full sync. */
export const NOT_SYNCED_YET = 'NOT_SYNCED_YET';
/** What `restore_preview` / `restore_apply` reject with for such a target. */
export const TARGET_NOT_SYNCED_YET = 'TARGET_NOT_SYNCED_YET';

export const backupDestinations = () => invoke<DestinationConfig[]>('backup_destinations');

export const addLocalDestination = (name: string, path: string) =>
  invoke<DestinationConfig>('backup_add_local_destination', { name, path });

/** Forgets the config entry; the files at the destination are never touched. */
export const removeDestination = (id: string) => invoke<void>('backup_remove_destination', { id });

export const runBackup = (accountId: string, destinationId: string) =>
  invoke<BackupOutcome>('backup_run', { accountId, destinationId });

export const backupStatuses = () => invoke<LastBackup[]>('backup_statuses');

export const listBackupSlugs = (destinationId: string) =>
  invoke<string[]>('backup_list_slugs', { destinationId });

export const listBackupVersions = (destinationId: string, slug: string) =>
  invoke<VersionEntry[]>('backup_list_versions', { destinationId, slug });

export const deleteBackupVersion = (destinationId: string, slug: string, stamp: string) =>
  invoke<void>('backup_delete_version', { destinationId, slug, stamp });

export const restorePreview = (req: RestoreRequest) => invoke<RestorePlan>('restore_preview', { req });

export const restoreApply = (req: RestoreRequest) => invoke<RestoreReport>('restore_apply', { req });
