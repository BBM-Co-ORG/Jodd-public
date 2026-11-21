<script lang="ts">
  // The update offer, on every platform (appUpdate.ts). App-level, like
  // ErrorBar, so it shows whichever pane is open, and it owns the once-a-day
  // check.
  //
  // It shares ErrorBar's place at the bottom edge and its calm look, and it
  // steps aside while an error is up: the two never stack, and an error is
  // the one that needs the user.
  import { onMount } from 'svelte';
  import { error } from '../stores/notes';
  import {
    updateState, dismissedVersion, bannerVisible, installUpdate, dismissUpdate,
    scheduleAutoCheck, percent, offerText, installLabel,
  } from '../appUpdate';

  onMount(() => scheduleAutoCheck());

  const shown = $derived(!$error && bannerVisible($updateState, $dismissedVersion));
</script>

{#if shown}
  {@const s = $updateState}
  {#if s.phase === 'downloading'}
    <div class="update-bar" role="status">
      <p>Downloading Jodd {s.update.version}…{#if s.total > 0} <span aria-live="off">{percent(s.received, s.total)}%</span>{/if}</p>
    </div>
  {:else if s.phase === 'restarting'}
    <div class="update-bar" role="status">
      <p>Installing — Jodd will close and reopen.</p>
    </div>
  {:else if s.phase === 'available' || s.phase === 'needsPermission' || s.phase === 'handedOff' || s.phase === 'error'}
    {@const update = s.update!}
    <div class="update-bar" role={s.phase === 'error' ? 'alert' : 'status'}>
      <p>
        {#if s.phase === 'available'}
          {offerText(update)}
        {:else if s.phase === 'needsPermission'}
          To update, allow Jodd under Install unknown apps, then tap Install.
        {:else if s.phase === 'handedOff'}
          Finish the update in Android's installer.
        {:else}
          Couldn't update Jodd. {s.message}
        {/if}
      </p>
      <div class="actions">
        <button type="button" class="primary" onclick={() => installUpdate(update)}>
          {s.phase === 'available' ? installLabel(update) : s.phase === 'needsPermission' ? 'Install' : s.phase === 'handedOff' ? 'Install again' : 'Try again'}
        </button>
        <button type="button" onclick={() => dismissUpdate(update.version)}>Later</button>
      </div>
    </div>
  {/if}
{/if}

<style>
  /* Pinned like ErrorBar (.app-layout is 100dvh with overflow hidden), with
     its calm colours and its safe-area inset for Android's nav bar. */
  .update-bar {
    position: fixed;
    bottom: 0;
    left: 0;
    right: 0;
    z-index: var(--z-banner);
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-3);
    padding: var(--space-2) var(--space-4);
    padding-bottom: calc(var(--space-2) + env(safe-area-inset-bottom));
    background: var(--surface-panel);
    color: var(--text);
    border-top: 1px solid var(--border);
    font-size: var(--size-sm);
  }
  p {
    flex: 1 1 16rem;
    min-width: 0;
    margin: 0;
    line-height: var(--leading);
    overflow-wrap: anywhere;
  }
  span { font-variant-numeric: tabular-nums; }
  .actions { display: flex; gap: var(--space-1); margin-left: auto; }
  button {
    min-height: var(--hit-target-desktop);
    padding: 0 var(--space-3);
    font-family: inherit;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    background: none;
    border: 1px solid transparent;
    border-radius: var(--radius);
    cursor: pointer;
    transition: background var(--dur-fast) var(--ease-out);
  }
  button:hover { background: var(--surface-hover); }
  button:focus-visible { box-shadow: var(--ring-focus); }
  button.primary {
    color: var(--accent-on-tint);
    background: var(--accent-tint);
    border-color: var(--accent-border);
    font-weight: var(--weight-semibold);
  }
  @media (pointer: coarse) {
    button { min-height: var(--touch-target); }
  }
</style>
