<script lang="ts">
  // Settings › General: Appearance, Date display, Ask Jodd history. Every
  // control here applies at once, so the page has no forms and no Save.
  // Appearance and Ask Jodd moved verbatim from the old AppSettings.svelte.
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  import { getThemePref, setThemePref, type ThemePref } from '../../theme';
  import { calendarPref, setCalendarPref, type CalendarPref } from '../../format/dates';
  import { aboutModalOpen } from '../../stores/ui';

  let theme = $state<ThemePref>(getThemePref());

  function chooseTheme(next: ThemePref) {
    theme = next;
    setThemePref(next);
  }

  const CALENDARS: [CalendarPref, string][] = [
    ['auto', 'Automatic'],
    ['gregory', 'Gregorian'],
    ['buddhist', 'Buddhist (BE)'],
  ];

  let askHistoryEnabled = $state(true);
  let askHistoryPrompt = $state(false);
  let askHistoryError = $state('');

  async function loadAskHistory() {
    // A prompt or error left from the last time the page was open describes
    // a switch the user may since have flipped elsewhere.
    askHistoryPrompt = false;
    askHistoryError = '';
    try { askHistoryEnabled = (await invoke<boolean | null>('get_ask_history_enabled')) !== false; }
    catch (e) { askHistoryError = String(e); }
  }

  async function toggleAskHistory() {
    const next = !askHistoryEnabled;
    askHistoryEnabled = next; // optimistic
    askHistoryError = '';
    try {
      await invoke('set_ask_history_enabled', { enabled: next });
      askHistoryPrompt = !next;
    } catch (e) {
      askHistoryEnabled = !next; // rollback
      askHistoryError = String(e);
    }
  }

  async function clearAskHistory() {
    askHistoryPrompt = false;
    try { await invoke('clear_ask_history'); }
    catch (e) { askHistoryError = `Could not delete Ask history: ${e}`; }
  }

  onMount(() => { void loadAskHistory(); });
</script>

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

<section class="section">
  <h4>Ask Jodd</h4>
  <label class="toggle-row">
    <input type="checkbox" checked={askHistoryEnabled} onchange={toggleAskHistory} />
    <span>Keep Ask Jodd history</span>
  </label>
  <p class="help">Saved on this device only; never synced to Gmail, iCloud or Apple Notes.</p>
  <details class="learn-more">
    <summary>Learn more</summary>
    <p>Saves your questions and answers so you can reopen and continue them.</p>
  </details>
  <!-- `!askHistoryEnabled` too: re-checking the box while the switch-off is
       still saving must take the Delete offer away at once, not when the
       second save lands. -->
  {#if askHistoryPrompt && !askHistoryEnabled}
    <div class="actions">
      <span>Also delete existing history?</span>
      <button class="link-btn" onclick={() => (askHistoryPrompt = false)}>Keep history</button>
      <button class="link-btn danger" onclick={clearAskHistory}>Delete history</button>
    </div>
  {/if}
  {#if askHistoryError}<p class="error">{askHistoryError}</p>{/if}
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
  h4 { margin: 0 0 var(--space-1-5); font-size: var(--size-md); font-weight: 600; }
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
    font-weight: 600;
  }
  .toggle-row {
    display: flex; align-items: center; gap: var(--space-2);
    font-size: var(--size); color: var(--text); margin-bottom: var(--space-1-5); cursor: pointer;
  }
  .toggle-row input { margin: 0; }
  .actions { display: flex; flex-wrap: wrap; gap: var(--space-3); margin-top: var(--space-0-5); font-size: var(--size-sm); }
  .link-btn {
    background: none; border: none; color: var(--accent-action); cursor: pointer;
    padding: 0; font-size: var(--size-sm); text-decoration: underline;
  }
  .link-btn.danger { color: var(--danger); }
  .error { color: var(--danger); font-size: var(--size-sm); margin: var(--space-1) 0 0; }
</style>
