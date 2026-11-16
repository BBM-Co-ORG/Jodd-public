<script lang="ts">
  // Settings › Appearance (was General until 0.48.1): Look, Light or dark, Date
  // display. Updates and About moved to their own section, About & updates
  // (AboutPage.svelte), so they are one tap from Settings. Ask Jodd history
  // moved to Privacy & Data › Records (spec 2026-11-11). Every control here
  // applies at once, so the page has no forms and no Save. Light or dark moved
  // verbatim from the old AppSettings.svelte.
  import { getLookPref, getThemePref, LOOKS, setLookPref, setThemePref, type LookPref, type ThemePref } from '../../theme';
  import { calendarPref, setCalendarPref, type CalendarPref } from '../../format/dates';

  let theme = $state<ThemePref>(getThemePref());

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
  <p class="help">Every look has a light and a dark version. Light or dark chooses which.</p>
</section>

<section class="section">
  <h4 id="appearance-heading">Light or dark</h4>
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

<style>
  .section { margin-bottom: var(--space-4); }
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
</style>
