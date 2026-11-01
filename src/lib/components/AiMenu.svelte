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
  import { askModalOpen, extractModalOpen } from '../stores/ui';
  import {
    currentAccount, capabilitiesByAccount, canWrite, selectedFolder,
    selectedSmartFolder, selectedTags, searchQuery,
  } from '../stores/notes';
  import { hiddenFromAi, aiHiddenFor } from '../stores/aiScope';
  import { isAndroid, isMacos } from '../stores/platform';
  import { studioRequest } from '../studio';
  import { isRealFolderSelection } from './askScope';
  import { aiGate } from '../controllers/aiActions';
  import { shortcutModLabel } from '../editor/shortcutKeys';
  import { viewName } from '../viewName';

  let {
    run,
    onorganize,
    iconSize = 16,
  }: {
    /** Close the menu, then do `action`. */
    run: (action: () => void) => void;
    /** Open Organize for an account. The Sidebar owns CurateReview, so only
     *  its popover passes this; without it there is no Organize row. */
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

  // The global hotkey (App.svelte) — shown on desktop only.
  const makeNoteShortcut = $derived($isAndroid ? '' : `${shortcutModLabel($isMacos)}+Shift+L`);

  function ask() {
    run(() => askModalOpen.set(true));
  }
  function makeNote() {
    run(() => extractModalOpen.set(true));
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

<button class="item" type="button" role="menuitem" onclick={ask} title="Ask Jodd about your notes">
  <span class="icon"><Icon name="chat" size={iconSize} /></span>
  <span class="label">Ask Jodd…</span>
</button>
{#if makeNoteGate.show}
  <button class="item" type="button" role="menuitem" onclick={makeNote}
    title="Make a note from pasted text, links or a note">
    <span class="icon"><Icon name="sparkle" size={iconSize} /></span>
    <span class="label">Make a note with AI…</span>
    {#if makeNoteShortcut}<span class="hint">{makeNoteShortcut}</span>{/if}
  </button>
{/if}
{#if studioGate.show}
  <button class="item" type="button" role="menuitem" onclick={studio} disabled={studioGate.disabled}
    title={studioGate.reason || 'Generate a briefing, FAQ, timeline and more from this folder’s notes'}>
    <span class="icon"><Icon name="sparkle" size={iconSize} /></span>
    <span class="label">Studio…</span>
    {#if studioFolder && !studioGate.disabled}<span class="hint">{viewName(null, studioFolder, '')}</span>{/if}
  </button>
  {#if studioGate.reason}<p class="menu-reason">{studioGate.reason}</p>{/if}
{/if}
{#if onorganize && !$isAndroid && account}
  <button class="item" type="button" role="menuitem" onclick={organize}
    title="Find duplicate notes, unfiled notes and secrets, and fix them">
    <span class="icon"><Icon name="check" size={iconSize} /></span>
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
