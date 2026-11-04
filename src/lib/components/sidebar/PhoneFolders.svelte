<script lang="ts">
  // The phone's own Folders pane (spec 2026-10-20 §5.2). The Sidebar renders
  // it instead of its desktop/tablet markup when the layout store says phone
  // ($isAndroid, then 'phone'), so it is the same Sidebar reading the layout
  // store, not a prop fork. Per account: a tappable label (it opens "All
  // <account>"), the user folder tree, then Tags / Smart folders / Recently
  // deleted rows; below them the bottom bar (§5.1): New folder · AI ✦ · New
  // note.
  //
  // What the desktop Sidebar shows and this does not goes elsewhere: the
  // Agents group to the AI ✦ sheet, the Views to the Smart folders pane,
  // the tag cloud to the Tags pane, the dup pill and the footer (accounts,
  // Ask Jodd, App settings, About) to Settings and ✦.
  //
  // The folder tree's state (which paths are expanded), the folder sheet and
  // folder creation stay the Sidebar's — the tablet layout uses them too —
  // and come in as props and callbacks.
  import Icon from '../Icon.svelte';
  import PhoneBar from '../PhoneBar.svelte';
  import AiActionsSheet, { type WorkflowEntry } from '../AiActionsSheet.svelte';
  import { longpress } from '../../longpress';
  import { type FolderRow, isRowVisible, rowHasChildren, stripWorkflowMarkers } from './folderTree';
  import {
    activeAccounts, currentAccount, selectedFolder, selectedSmartFolder, selectedTags, searchQuery,
    tagsByAccount, capabilitiesByAccount, shouldShowTrash, canWrite, accountDisplay, backendLabel,
  } from '../../stores/notes';
  import { accountShortName, currentListTitle } from '../../stores/listView';
  import { phonePaneAccount } from '../../stores/phonePanes';
  import { navigateToPane } from '../../stores/phoneNav';
  import { selectFolder, openNewNote } from '../../controllers/viewSelection';
  import { folderBlockReason, syncBlocks } from '../../syncBlocks';
  import { viewName } from '../../viewName';
  import type { Account } from '../../types';

  let {
    split,
    totals,
    expandedPaths,
    ontoggle,
    onfoldermenu,
    onaccountmenu,
    onnewfolder,
  }: {
    /** Per account: the user folder rows and the workflow folder rows. */
    split: Record<string, { user: FolderRow[]; workflow: FolderRow[] }>;
    /** Per account: every note in it — desktop's "All <account>" count. */
    totals: Record<string, number>;
    expandedPaths: Record<string, boolean>;
    ontoggle: (e: MouseEvent | KeyboardEvent, path: string) => void;
    /** Long-press / right-click on a folder row: the Sidebar's folder sheet. */
    onfoldermenu: (e: MouseEvent, accountId: string, path: string) => void;
    /** Long-press / right-click on an account label: the Sidebar's account sheet. */
    onaccountmenu: (e: MouseEvent, accountId: string) => void;
    /** New top-level folder in `accountId`; `title` names the prompt. */
    onnewfolder: (accountId: string, title?: string) => void;
  } = $props();

  let aiSheetOpen = $state(false);

  /** "a · Gmail": the account's short name and its backend. */
  function accountLabel(acct: Account, active: Account[]): string {
    const short = accountShortName(acct, active);
    const backend = backendLabel(acct.backend_kind ?? 'gmail');
    // A clash already widens the short name to "address · Backend".
    return short.endsWith(` · ${backend}`) ? short : `${short} · ${backend}`;
  }

  // A folder pick, then the list push. The push is made here, not left to
  // App's $selectedFolder watcher: that watcher only fires on a CHANGE, so
  // re-tapping the folder already shown would do nothing (ruling R21). The
  // title names the list for the Back label of a note opened from it (§5.1).
  function pickFolder(accountId: string, path: string) {
    selectFolder(accountId, path);
    navigateToPane('list', currentListTitle());
  }

  // Tags and Smart folders are per account in today's stores, so each
  // account block drills into its own pane; the pane reads which account
  // from phonePaneAccount.
  function openPane(accountId: string, pane: 'tags' | 'smart') {
    phonePaneAccount.set(accountId);
    navigateToPane(pane, pane === 'tags' ? 'Tags' : 'Smart folders');
  }

  // The Agents group's rows (agent folders), for the AI ✦ sheet.
  const workflows = $derived(
    $activeAccounts.flatMap((a): WorkflowEntry[] =>
      (split[a.id]?.workflow ?? []).map((r) => ({
        accountId: a.id,
        path: r.path,
        name: stripWorkflowMarkers(r.name),
        depth: r.depth,
        account: $activeAccounts.length > 1 ? accountShortName(a, $activeAccounts) : '',
      })),
    ),
  );

  const canWriteFolders = $derived(
    canWrite($currentAccount ? $capabilitiesByAccount[$currentAccount] : undefined, 'folders'),
  );
  const canWriteNotes = $derived(
    canWrite($currentAccount ? $capabilitiesByAccount[$currentAccount] : undefined, 'notes'),
  );

  // New folder goes in the last-viewed account; with more than one active,
  // the prompt says which.
  function newFolder() {
    const accountId = $currentAccount;
    if (!accountId) return;
    const acct = $activeAccounts.find((a) => a.id === accountId);
    const title = acct && $activeAccounts.length > 1
      ? `New folder in ${accountShortName(acct, $activeAccounts)}`
      : undefined;
    onnewfolder(accountId, title);
  }

  // New note (§5.1) goes in the folder the user last viewed — the Notes root
  // when that view was not a real folder (All, Recently Deleted, a Smart
  // Folder or a tag filter), the same rule as NoteList's + — and Back from it
  // lands on that folder's list: push the list (titled), then the selection
  // (App's note watcher) pushes the note above it.
  function newNote() {
    const accountId = $currentAccount;
    if (!accountId || !canWriteNotes) return;
    const f = $selectedFolder;
    const filtered = !!$selectedSmartFolder || $selectedTags.size > 0;
    const path = filtered || !f || f === '__ALL__' || f === '__TRASH__' ? 'Notes' : f;
    if ($searchQuery) searchQuery.set('');
    navigateToPane('list', viewName(null, path, ''));
    openNewNote(accountId, path);
  }
