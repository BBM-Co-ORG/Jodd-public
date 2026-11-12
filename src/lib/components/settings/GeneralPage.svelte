<script lang="ts">
  // Settings › General: Look, Appearance, Date display, Updates (every platform),
  // About. Ask Jodd history moved to Privacy & Data › Records (spec 2026-11-11). Every
  // control here applies at once, so the page has no forms and no Save.
  // Appearance moved verbatim from the old AppSettings.svelte.
  import { getLookPref, getThemePref, LOOKS, setLookPref, setThemePref, type LookPref, type ThemePref } from '../../theme';
  import { calendarPref, setCalendarPref, type CalendarPref } from '../../format/dates';
  import { aboutModalOpen } from '../../stores/ui';
  import { isAndroid, isMacos } from '../../stores/platform';
  import {
    updateState, autoCheck, setAutoCheck, checkForUpdate, installUpdate, percent, offerText, installLabel,
  } from '../../appUpdate';

  let theme = $state<ThemePref>(getThemePref());
  const upd = $derived($updateState);

  function chooseTheme(next: ThemePref) {
    theme = next;
    setThemePref(next);
  }

  let look = $state<LookPref>(getLookPref());

  function chooseLook(next: LookPref) {
    look = next;
    setLookPref(next);
  }

  const CALENDARS: [CalendarPref, string][] = [
    ['auto', 'Automatic'],
    ['gregory', 'Gregorian'],
    ['buddhist', 'Buddhist (BE)'],
  ];
</script>

<section class="section">
  <h4 id="look-heading">Look</h4>
  <!-- Each card is drawn in its own look: sidebar, list and editor with the
       accent on the editor, light on top and dark below, and the name in the
       look's face. -->
  <div class="looks" role="group" aria-labelledby="look-heading">
    {#each LOOKS as l (l.id)}
      <button
        type="button"
        aria-pressed={look === l.id}
        class="look"
        class:active={look === l.id}
        onclick={() => chooseLook(l.id)}
      >
        <span class="look-swatch" aria-hidden="true">
          {#each [l.light, l.dark] as row, r (r)}
            <span class="look-row">
              {#each row.slice(0, 3) as c, i (i)}<span style:background={c}></span>{/each}
              <span class="look-accent" style:background={row[3]}></span>
            </span>
          {/each}
        </span>
        <span class="look-name" style:--look-face={l.face}>{l.label}</span>
        <span class="look-blurb">{l.blurb}</span>
      </button>
    {/each}
  </div>
  <p class="help">Every look has a light and a dark version. Appearance chooses which.</p>
</section>

<section class="section">
  <h4 id="appearance-heading">Appearance</h4>
  <div class="choice-group" role="group" aria-labelledby="appearance-heading">
    {#each [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']] as [value, label] (value)}
      <button
        type="button"
        aria-pressed={theme === value}
        class="choice"
        class:active={theme === value}
        onclick={() => chooseTheme(value as ThemePref)}
      >{label}</button>
    {/each}
  </div>
  <p class="help">System follows your operating system setting.</p>
</section>

<section class="section">
  <h4 id="date-display-heading">Date display</h4>
  <div class="choice-group" role="group" aria-labelledby="date-display-heading">
    {#each CALENDARS as [value, label] (value)}
      <button
        type="button"
        aria-pressed={$calendarPref === value}
        class="choice"
        class:active={$calendarPref === value}
        onclick={() => setCalendarPref(value)}
      >{label}</button>
    {/each}
  </div>
</section>

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
     no footer (spec 2026-10-20 §5.2), so this is its way in. -->
<section class="section">
  <h4>About</h4>
  <button type="button" class="choice about" onclick={() => aboutModalOpen.set(true)}>About Jodd</button>
</section>

<style>
  .section { margin-bottom: var(--space-4); }
  .about { flex: 0 0 auto; }
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: var(--weight-semibold); }
  .help { font-size: var(--size-sm); color: var(--text-muted); margin: 0 0 var(--space-3); line-height: var(--leading); }
  .choice-group { display: flex; gap: var(--space-1-5); margin-bottom: var(--space-2); }
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
  .looks {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: var(--space-2);
    margin-bottom: var(--space-2);
  }
  .look {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    padding: var(--space-2);
    font-family: inherit;
    text-align: left;
    color: var(--text);
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
  }
  .look:hover { background: var(--surface-hover); }
  .look:focus-visible { box-shadow: var(--ring-focus); }
  /* Chosen: the accent border, doubled so it reads without colour. */
  .look.active {
    border-color: var(--accent-action);
    box-shadow: inset 0 0 0 1px var(--accent-action);
  }
  .look-swatch {
    display: grid;
    grid-template-rows: 1fr 1fr;
    height: var(--space-8);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .look-row {
    position: relative;
    display: grid;
    grid-template-columns: 2fr 3fr 4fr;
  }
  /* The accent, as each look uses it: one small mark on the editor pane. */
  .look-accent {
    position: absolute;
    right: var(--space-1-5);
    top: calc(50% - var(--space-0-5));
    width: var(--space-3);
    height: var(--space-1);
    border-radius: var(--radius-sm);
  }
  .look-name {
    font-family: var(--look-face, inherit);
    font-size: var(--size);
    font-weight: var(--weight-semibold);
    line-height: var(--leading);
  }
  .look-blurb {
    font-size: var(--size-xs);
    color: var(--text-muted);
    line-height: var(--leading);
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
