<script lang="ts">
  // Settings › Backup (spec 2026-10-23 backup-restore, M1): where backups go,
  // and a Back up now per account. Local folders only for now. Every choice
  // applies at once, so there are no forms and no page Save.
  //
  // Backups are plain files (the spec's M1 has no encryption), so adding a
  // folder says that first, at the point of choice, and writes nothing until
  // the owner agrees.
  import { onMount } from 'svelte';
  import { accounts, accountDisplay } from '../../stores/notes';
  import { addLocalDestination, removeDestination } from '../../backup/api';
  import type { DestinationConfig } from '../../backup/api';
  import { pickFolder } from '../../backup/pickFolder';
  import { destinations, refreshBackupState, watchBackupFinished } from '../../backup/state';
  import PromptDialogs from '../PromptDialogs.svelte';
  import BackupNow from './BackupNow.svelte';
  import RestoreDialog from '../backup/RestoreDialog.svelte';

  let promptDialogs: PromptDialogs;

  let error = $state<string | null>(null);
  let adding = $state(false);
  // The restore dialog reads the backups at this destination.
  let browsing = $state<DestinationConfig | null>(null);

  onMount(() => {
    void refreshBackupState().then((e) => { error = e; });
    return watchBackupFinished();
  });

  const active = $derived($accounts.filter((a) => a.status === undefined || a.status === 'active'));

  async function addFolder() {
    if (adding) return;
    adding = true;
    error = null;
    try {
      const dir = await pickFolder('Choose a backup folder', () => false, {
        confirm: (title, message) => promptDialogs.askConfirm(title, message),
      });
      if (!dir) return; // cancelled
      const agreed = await promptDialogs.askConfirm(
        'Backups are not encrypted',
        'Jodd writes backups as plain files in the folder you choose. Anyone who can open that folder can read your notes. Encrypted backups are planned.',
      );
      if (!agreed) return;
      const name = dir.replace(/\\/g, '/').split('/').filter(Boolean).pop() ?? dir;
      await addLocalDestination(name, dir);
      error = await refreshBackupState();
    } catch (e) {
      error = String(e);
    } finally {
      adding = false;
    }
  }

  async function remove(d: DestinationConfig) {
    error = null;
    try {
      await removeDestination(d.id);
      error = await refreshBackupState();
    } catch (e) {
      error = String(e);
    }
  }
</script>

<PromptDialogs bind:this={promptDialogs} />
<RestoreDialog destinationId={browsing?.id ?? ''} open={browsing !== null} onclose={() => (browsing = null)} />

<section class="section">
  <h4>Backup folders</h4>
  <p class="help">Each backup is one file per account, kept in a folder you choose.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>
      Removing a folder here only makes Jodd forget it; the backups already in it stay.
      Put the folder where another device can reach it, such as a synced folder, to
      restore there.
    </p>
  </details>
  {#if $destinations.length > 0}
    <ul class="dests">
      {#each $destinations as d (d.id)}
        <li class="dest">
          <div class="dest-text">
            <span class="dest-name">{d.name}</span>
            <span class="dest-path">{d.path}</span>
          </div>
          <div class="dest-actions">
            <button type="button" class="link-btn" onclick={() => (browsing = d)}>Browse backups…</button>
            <button type="button" class="link-btn" onclick={() => remove(d)}>Remove</button>
          </div>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="help">No backup folder yet.</p>
  {/if}
  <div class="actions">
    <button type="button" class="add-btn" onclick={addFolder} disabled={adding}>Add local folder</button>
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</section>

<section class="section">
  <h4>Accounts</h4>
  {#each active as a (a.id)}
    <div class="acct">
      <span class="acct-name">{accountDisplay(a)}</span>
      <BackupNow accountId={a.id} />
    </div>
  {:else}
    <p class="help">No active account to back up.</p>
  {/each}
</section>

<style>
  .section { margin-bottom: var(--space-5); }
  h4 { margin: 0 0 var(--space-1); font-size: var(--size-md); font-weight: 600; }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
  .dests { list-style: none; margin: 0 0 var(--space-3); padding: 0; }
  .dest {
    display: flex; align-items: center; gap: var(--space-3); flex-wrap: wrap;
    padding: var(--space-2) var(--space-1); border-bottom: 1px solid var(--border-subtle);
  }
  .dest-text { flex: 1 1 12rem; min-width: 0; display: flex; flex-direction: column; }
  .dest-name { font-size: var(--size); }
  .dest-path {
    font-size: var(--size-xs); color: var(--text-muted); font-family: var(--font-mono);
    overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  }
  .dest-actions { flex: none; display: flex; gap: var(--space-3); }
  .actions { margin: 0 0 var(--space-3); }
  .add-btn {
    background: none; border: none; padding: var(--space-1) 0;
    color: var(--accent-action); font: inherit; font-size: var(--size); cursor: pointer;
  }
  .add-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .acct { margin-bottom: var(--space-4); }
  .acct-name { display: block; font-weight: 600; font-size: var(--size-sm); color: var(--text-secondary); margin-bottom: var(--space-1-5); }
  .error {
    background: var(--danger-wash); color: var(--danger); padding: var(--space-2);
    border-radius: var(--radius); font-size: var(--size-sm); margin: 0 0 var(--space-3);
  }
</style>
