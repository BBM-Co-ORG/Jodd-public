<script lang="ts">
  // Settings › Accounts: one row per account (name, backend, status dot, the
  // duplicate count when > 0), each drilling into its own page
  // (`accounts/<id>`), then the add-account kinds exactly as the Sidebar's
  // account panel offers them on this platform.
  import { onMount } from 'svelte';
  import { accounts, accountDisplay, backendLabel, isAuthenticated } from '../../stores/notes';
  import { settingsRoute, localFolderBusy } from '../../stores/ui';
  import { isAndroid, supportsIcloud } from '../../stores/platform';
  import { dupStats, refreshDupStats } from '../../stores/dupStats';
  import { startAddAccount } from '../../controllers/accountActions';
  import Icon from '../Icon.svelte';

  // The same count the Sidebar's dup pill shows (stores/dupStats.ts); read it
  // fresh on open, since the Sidebar may not be mounted (phone).
  onMount(() => {
    for (const a of $accounts) void refreshDupStats(a.id);
  });

  function statusOf(status: string | undefined): 'active' | 'draining' | 'inactive' {
    return status === 'draining' || status === 'inactive' ? status : 'active';
  }
  const STATUS_LABEL = { active: 'Active', draining: 'Finishing sync', inactive: 'Inactive' };
</script>

<ul class="rows">
  {#each $accounts as a (a.id)}
    {@const st = statusOf(a.status)}
    {@const dups = $dupStats[a.id]?.collapsed ?? 0}
    <li>
      <button type="button" class="row" data-account-id={a.id} onclick={() => settingsRoute.set('accounts/' + a.id)}>
        <span class="dot {st}" role="img" aria-label={STATUS_LABEL[st]} title={STATUS_LABEL[st]}></span>
        <span class="name">{accountDisplay(a)}</span>
        {#if a.backend_kind}<span class="backend">{backendLabel(a.backend_kind)}</span>{/if}
        {#if dups > 0}<span class="dups">{dups} duplicates</span>{/if}
        <span class="chev"><Icon name="chevron-right" size={12} /></span>
      </button>
    </li>
  {/each}
</ul>

<section class="add">
  <h4>Add account</h4>
  <button class="add-btn" onclick={() => startAddAccount('gmail')}>Add Gmail account</button>
  <!-- Every platform, Android included: Microsoft redirects through the
       same App Links URL Gmail uses (`auth_ms::redirect_uri`). -->
  <button class="add-btn" onclick={() => startAddAccount('microsoft')}>Add Microsoft account</button>
  {#if $supportsIcloud}
    <button class="add-btn" onclick={() => startAddAccount('icloud')}>Add iCloud account</button>
  {/if}
  <!-- These three run in AccountAddHost, which App.svelte mounts only once
       signed in (Settings is reachable from the sign-in screen too). A
       request made before that would wait and fire after sign-in. -->
  {#if $isAuthenticated}
    <button class="add-btn" onclick={() => startAddAccount('localfs')} disabled={$localFolderBusy}>Add Local Folder</button>
    <!-- Managed SSH works on Android through the embedded russh client;
         Advanced SSH needs the user's own ssh-agent / ~/.ssh/config,
         which a phone does not have. -->
    <button class="add-btn" onclick={() => startAddAccount('ssh')}>Add SSH Server</button>
    {#if !$isAndroid}
      <button class="add-btn" onclick={() => startAddAccount('ssh_advanced')}>Add SSH Server (advanced)</button>
    {/if}
  {/if}
</section>

<style>
  .rows { list-style: none; margin: 0 0 var(--space-4); padding: 0; }
  .row {
    display: flex; align-items: center; gap: var(--space-2);
    width: 100%; min-height: var(--hit-target-desktop);
    padding: var(--space-2) var(--space-1);
    border: 0; border-bottom: 1px solid var(--border-subtle);
    background: transparent; color: var(--text);
    font: inherit; font-size: var(--size); text-align: left; cursor: pointer;
  }
  .row:hover { background: var(--surface-hover); }
  :global(.modal.touch) .row { min-height: var(--touch-target); }
  .dot {
    flex: none; width: var(--space-2); height: var(--space-2);
    border-radius: 50%; background: var(--success);
  }
  .dot.draining { background: var(--accent); }
  .dot.inactive { background: var(--text-disabled); }
  .name { flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .backend, .dups {
    flex: none; font-size: var(--size-xs); color: var(--text-muted);
    background: var(--surface-badge); border-radius: var(--radius-sm);
    padding: var(--space-0-5) var(--space-1-5);
  }
  .dups { color: var(--danger); background: var(--danger-wash); }
  .chev { flex: none; color: var(--text-muted); display: inline-flex; }
  .add { display: flex; flex-direction: column; align-items: flex-start; gap: var(--space-1); }
  h4 { margin: 0 0 var(--space-1); font-size: var(--size-md); font-weight: 600; }
  .add-btn {
    background: none; border: none; padding: var(--space-1) 0;
    color: var(--accent-action); font: inherit; font-size: var(--size); cursor: pointer;
  }
  .add-btn:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
