<script lang="ts">
  // Settings › About & updates: the update check and About Jodd, a section of
  // their own since 0.48.1 (they were the bottom of General, two taps and a
  // scroll away). Every control applies at once: no forms, no Save.
  import { aboutModalOpen } from '../../stores/ui';
  import { isAndroid, isMacos } from '../../stores/platform';
  import {
    updateState, autoCheck, setAutoCheck, checkForUpdate, installUpdate, percent, offerText, installLabel,
  } from '../../appUpdate';

  const upd = $derived($updateState);
</script>

<!-- Every platform; how an update installs is the update's `method`
     (src/lib/appUpdate.ts, spec 2026-11-11-app-updater-design.md). -->
<section class="section">
  <h4>Updates</h4>
  <label class="toggle-row">
    <input type="checkbox" checked={$autoCheck} onchange={() => setAutoCheck(!$autoCheck)} />
    <span>Check for updates once a day</span>
  </label>
  <p class="help">Asks GitHub for Jodd's latest release. Your notes and accounts are not sent.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    {#if $isAndroid}
      <p>Jodd tells you when a new version is out, and Download opens its release page. Install the Android file from there; Android's installer asks you before anything changes, and refuses an update signed by anyone else.</p>
    {:else if $isMacos}
      <p>Jodd tells you when a new version is out. Installing it yourself from the release page avoids a Keychain prompt this build can't skip.</p>
    {:else}
      <p>Jodd downloads the installer from its GitHub release, checks it was signed by Jodd's release key, then closes and reinstalls itself. Anything still running, such as an Ask Jodd answer, stops as it would when you quit.</p>
    {/if}
  </details>
  <div class="update-row">
    {#if upd.phase === 'checking'}
      <span class="status">Checking…</span>
    {:else if upd.phase === 'upToDate'}
      <span class="status">Jodd {upd.current} is the latest version.</span>
    {:else if upd.phase === 'available'}
      <span class="status">{offerText(upd.update)}</span>
      <button type="button" class="choice active" onclick={() => installUpdate(upd.update)}>{installLabel(upd.update)}</button>
    {:else if upd.phase === 'downloading'}
      <span class="status">Downloading…{#if upd.total > 0} {percent(upd.received, upd.total)}%{/if}</span>
    {:else if upd.phase === 'restarting'}
      <span class="status">Installing — Jodd will close and reopen.</span>
    {:else if upd.phase === 'handedOff'}
      <span class="status">Finish the update in Android's installer.</span>
      <button type="button" class="choice" onclick={() => installUpdate(upd.update)}>Install again</button>
    {:else if upd.phase === 'needsPermission'}
      <span class="status">Allow Jodd under Install unknown apps, then try again.</span>
      <button type="button" class="choice active" onclick={() => installUpdate(upd.update)}>Install</button>
    {:else if upd.phase === 'error' && upd.update}
      <button type="button" class="choice" onclick={() => installUpdate(upd.update!)}>Try again</button>
    {/if}
    {#if upd.phase !== 'checking' && upd.phase !== 'downloading' && upd.phase !== 'restarting'}
      <button type="button" class="choice" onclick={() => checkForUpdate()}>Check for updates</button>
    {/if}
  </div>
  {#if upd.phase === 'error'}<p class="error">{upd.message}</p>{/if}
</section>

<!-- The Sidebar footer's version label opens About on desktop; the phone has
     no footer (spec 2026-10-20 §5.2), so this is its way in there. -->
<section class="section">
  <h4>About</h4>
  <button type="button" class="choice about" onclick={() => aboutModalOpen.set(true)}>About Jodd</button>
</section>

<style>
  .section { margin-bottom: var(--space-4); }
  .about { flex: 0 0 auto; }
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
  .choice {
    flex: 1;
    padding: var(--space-1-5) var(--space-2);
    font-family: inherit;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
  }
  .choice:hover { background: var(--surface-hover); }
  .choice:focus-visible { box-shadow: var(--ring-focus); }
  .choice.active {
    color: var(--accent-on-tint);
    background: var(--accent-tint);
    border-color: var(--accent-border);
    font-weight: var(--weight-semibold);
  }
  .toggle-row {
    display: flex; align-items: center; gap: var(--space-2);
    font-size: var(--size); color: var(--text); margin-bottom: var(--space-1-5); cursor: pointer;
  }
  .toggle-row input { margin: 0; }
  .error { color: var(--danger); font-size: var(--size-sm); margin: var(--space-1) 0 0; }
  .update-row { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-1-5) var(--space-2); margin-top: var(--space-2); }
  .update-row .choice { flex: 0 0 auto; }
  .status { flex: 1 1 100%; font-size: var(--size-sm); color: var(--text-secondary); line-height: var(--leading); }
</style>