</script>

<aside class="phone-folders">
  <nav class="folder-list">
    {#each $activeAccounts as acct (acct.id)}
      <section class="account-section" data-account-id={acct.id}>
        <button
          type="button"
          class="phone-account-label"
          title="All {accountDisplay(acct)}"
          onclick={() => pickFolder(acct.id, '__ALL__')}
          oncontextmenu={(e) => onaccountmenu(e, acct.id)}
          use:longpress
        >
          <span class="phone-account-name">{accountLabel(acct, $activeAccounts)}</span>
          <!-- The label opens "All <account>", so it carries that row's count. -->
          <span class="folder-count">{totals[acct.id] ?? 0}</span>
        </button>
        {#each split[acct.id]?.user ?? [] as row (row.path)}
          {#if isRowVisible(row.path, expandedPaths)}
            <div
              class="folder-item"
              class:active={!$selectedSmartFolder && $selectedTags.size === 0 && $selectedFolder === row.path && $currentAccount === acct.id}
              style="padding-left: calc(var(--space-1) + {row.depth} * var(--space-4))"
              role="button"
              tabindex="0"
              onclick={() => pickFolder(acct.id, row.path)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pickFolder(acct.id, row.path); } }}
              oncontextmenu={(e) => onfoldermenu(e, acct.id, row.path)}
              use:longpress
            >
              {#if rowHasChildren(split[acct.id].user, row.path)}
                <span
                  class="expand-toggle"
                  role="button"
                  tabindex="-1"
                  aria-label={expandedPaths[row.path] ? 'Collapse' : 'Expand'}
                  onclick={(e) => ontoggle(e, row.path)}
                  onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') ontoggle(e, row.path); }}
                ><Icon name={expandedPaths[row.path] ? 'chevron-down' : 'chevron-right'} size={16} /></span>
              {:else}
                <span class="expand-toggle"></span>
              {/if}
              <span class="folder-icon"><Icon name="folder" size={20} /></span>
              <span class="folder-name" title={stripWorkflowMarkers(row.name)}>{stripWorkflowMarkers(row.name)}</span>
              {#if folderBlockReason($syncBlocks, acct.id, row.path)}
                <!-- Refused permanently (gotcha #14); the reason and Try again
                     are in NoteList's banner when the folder is open. -->
                <span
                  class="folder-blocked-indicator"
                  role="img"
                  aria-label="Folder not synced"
                  title={`Not synced — ${folderBlockReason($syncBlocks, acct.id, row.path)}`}>!</span>
              {/if}
              <span class="folder-count">{row.count}</span>
            </div>
          {/if}
        {/each}
        <button type="button" class="phone-nav-row" data-row="tags" onclick={() => openPane(acct.id, 'tags')}>
          <span class="folder-icon"><Icon name="tag" size={20} /></span>
          <span class="folder-name">Tags</span>
          <span class="folder-count">{($tagsByAccount.get(acct.id) ?? []).length}</span>
          <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
        </button>
        <button type="button" class="phone-nav-row" data-row="smart" onclick={() => openPane(acct.id, 'smart')}>
          <span class="folder-icon"><Icon name="eye" size={20} /></span>
          <span class="folder-name">Smart folders</span>
          <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
        </button>
        {#if shouldShowTrash($capabilitiesByAccount[acct.id])}
          <button type="button" class="phone-nav-row" data-row="trash" onclick={() => pickFolder(acct.id, '__TRASH__')}>
            <span class="folder-icon"><Icon name="trash" size={20} /></span>
            <span class="folder-name">Recently deleted</span>
            <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
          </button>
        {/if}
      </section>
    {/each}
  </nav>

  <PhoneBar label="Folder actions">
    <button
      type="button"
      aria-label="New folder"
      title={canWriteFolders ? 'New folder' : "This account can't accept folder changes in Jodd yet"}
      disabled={!$currentAccount || !canWriteFolders}
      onclick={newFolder}
    ><Icon name="folder-plus" size={22} /></button>
    <button
      type="button"
      aria-label="AI actions"
      title="Ask Jodd, make a note with AI, and agent folders"
      onclick={() => (aiSheetOpen = true)}
    ><Icon name="sparkle" size={22} /></button>
    <button
      type="button"
      aria-label="New note"
      title="New note"
      disabled={!$currentAccount || !canWriteNotes}
      onclick={newNote}
    ><Icon name="note-plus" size={22} /></button>
  </PhoneBar>
</aside>

<!-- Unmounts with this pane (rotation off the phone, or another pane), so a
     sheet left open never comes back on its own. -->
{#if aiSheetOpen}
  <AiActionsSheet {workflows} onpick={pickFolder} onclose={() => (aiSheetOpen = false)} />
{/if}

<style>
  /* ─── Phone Folders screen (spec 2026-10-20 §5.2) ───────────────────────
     Full width under App's PaneHeader, rows at touch size, the bar
     (PhoneBar.svelte) at the bottom. Tokens only. */
  .phone-folders {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    width: 100%;
    overflow: hidden;
    background: var(--surface-sidebar);
  }
  .folder-list {
    flex: 1;
    overflow: auto;
    padding: var(--space-1) 0 var(--space-4);
  }
  .account-section + .account-section {
    margin-top: var(--space-4);
  }
  .phone-account-label {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    box-sizing: border-box;
    width: 100%;
    min-height: var(--touch-target);
    padding: 0 var(--space-4);
    background: none;
    border: none;
    text-align: left;
    font: inherit;
    font-size: var(--size-sm);
    font-weight: 600;
    line-height: var(--touch-target);
    color: var(--text-secondary);
    cursor: pointer;
  }
  /* A long address truncates; the count pill keeps its place at the end,
     lined up with the folder rows' counts (same right padding). */
  .phone-account-name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .phone-account-label .folder-count {
    flex-shrink: 0;
    font-weight: normal;
    line-height: var(--leading);
  }
  .folder-item,
  .phone-nav-row {
    display: flex;
    align-items: center;
    box-sizing: border-box;
    width: 100%;
    min-height: var(--touch-target);
    margin: 0;
    gap: var(--space-2);
    padding-top: 0;
    padding-bottom: 0;
    padding-right: var(--space-4);
    background: none;
    border: none;
    border-radius: 0;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .folder-item:hover,
  .phone-nav-row:hover {
    background: var(--surface-hover);
  }
  .folder-item.active {
    background: var(--surface-active);
  }
  .phone-nav-row {
    /* Lines the icon up with the folder rows' icons, past their toggle. */
    padding-left: calc(var(--space-1) + var(--touch-target) + var(--space-2));
  }
  .expand-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: var(--touch-target);
    height: var(--touch-target);
    border-radius: var(--radius);
    color: var(--text-muted);
    user-select: none;
  }
  /* The same hover as the Sidebar's toggle; the empty spacer toggle on a
     leaf folder has no role and stays inert. */
  .expand-toggle[role='button']:hover {
    background: var(--surface-active);
    color: var(--text);
  }
  .folder-icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    color: var(--accent-action);
  }
  .folder-name {
    flex: 1;
    min-width: 0;
    font-size: var(--size);
    line-height: var(--leading);
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .folder-blocked-indicator {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    /* The Sidebar's badge: 14px, set off from the name by --space-1. */
    width: 14px;
    height: 14px;
    margin-left: var(--space-1);
    border-radius: 50%;
    background: var(--danger-wash);
    color: var(--danger);
    font-size: var(--size-xs);
    font-weight: 700;
  }
  .folder-count {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    background: var(--surface-badge);
    padding: var(--space-0-5) var(--space-1-5);
    /* A pill, as in the Sidebar (10px). */
    border-radius: var(--radius-lg);
  }
  .row-chevron {
    display: inline-flex;
    align-items: center;
    color: var(--text-disabled);
  }
</style>
