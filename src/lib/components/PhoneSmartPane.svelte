<script lang="ts">
  // The phone's Smart folders pane (spec 2026-10-20 §5.2): one account's
  // Smart Folders, between Folders and the list. The account is the one whose
  // "Smart folders" row was tapped (phonePaneAccount), because Smart Folders
  // are per account in today's stores.
  import PaneHeader from './PaneHeader.svelte';
  import Icon from './Icon.svelte';
  import { phonePaneAccount } from '../stores/phonePanes';
  import { backLabel, navigateToPane } from '../stores/phoneNav';
  import { activeAccounts, currentAccount, type SmartFolderKind } from '../stores/notes';
  import { selectSmartFolder } from '../controllers/viewSelection';
  import { accountShortName } from '../stores/listView';
  import { SMART_FOLDER_NAMES } from '../viewName';
  import { SMART_FOLDER_ROWS, SMART_FOLDER_KINDS } from '../smartFolders';

  const accountId = $derived($phonePaneAccount ?? $currentAccount ?? '');
  const account = $derived($activeAccounts.find((a) => a.id === accountId));
  // Which account, only when there is more than one to tell apart.
  const subtitle = $derived(
    account && $activeAccounts.length > 1 ? accountShortName(account, $activeAccounts) : undefined,
  );

  // The Sidebar's Views pick (controllers/viewSelection.ts), then the list
  // push. The push is made here rather than left to App's Smart Folder
  // watcher: that watcher is keyed on a CHANGE of the value, so re-tapping
  // the Smart Folder already shown would otherwise do nothing (ruling R21).
  // On a change the watcher's own push lands on the list this one opened and
  // only renames it.
  function pick(kind: SmartFolderKind) {
    if (!accountId) return;
    selectSmartFolder(accountId, kind);
    navigateToPane('list', SMART_FOLDER_NAMES[kind]);
  }
</script>

<div class="phone-pane">
  <PaneHeader title="Smart folders" {subtitle} back={$backLabel} />
  <div class="rows">
    {#each SMART_FOLDER_KINDS as kind (kind)}
      <button
        type="button"
        class="smart-row"
        data-smart-folder={kind}
        title={SMART_FOLDER_ROWS[kind].title}
        onclick={() => pick(kind)}
      >
        <span class="row-icon"><Icon name={SMART_FOLDER_ROWS[kind].icon} size={20} /></span>
        <span class="row-name">{SMART_FOLDER_NAMES[kind]}</span>
        <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
      </button>
    {/each}
  </div>
</div>

<style>
  .phone-pane {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    background: var(--surface-sidebar);
  }
  .rows {
    flex: 1;
    overflow: auto;
    padding: var(--space-1) 0 var(--space-4);
  }
  .smart-row {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    width: 100%;
    min-height: var(--touch-target);
    padding: 0 var(--space-4);
    background: none;
    border: none;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .smart-row:hover {
    background: var(--surface-hover);
  }
  .row-icon {
    display: inline-flex;
    color: var(--accent-action);
  }
  .row-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .row-chevron {
    display: inline-flex;
    color: var(--text-disabled);
  }
</style>
