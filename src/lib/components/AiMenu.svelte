<script lang="ts">
  // The AI menu's list (spec 2026-10-28 §5.1): one list on every layout. Two
  // thin wrappers present it — the Sidebar footer's ✦ popover on desktop and
  // tablet, and AiActionsSheet on phone — and each passes `run`, which closes
  // the wrapper its own way before the chosen action opens its dialog.
  //
  // Each row asks controllers/aiActions.ts whether it shows and whether it
  // can be used, like every other AI entry point. A disabled row says why on
  // a visible line (touch has no hover), not only in its tooltip.
  import Icon from './Icon.svelte';
  import { askModalOpen, extractModalOpen, settingsRoute } from '../stores/ui';
  import { extractPrefill } from '../capture';
  import { tvs } from '../tvs';
  import {
    currentAccount, capabilitiesByAccount, canWrite, selectedFolder,
    selectedSmartFolder, selectedTags, searchQuery,
  } from '../stores/notes';
  import { hiddenFromAi, aiHiddenFor } from '../stores/aiScope';
  import { isAndroid, isMacos } from '../stores/platform';
  import { studioRequest } from '../studio';
  import { isRealFolderSelection } from './askScope';
  import { aiGate } from '../controllers/aiActions';
  import { hotkeyLabel } from '../appHotkeys';
  import { viewName } from '../viewName';

  let {
    run,
    onorganize,
    iconSize = 16,
  }: {
    /** Close the menu, then do `action`. */
    run: (action: () => void) => void;
    /** Open Organize for an account. The Sidebar passes one that sets
     *  stores/ui.ts organizeAccount, and OrganizeHost (App.svelte) mounts
     *  CurateReview app-wide; without it there is no Organize row. */
    onorganize?: (accountId: string) => void;
    iconSize?: number;
  } = $props();

  const account = $derived($currentAccount);
  const writesNotes = $derived(!!account && canWrite($capabilitiesByAccount[account], 'notes'));

  // Studio runs on a folder: the one the list shows, when the list shows a
  // folder — not All, Recently Deleted, a Smart Folder, a tag or a search.
  const studioFolder = $derived(
    isRealFolderSelection($selectedFolder) && !$selectedSmartFolder && $selectedTags.size === 0 && !$searchQuery.trim()
      ? $selectedFolder
      : null,
  );

  const makeNoteGate = $derived(aiGate({ canWrite: writesNotes, hidden: false }));
  const studioGate = $derived(aiGate({
    canWrite: writesNotes,
    hidden: !!studioFolder && aiHiddenFor($hiddenFromAi, account, studioFolder),
    hasFolder: !!studioFolder,
  }));

  const showOrganize = $derived(!!onorganize && !!account);
  const hasTv = $derived($tvs.length > 0);

  // The global hotkeys (appHotkeys.ts, App.svelte) — shown on desktop only.
  const makeNoteShortcut = $derived($isAndroid ? '' : hotkeyLabel('makeNote', $isMacos));
  const askShortcut = $derived($isAndroid ? '' : hotkeyLabel('ask', $isMacos));

  function ask() {
    run(() => askModalOpen.set(true));
  }
  function makeNote() {
    run(() => extractModalOpen.set(true));
  }
  // The TV list is a store loaded at startup (no IPC here). With no TV linked
  // the item is a doorway to where one is linked, not a dead end.
  function whatsOnMyTv() {
    if (!hasTv) {
      run(() => settingsRoute.set('share'));
      return;
    }
    run(() => {
      extractPrefill.set({ kind: 'tv' });
      extractModalOpen.set(true);
    });
  }
  function studio() {
    const acct = account;
    const label = studioFolder;
    if (!acct || !label) return;
    run(() => studioRequest.set({ accountId: acct, label }));
  }
  function organize() {
    const acct = account;
    if (!acct || !onorganize) return;
    const open = onorganize;
    run(() => open(acct));
  }
</script>

