<script lang="ts">
  // The phone's own Folders pane (spec 2026-10-20 §5.2). The Sidebar renders
  // it instead of its desktop/tablet markup when the layout store says phone
  // ($isAndroid, then 'phone'), so it is the same Sidebar reading the layout
  // store, not a prop fork. At the top, once, the Views for the current
  // account: Tags, Smart views and Recently Deleted (spec 2026-11-10 §6, the
  // desktop order). Then per account: a tappable label (it opens "All
  // <account>") with the account's state under it, and the user folder tree.
  // Below them the bottom bar (§5.1): New folder · AI ✦ · New note.
  //
  // What the desktop Sidebar shows and this does not goes elsewhere: the
  // Agents group to the AI ✦ sheet, the Views to the Smart views pane,
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
  import { navRows } from '../../navRows';
  import { isSignedOut, reconnectKind } from '../../controllers/authLoss';
  import { startAddAccount } from '../../controllers/accountActions';
  import { type FolderRow, isRowVisible, rowHasChildren, stripWorkflowMarkers } from './folderTree';
  import {
    activeAccounts, currentAccount, selectedFolder, selectedSmartFolder, selectedTags, searchQuery,
    tagsByAccount, capabilitiesByAccount, shouldShowTrash, canWrite, accountDisplay, backendLabel,
  } from '../../stores/notes';
  import { accountShortName, accountNameWithBackend, currentListTitle } from '../../stores/listView';
  import { phonePaneAccount } from '../../stores/phonePanes';
  import { navigateToPane } from '../../stores/phoneNav';
  import { selectFolder, openNewNote } from '../../controllers/viewSelection';
  import { folderBlockReason, syncBlocks } from '../../syncBlocks';
  import { viewName } from '../../viewName';
  import { loadViewsOpen, saveViewsOpen } from '../../viewsFold';
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

  /** "a · Gmail": the account's short name and its backend, as on desktop. */
  function accountLabel(acct: Account, active: Account[]): string {
    return accountNameWithBackend(acct, active);
  }

  // A folder pick, then the list push. The push is made here, not left to
  // App's $selectedFolder watcher: that watcher only fires on a CHANGE, so
  // re-tapping the folder already shown would do nothing (ruling R21). The
  // title names the list for the Back label of a note opened from it (§5.1).
  function pickFolder(accountId: string, path: string) {
    selectFolder(accountId, path);
    navigateToPane('list', currentListTitle());
  }

  // The Views act on the current account: the account of the last thing
  // picked, as on desktop (spec 2026-11-10 §1). The panes read which account
  // from phonePaneAccount.
  const currentAcct = $derived($activeAccounts.find((a) => a.id === $currentAccount) ?? $activeAccounts[0]);
  const multi = $derived($activeAccounts.length > 1);
  // Views fold away, as on desktop (0.47.0): the same stored choice
  // (lib/viewsFold.ts), so folding them on one layout folds them on the other.
  let viewsOpen = $state(loadViewsOpen());
  function toggleViews() {
    viewsOpen = !viewsOpen;
    saveViewsOpen(viewsOpen);
  }

  function openPane(accountId: string, pane: 'tags' | 'smart') {
    phonePaneAccount.set(accountId);
    navigateToPane(pane, pane === 'tags' ? 'Tags' : 'Smart views');
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
      ? `New folder in ${accountNameWithBackend(acct, $activeAccounts)}`
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
  <nav class="folder-list" use:navRows aria-label="Folders">
    {#if currentAcct}
      <!-- Once, for the current account (spec 2026-11-10 §6). With more than
           one account the label names which, so the scope is never a memory
           task. -->
      <section class="phone-views" data-scope-account={currentAcct.id} aria-label="Views">
        <p class="phone-section-label">
          <button type="button" class="phone-views-toggle" aria-expanded={viewsOpen} onclick={toggleViews}>
            <span class="phone-views-caret"><Icon name={viewsOpen ? 'chevron-down' : 'chevron-right'} size={14} /></span>
            Views{#if multi}<span class="phone-section-scope"> · {accountNameWithBackend(currentAcct, $activeAccounts)}</span>{/if}
          </button>
        </p>
        {#if viewsOpen}
        <button type="button" class="phone-nav-row" data-row="tags" onclick={() => openPane(currentAcct.id, 'tags')}>
          <span class="folder-icon"><Icon name="tag" size={20} /></span>
          <span class="folder-name">Tags</span>
          <span class="folder-count">{($tagsByAccount.get(currentAcct.id) ?? []).length}</span>
          <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
        </button>
        <button type="button" class="phone-nav-row" data-row="smart" onclick={() => openPane(currentAcct.id, 'smart')}>
          <span class="folder-icon"><Icon name="eye" size={20} /></span>
          <span class="folder-name">Smart views</span>
          <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
        </button>
        {#if shouldShowTrash($capabilitiesByAccount[currentAcct.id])}
          <button
            type="button"
            class="phone-nav-row"
            class:active={$selectedFolder === '__TRASH__' && !$selectedSmartFolder && $selectedTags.size === 0}
            data-row="trash"
            onclick={() => pickFolder(currentAcct.id, '__TRASH__')}
          >
            <span class="folder-icon"><Icon name="trash" size={20} /></span>
            <span class="folder-name">Recently Deleted</span>
            <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
          </button>
        {/if}
        {/if}
      </section>
    {/if}
    {#each $activeAccounts as acct (acct.id)}
      {@const isCurrent = acct.id === $currentAccount}
      {@const allOpen = isCurrent && $selectedFolder === '__ALL__' && !$selectedSmartFolder && $selectedTags.size === 0}
      {@const reconnect = reconnectKind(acct)}
      <section class="account-section" class:current={isCurrent} data-account-id={acct.id} aria-label={accountLabel(acct, $activeAccounts)}>
        <button
          type="button"
          class="phone-account-label"
          class:active={allOpen}
          aria-current={allOpen ? 'true' : undefined}
          title="All {accountDisplay(acct)}"
          onclick={() => pickFolder(acct.id, '__ALL__')}
          oncontextmenu={(e) => onaccountmenu(e, acct.id)}
          use:longpress
        >
          <span class="phone-account-name">{accountLabel(acct, $activeAccounts)}</span>
          {#if isCurrent && multi}<span class="sr-only">, current account</span>{/if}
          {#if acct.blocked_reason}
            <!-- The phone polls nothing (spec 2026-11-10 §3): signed out and
                 blocked come from the account itself. -->
            <span class="account-mark" role="img" aria-label="Not syncing" title={acct.blocked_reason}></span>
          {/if}
          <!-- The label opens "All <account>", so it carries that row's count. -->
          <span class="folder-count">{totals[acct.id] ?? 0}</span>
        </button>
        {#if acct.blocked_reason}
          <div class="account-notice" role="status">
            <span class="notice-text">{isSignedOut(acct) ? 'Signed out. Its notes and unsent changes are kept here.' : acct.blocked_reason}</span>
            {#if reconnect}
              <button type="button" class="notice-action" onclick={() => startAddAccount(reconnect)}>Sign in again</button>
            {/if}
          </div>
        {/if}
        {#each split[acct.id]?.user ?? [] as row (row.path)}
          {#if isRowVisible(row.path, expandedPaths)}
            {@const hasKids = rowHasChildren(split[acct.id].user, row.path)}
            {@const isOpenRow = !$selectedSmartFolder && $selectedTags.size === 0 && $selectedFolder === row.path && isCurrent}
            {@const blockReason = folderBlockReason($syncBlocks, acct.id, row.path)}
            <div
              class="folder-item"
              class:active={isOpenRow}
              aria-current={isOpenRow ? 'true' : undefined}
              aria-expanded={hasKids ? !!expandedPaths[row.path] : undefined}
              data-depth={row.depth}
              style="--depth: {row.depth}"
              role="button"
              tabindex="-1"
              onclick={() => pickFolder(acct.id, row.path)}
              onkeydown={(e) => { if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); pickFolder(acct.id, row.path); } }}
              oncontextmenu={(e) => onfoldermenu(e, acct.id, row.path)}
              use:longpress
            >
              {#if hasKids}
                <!-- Reachable by touch and by a screen reader's swipe; from a
                     keyboard, → and ← on the row do the same (navRows). -->
                <span
                  class="expand-toggle"
                  role="button"
                  tabindex="-1"
                  aria-label="{expandedPaths[row.path] ? 'Collapse' : 'Expand'} {stripWorkflowMarkers(row.name)}"
                  onclick={(e) => ontoggle(e, row.path)}
                  onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') ontoggle(e, row.path); }}
                ><Icon name={expandedPaths[row.path] ? 'chevron-down' : 'chevron-right'} size={16} /></span>
              {:else}
                <span class="expand-toggle"></span>
              {/if}
              <span class="folder-icon"><Icon name="folder" size={20} /></span>
              <span class="folder-name" title={stripWorkflowMarkers(row.name)}>{stripWorkflowMarkers(row.name)}</span>
              {#if blockReason}
                <!-- Refused permanently (gotcha #14); the reason and Try again
                     are in NoteList's banner when the folder is open. -->
                <span
                  class="folder-blocked-indicator"
                  role="img"
                  aria-label="Folder not synced — {blockReason}"
                  title={`Not synced — ${blockReason}`}><Icon name="alert" size={14} /></span>
              {/if}
              {#if row.count > 0}<span class="folder-count">{row.count}</span>{/if}
            </div>
          {/if}
        {/each}
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
  .account-section,
  .phone-views + .account-section {
    margin-top: var(--space-4);
  }
  .phone-views {
    padding-bottom: var(--space-1);
  }
  /* The Section Label (DESIGN.md): 700, tracked, uppercase; the scope after
     it is the account's own name, so it keeps its case. */
  .phone-section-label {
    display: flex;
    min-width: 0;
    margin: 0;
    padding: var(--space-2) var(--space-4) var(--space-1);
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    line-height: var(--leading);
    color: var(--text-muted);
  }
  /* The Views label is its own fold toggle: the whole label is the target,
     at least 44px tall for a thumb, and it keeps the Section Label look. */
  .phone-views-toggle {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    min-height: 44px;
    width: 100%;
    margin: calc(-1 * var(--space-2)) 0 calc(-1 * var(--space-1));
    padding: 0;
    border: 0;
    background: none;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .phone-views-caret {
    display: inline-flex;
    flex: none;
  }
  .phone-section-scope {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    text-transform: none;
    letter-spacing: 0;
    font-weight: var(--weight-semibold);
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
    font-weight: var(--weight-semibold);
    line-height: var(--touch-target);
    color: var(--text-secondary);
    cursor: pointer;
    position: relative;
  }
  /* The current account, in full ink (spec 2026-11-10 §4). */
  .account-section.current .phone-account-label {
    color: var(--text);
  }
  .phone-account-label:hover {
    background: var(--surface-hover);
  }
  .phone-account-label.active {
    background: var(--surface-active);
  }
  /* Signed out or blocked: the one state the phone can know without a poll. */
  .account-mark {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--danger);
  }
  .account-notice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-4) var(--space-2);
    font-size: var(--size-sm);
    line-height: var(--leading);
    color: var(--danger);
  }
  .notice-text {
    min-width: 0;
  }
  .notice-action {
    min-height: var(--touch-target);
    padding: 0 var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-badge);
    font: inherit;
    font-weight: var(--weight-semibold);
    color: var(--text);
    cursor: pointer;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
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
    font-weight: var(--weight-regular);
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
    position: relative;
  }
  .folder-item {
    padding-left: calc(var(--space-1) + var(--depth, 0) * var(--space-4));
  }
  .folder-item:hover,
  .phone-nav-row:hover {
    background: var(--surface-hover);
  }
  .folder-item.active,
  .phone-nav-row.active {
    background: var(--surface-active);
  }
  /* The lamp (DESIGN.md, The Lamp Marks Here Rule): the open row carries the
     Lamp Mark rail, as on desktop. */
  .folder-item.active::before,
  .phone-nav-row.active::before,
  .phone-account-label.active::before {
    content: '';
    position: absolute;
    left: var(--space-0-5);
    top: var(--space-3);
    bottom: var(--space-3);
    width: 3px;
    border-radius: var(--radius-sm);
    background: var(--accent-mark);
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
    /* Ink, not amber: amber means touched (DESIGN.md). */
    color: var(--text-secondary);
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
    margin-left: var(--space-1);
    color: var(--danger);
  }
  .folder-count {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
    background: var(--surface-badge);
    padding: var(--space-0-5) var(--space-1-5);
    /* A pill, as in the Sidebar. */
    border-radius: var(--radius-sm);
  }
  .row-chevron {
    display: inline-flex;
    align-items: center;
    color: var(--text-disabled);
  }
</style>
