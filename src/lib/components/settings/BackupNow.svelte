<script lang="ts">
  // One account's backup control: which destination, Back up now, and how the
  // last backup went. Shared by Settings › Backup (one per account) and the
  // account page. The parent loads `destinations` / `statuses` and watches
  // `backup-finished` (backup/state.ts), so this shows what any view started.
  //
  // The only state that is its own is "a backup is running" and the stamp of
  // the one it just finished; nothing here touches a shared store after an
  // awaited call except the status upsert, which is the page's own readout.
  import { runBackup } from '../../backup/api';
  import { destinations, statuses, upsertStatus, friendlyBackupMessage, backupSummary } from '../../backup/state';
  import RestoreDialog from '../backup/RestoreDialog.svelte';

  let { accountId }: { accountId: string } = $props();

  // Live pass 2026-10-24: the select always fell back to the first folder, so
  // an account backed up elsewhere looked never backed up. It now remembers
  // this account's last choice (this device only: a convenience, not state),
  // else the folder of its most recent backup, else the first folder.
  const keyFor = (id: string) => `jodd.backup.destination.${id}`;
  function remembered(id: string): string | null {
    try { return localStorage.getItem(keyFor(id)); } catch { return null; }
  }
  let chosen = $state<string | null>(null);
  // Reads this account's choice, and again if the row is reused for another account.
  $effect(() => {
    chosen = remembered(accountId);
  });
  function choose(id: string) {
    chosen = id;
    try { localStorage.setItem(keyFor(accountId), id); } catch { /* private window: forget it */ }
  }
  const mostRecent = $derived(
    [...$statuses].filter((s) => s.accountId === accountId).sort((a, b) => b.atMs - a.atMs)[0]?.destinationId ?? null,
  );
  const has = (id: string | null) => !!id && $destinations.some((d) => d.id === id);
  // Falls back past a folder that was removed.
  const destinationId = $derived(
    has(chosen) ? chosen : has(mostRecent) ? mostRecent : ($destinations[0]?.id ?? null),
  );
  let restoring = $state(false);
  const last = $derived($statuses.find((s) => s.accountId === accountId && s.destinationId === destinationId));

  let running = $state(false);

  async function backUp() {
    const dest = destinationId;
    if (!dest || running) return;
    running = true;
    try {
      const o = await runBackup(accountId, dest);
      const message = backupSummary(o);
      const atMs = Date.now();
      upsertStatus({ accountId, destinationId: dest, atMs, ok: true, message });
    } catch (e) {
      upsertStatus({ accountId, destinationId: dest, atMs: Date.now(), ok: false, message: String(e) });
    } finally {
      running = false;
    }
  }

  const line = $derived.by(() => {
    if (!last) return '';
    const message = friendlyBackupMessage(last.message);
    // Local time, the same whether this view ran the backup or not. It used
    // to show the backup's file stamp (UTC, `2026-10-24T13-49-45Z`) right
    // after a run and local time after a reopen (Android live pass 2026-10-24).
    return `${message} · ${new Date(last.atMs).toLocaleString()}`;
  });
</script>

{#if $destinations.length === 0}
  <p class="help">Add a backup folder to back this account up.</p>
{:else}
  <div class="row">
    <select aria-label="Backup destination" value={destinationId} onchange={(e) => choose(e.currentTarget.value)}>
      {#each $destinations as d (d.id)}
        <option value={d.id}>{d.name}</option>
      {/each}
    </select>
    <button type="button" class="now" onclick={backUp} disabled={running || !destinationId}>
      {running ? 'Backing up…' : 'Back up now'}
    </button>
    <button type="button" class="now" onclick={() => (restoring = true)} disabled={!destinationId}>Restore…</button>
  </div>
  {#if destinationId}
    <RestoreDialog {destinationId} open={restoring} initialAccountId={accountId} onclose={() => (restoring = false)} />
  {/if}
  <p class="status" class:failed={last && !last.ok} role="status" data-backup-status>{line}</p>
{/if}

<style>
  .row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
  select {
    min-width: 0; max-width: 100%; padding: var(--space-1-5) var(--space-2);
    border: 1px solid var(--border-strong); border-radius: var(--radius);
    background: var(--surface-editor); color: var(--text); font: inherit; font-size: var(--size-sm);
  }
  .now {
    padding: var(--space-1-5) var(--space-3);
    border: 1px solid var(--border); border-radius: var(--radius);
    background: var(--surface-panel); color: var(--text);
    font: inherit; font-size: var(--size-sm); cursor: pointer;
  }
  .now:disabled { opacity: 0.6; cursor: default; }
  :global(.modal.touch) .now, :global(.modal.touch) select { min-height: var(--touch-target); }
  .help { margin: var(--space-2) 0 0; font-size: var(--size-xs); color: var(--text-muted); line-height: var(--leading); }
  .status { margin: var(--space-1-5) 0 0; font-size: var(--size-xs); color: var(--text-muted); line-height: var(--leading); }
  .status:empty { display: none; }
  .status.failed { color: var(--danger); }
</style>