<!-- Three groups, by what the row does: ask about notes; make a note (from
     what you paste or link, from the TV, or from a folder's notes); tidy up.
     A rule between groups, only where both sides have a row. -->
<button class="item" type="button" role="menuitem" onclick={ask} title="Ask Jodd about your notes">
  <span class="icon"><Icon name="chat" size={iconSize} /></span>
  <span class="label">Ask Jodd…</span>
  {#if askShortcut}<span class="hint">{askShortcut}</span>{/if}
</button>
{#if makeNoteGate.show || studioGate.show}
  <div class="sep" role="separator"></div>
{/if}
{#if makeNoteGate.show}
  <button class="item" type="button" role="menuitem" onclick={makeNote}
    title="Make a note from pasted text, links or a note">
    <span class="icon"><Icon name="sparkle" size={iconSize} /></span>
    <span class="label">Make a note with AI…</span>
    {#if makeNoteShortcut}<span class="hint">{makeNoteShortcut}</span>{/if}
  </button>
  <!-- With no TV linked the row says what it does instead: it opens
       Settings › Share to Jodd, where a TV is linked, rather than a dialog
       that could only fail. -->
  <button class="item" type="button" role="menuitem" onclick={whatsOnMyTv}
    title={hasTv ? 'Make a note from the YouTube video playing on your TV' : 'Link a TV in Settings › Share to Jodd, then make notes from what it plays'}>
    <span class="icon"><Icon name="tv" size={iconSize} /></span>
    <span class="label">{hasTv ? 'What’s on my TV…' : 'Link a TV…'}</span>
  </button>
{/if}
{#if studioGate.show}
  <!-- The folder Studio reads is said on a line under the name, never in the
       right-hand column: that column holds shortcuts, and a folder called
       "VDO" there read as one. The line is cut short with an ellipsis, so
       the tooltip carries the whole folder name. -->
  <button class="item" type="button" role="menuitem" onclick={studio} disabled={studioGate.disabled}
    title={studioGate.reason || (studioFolder
      ? `Generate a briefing, FAQ, timeline and more from the notes in ${viewName(null, studioFolder, '')}`
      : 'Generate a briefing, FAQ, timeline and more from this folder’s notes')}>
    <span class="icon"><Icon name="layers" size={iconSize} /></span>
    <span class="text">
      <span class="label">Studio…</span>
      {#if studioFolder && !studioGate.disabled}<span class="sub">from {viewName(null, studioFolder, '')}</span>{/if}
    </span>
  </button>
  {#if studioGate.reason}<p class="menu-reason">{studioGate.reason}</p>{/if}
{/if}
{#if showOrganize}
  <div class="sep" role="separator"></div>
  <!-- Organize's own words (spec 2026-10-28 §16): "overlapping notes", not
       "duplicates", which are the extra Gmail copies; "suggest folders", as
       the note menu's Suggest a folder. -->
  <button class="item" type="button" role="menuitem" onclick={organize}
    title="Find overlapping notes, suggest folders, and find secrets">
    <span class="icon"><Icon name="broom" size={iconSize} /></span>
    <span class="label">Organize…</span>
  </button>
{/if}

<style>
  /* The anchored popover's rows. Inside a phone sheet, src/styles/
     sheet-menu.css styles them instead (its selectors outrank these). */
  .item {
    display: flex;
    align-items: center;
    gap: calc(var(--space-2) + var(--space-0-5));
    width: 100%;
    min-width: 220px;
    padding: var(--space-1-5) var(--space-3);
    background: none;
    border: none;
    border-radius: var(--radius);
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .item:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .item:disabled {
    cursor: default;
    opacity: 0.4;
  }
  .icon {
    display: inline-flex;
    flex-shrink: 0;
    color: var(--accent-action);
  }
  .label {
    flex: 1;
    white-space: nowrap;
  }
  /* A row with a second line (Studio's folder): the name, then the line. */
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  /* Capped, or a long folder name would widen every row of the popover
     (it sizes to its content). The tooltip has the whole name. */
  .sub {
    max-width: 28ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--size-xs);
  }
  /* Between groups. Inside a phone sheet, sheet-menu.css's .sep wins. */
  .sep {
    height: 1px;
    margin: var(--space-1) var(--space-2);
    background: var(--surface-active);
  }
  .hint {
    margin-left: auto;
    padding-left: var(--space-3);
    max-width: 14ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--size-xs);
  }
</style>
