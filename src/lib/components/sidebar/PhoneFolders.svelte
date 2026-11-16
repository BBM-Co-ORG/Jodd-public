<script lang="ts">
  // The phone's own Folders pane. The Sidebar renders it instead of its
  // desktop/tablet markup when the layout store says phone ($isAndroid, then
  // 'phone'), so it is the same Sidebar reading the layout store, not a prop
  // fork.
  //
  // It is the desktop column at touch size (spec 2026-11-12): the same order,
  // names and marks — the "For ⌄" account picker (2+ accounts), Views as a
  // fold, Tags, then each account: its header (fold chevron, short name,
  // backend, state, count; the header opens every note in the account), its
  // folders, its Agents group and its Recently Deleted. What differs, and
  // why:
  //   - Tags opens its own pane (PhoneTagsPane) rather than a pill cloud:
  //     hundreds of tags at a 48px touch size cannot sit inline, and picking
  //     several needs a Select mode;
  //   - each view carries its meaning as a second line: touch has no hover;
  //   - the bottom bar (§5.1 of spec 2026-10-20): New folder · AI ✦ · New
  //     note, and the footer's accounts, App settings and About live in
  //     Settings (the pane header's gear).
  //
  // The folds, the folder tree's state (which paths are expanded), the sync
  // marks, the folder sheet and folder creation stay the Sidebar's — the
  // tablet layout uses them too — and come in as props and callbacks.
  import Icon from '../Icon.svelte';
  import PhoneBar from '../PhoneBar.svelte';
  import AiActionsSheet from '../AiActionsSheet.svelte';
  import ScopePicker from './ScopePicker.svelte';
  import { longpress } from '../../longpress';
  import { navRows } from '../../navRows';
  import { isSignedOut, reconnectKind } from '../../controllers/authLoss';
  import { startAddAccount } from '../../controllers/accountActions';
  import { type FolderRow, isRowVisible, rowHasChildren, stripWorkflowMarkers } from './folderTree';
  import {
    activeAccounts, currentAccount, selectedFolder, selectedSmartFolder, selectedTags, searchQuery,
    tagsByAccount, capabilitiesByAccount, shouldShowTrash, canWrite, accountDisplay, backendLabel,
    type SmartFolderKind,
  } from '../../stores/notes';
  import { accountNameBesideBackend, accountNameWithBackend, currentListTitle } from '../../stores/listView';
  import { phonePaneAccount, phonePaneAllAccounts } from '../../stores/phonePanes';
  import { scope, ALL_ACCOUNTS } from '../../stores/sidebarScope';
  import { unionTags } from '../../tagOrder';
  import { navigateToPane } from '../../stores/phoneNav';
  import { selectFolder, selectSmartFolder, openNewNote } from '../../controllers/viewSelection';
  import { folderBlockReason, syncBlocks } from '../../syncBlocks';
  import { SMART_FOLDER_HINTS, SMART_FOLDER_NAMES, viewName } from '../../viewName';
  import { SMART_FOLDER_KINDS, SMART_FOLDER_ROWS } from '../../smartFolders';
  import { accountHeaderMark, type AccountDot } from '../../accountDot';

  let {
    split,
    totals,
    expandedPaths,
    dots,
    viewsOpen,
    collapsedAccounts,
    ontoggle,
    ontoggleviews,
    ontoggleaccount,
    onfoldermenu,
    onaccountmenu,
    onnewfolder,
  }: {
    /** Per account: the user folder rows and the workflow folder rows. */
    split: Record<string, { user: FolderRow[]; workflow: FolderRow[] }>;
    /** Per account: every note in it — the header's count. */
    totals: Record<string, number>;
    expandedPaths: Record<string, boolean>;
    /** Per account: its sync state (the Sidebar's poll, accountDot.ts). */
    dots: Record<string, AccountDot | undefined>;
    /** The Views fold, shared with the desktop column (sidebarFolds.ts). */
    viewsOpen: boolean;
    /** The account ids folded away, shared with the desktop column. */
    collapsedAccounts: Set<string>;
    ontoggle: (e: MouseEvent | KeyboardEvent, path: string) => void;
    ontoggleviews: () => void;
    ontoggleaccount: (accountId: string) => void;
    /** Long-press / right-click on a folder row: the Sidebar's folder sheet. */
    onfoldermenu: (e: MouseEvent, accountId: string, path: string) => void;
    /** Long-press / right-click on an account header: the Sidebar's account sheet. */
    onaccountmenu: (e: MouseEvent, accountId: string) => void;
    /** New top-level folder in `accountId`; `title` names the prompt. */
    onnewfolder: (accountId: string, title?: string) => void;
  } = $props();

  let aiSheetOpen = $state(false);

  // A folder pick, then the list push. The push is made here, not left to
  // App's $selectedFolder watcher: that watcher only fires on a CHANGE, so
  // re-tapping the folder already shown would do nothing (ruling R21). The
  // title names the list for the Back label of a note opened from it (§5.1).
  function pickFolder(accountId: string, path: string) {
    selectFolder(accountId, path);
    navigateToPane('list', currentListTitle());
  }

  // A view pick, then the list push, for the same reason (R21).
  function pickView(accountId: string | null, kind: SmartFolderKind) {
    selectSmartFolder(accountId, kind);
    navigateToPane('list', SMART_FOLDER_NAMES[kind]);
  }

  // Views and Tags show the scoped account: the "For ⌄" picker's choice,
  // else the current account (stores/sidebarScope.ts). The Tags pane reads
  // which account from phonePaneAccount.
  // Under All accounts (spec 2026-11-13) Views read every active account and
  // Tags list their union; `viewAccountId` is then null.
  const scopeAll = $derived($scope?.kind === 'all');
  const scopeAcct = $derived.by(() => {
    const s = $scope;
    return s?.kind === 'account' ? $activeAccounts.find((a) => a.id === s.accountId) : undefined;
  });
  const viewAccountId = $derived(scopeAll ? null : (scopeAcct?.id ?? null));
  const scopeTagCount = $derived(
    scopeAll
      ? unionTags($activeAccounts.map((a) => $tagsByAccount.get(a.id) ?? [])).length
      : scopeAcct ? ($tagsByAccount.get(scopeAcct.id) ?? []).length : 0,
  );
  function openTags(accountId: string | null) {
    phonePaneAllAccounts.set(accountId === null);
    phonePaneAccount.set(accountId);
    navigateToPane('tags', 'Tags');
  }

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
    <ScopePicker {dots} touch />
    {#if scopeAll || scopeAcct}
      <!-- Once, for the scoped account, in the desktop column's order
           (spec 2026-11-12 §1). -->
      <section class="phone-views" data-scope-account={viewAccountId ?? ALL_ACCOUNTS} aria-label="Views and tags">
        <button
          type="button"
          class="phone-fold"
          data-row="views"
          aria-expanded={viewsOpen}
          onclick={ontoggleviews}
        >
          <span class="fold-caret"><Icon name={viewsOpen ? 'chevron-down' : 'chevron-right'} size={16} /></span>
          <span class="fold-label">Views</span>
        </button>
        {#if viewsOpen}
          {#each SMART_FOLDER_KINDS as kind (kind)}
            {@const open = !!$selectedSmartFolder && $selectedSmartFolder.account === viewAccountId && $selectedSmartFolder.kind === kind}
            <button
              type="button"
              class="phone-view-row"
              class:active={open}
              aria-current={open ? 'true' : undefined}
              data-smart-folder={kind}
              onclick={() => pickView(viewAccountId, kind)}
            >
              <span class="folder-icon"><Icon name={SMART_FOLDER_ROWS[kind].icon} size={20} /></span>
              <span class="view-text">
                <span class="folder-name">{SMART_FOLDER_NAMES[kind]}</span>
                <span class="view-hint">{SMART_FOLDER_HINTS[kind]}</span>
              </span>
            </button>
          {/each}
        {/if}
        {#if scopeTagCount > 0}
          <!-- The desktop's Tags header, at touch size; it opens the Tags
               pane rather than a pill cloud (see the top of this file). -->
          <button type="button" class="phone-fold" data-row="tags" onclick={() => openTags(viewAccountId)}>
            <span class="fold-caret"></span>
            <span class="fold-label">Tags</span>
            <span class="fold-num">{scopeTagCount}</span>
            <span class="row-chevron"><Icon name="chevron-right" size={16} /></span>
          </button>
        {/if}
      </section>
    {/if}
    {#each $activeAccounts as acct (acct.id)}
      {@const isCurrent = acct.id === $currentAccount}
      {@const name = accountNameBesideBackend(acct, $activeAccounts)}
      {@const backend = backendLabel(acct.backend_kind ?? 'gmail')}
      {@const collapsed = collapsedAccounts.has(acct.id)}
      {@const allOpen = isCurrent && $selectedFolder === '__ALL__' && !$selectedSmartFolder && $selectedTags.size === 0}
      {@const dot = dots[acct.id]}
      {@const mark = dot ? accountHeaderMark(dot) : null}
      {@const foldersLocked = !canWrite($capabilitiesByAccount[acct.id], 'folders')}
      {@const reconnect = reconnectKind(acct)}
      {@const rows = split[acct.id]}
      <section
        class="account-section"
        class:current={isCurrent}
        data-account-id={acct.id}
        aria-label="{name} · {backend}"
      >
        <!-- The account's own row, as on desktop (spec 2026-11-10 §2): it
             opens every note in the account; its chevron folds the account
             away. -->
        <div
          class="folder-item account-header"
          class:active={allOpen}
          aria-current={allOpen ? 'true' : undefined}
          aria-expanded={!collapsed}
          data-depth="-1"
          role="button"
          tabindex="-1"
          title="All notes in {accountDisplay(acct)}"
          onclick={() => pickFolder(acct.id, '__ALL__')}
          onkeydown={(e) => { if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); pickFolder(acct.id, '__ALL__'); } }}
          oncontextmenu={(e) => onaccountmenu(e, acct.id)}
          use:longpress
        >
          <span
            class="expand-toggle"
            role="button"
            tabindex="-1"
            aria-label="{collapsed ? 'Expand' : 'Collapse'} {name} · {backend}"
            onclick={(e) => { e.stopPropagation(); ontoggleaccount(acct.id); }}
            onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.stopPropagation(); ontoggleaccount(acct.id); } }}
          ><Icon name={collapsed ? 'chevron-right' : 'chevron-down'} size={16} /></span>
          <span class="account-name">{name}</span>
          {#if isCurrent && $activeAccounts.length > 1}<span class="sr-only">, current account</span>{/if}
          <span class="account-backend-tag">{backend}</span>
          {#if mark && dot}
            <!-- Only what needs the user, as on desktop (spec 2026-11-10 §3). -->
            <span class="account-mark" data-mark={mark} role="img" aria-label={dot.text} title={dot.text}></span>
          {/if}
          {#if foldersLocked}
            <span
              class="account-lock"
              role="img"
              aria-label="Folders can't be changed from Jodd for this account"
              title="Folders can't be changed from Jodd for this account"
            ><Icon name="lock" size={14} /></span>
          {/if}
          <span class="folder-count">{totals[acct.id] ?? 0}</span>
        </div>
        {#if acct.blocked_reason}
          <div class="account-notice" role="status">
            <span class="notice-text">{isSignedOut(acct) ? 'Signed out. Its notes and unsent changes are kept here.' : acct.blocked_reason}</span>
            {#if reconnect}
              <button type="button" class="notice-action" onclick={() => startAddAccount(reconnect)}>Sign in again</button>
            {/if}
          </div>
        {/if}
        {#if !collapsed}
          {#snippet folderRow(row: FolderRow, all: FolderRow[])}
            {#if isRowVisible(row.path, expandedPaths)}
              {@const hasKids = rowHasChildren(all, row.path)}
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
          {/snippet}
          {#each rows?.user ?? [] as row (row.path)}
            {@render folderRow(row, rows.user)}
          {/each}
          {#if (rows?.workflow.length ?? 0) > 0}
            <!-- Agent folders (spec 2026-10-28 §6), in the tree as on
                 desktop (spec 2026-11-12 §5). -->
            <p class="group-label" title="Folders that AI agents write to">Agents</p>
            {#each rows.workflow as row (row.path)}
              {@render folderRow(row, rows.workflow)}
            {/each}
          {/if}
          <!-- The account's Recently Deleted, last, only where its backend
               has a trash (spec 2026-11-12 §2). -->
          {#if shouldShowTrash($capabilitiesByAccount[acct.id])}
            {@const open = isCurrent && $selectedFolder === '__TRASH__' && !$selectedSmartFolder && $selectedTags.size === 0}
            <div
              class="folder-item"
              class:active={open}
              aria-current={open ? 'true' : undefined}
              data-view="trash"
              data-depth="0"
              style="--depth: 0"
              role="button"
              tabindex="-1"
              onclick={() => pickFolder(acct.id, '__TRASH__')}
              onkeydown={(e) => { if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); pickFolder(acct.id, '__TRASH__'); } }}
            >
              <span class="expand-toggle"></span>
              <!-- Ink, not danger: this row goes to a place; it deletes nothing. -->
              <span class="folder-icon"><Icon name="trash" size={20} inherit /></span>
              <span class="folder-name">Recently Deleted</span>
            </div>
          {/if}
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
      title="Ask Jodd, make a note with AI, Studio and Organize"
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
  <AiActionsSheet onclose={() => (aiSheetOpen = false)} />
{/if}

<style>
  /* ─── Phone Folders screen: the desktop column at touch size ────────────
     (spec 2026-11-12). Full width under App's PaneHeader, rows 48px, one
     text column for the account headers, the folds and depth-0 folders. The
     bar (PhoneBar.svelte) at the bottom. Tokens only. */
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
  .account-section {
    margin-top: var(--space-2);
  }
  .phone-views {
    padding-bottom: var(--space-1);
  }

  /* The desktop fold headers (Views, Tags), at touch size: DESIGN.md's
     Section Label — 700, tracked, uppercase, muted. */
  .phone-fold {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    box-sizing: border-box;
    width: 100%;
    min-height: var(--touch-target);
    padding: 0 var(--space-4) 0 0;
    background: none;
    border: none;
    font: inherit;
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    line-height: var(--leading-tight);
    color: var(--text-muted);
    text-align: left;
    cursor: pointer;
  }
  .phone-fold:hover {
    background: var(--surface-hover);
  }
  .fold-caret,
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
  .fold-label {
    flex: 1;
  }
  .fold-num {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    font-weight: var(--weight-regular);
    letter-spacing: 0;
    color: var(--text-muted);
    background: var(--surface-badge);
    padding: var(--space-0-5) var(--space-1-5);
    border-radius: var(--radius-sm);
  }

  /* A view: its name, and under it what it holds (no hover on touch). */
  .phone-view-row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    box-sizing: border-box;
    width: 100%;
    min-height: var(--touch-target);
    padding: var(--space-1-5) var(--space-4) var(--space-1-5) calc(var(--touch-target) + var(--space-2));
    background: none;
    border: none;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    position: relative;
  }
  .view-text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .view-hint {
    font-size: var(--size-xs);
    line-height: var(--leading);
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* The account's own row: the desktop header at touch size. */
  .account-name {
    flex: 0 1 auto;
    min-width: 0;
    font-size: var(--size-sm);
    font-weight: var(--weight-semibold);
    line-height: var(--leading);
    color: var(--text-secondary);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* The current account, in full ink (spec 2026-11-10 §4). */
  .account-section.current .account-name {
    color: var(--text);
  }
  .account-backend-tag {
    /* Gives way ten times faster than the name beside it. */
    flex: 0 10 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--size-xs);
    color: var(--text-muted);
    background: var(--surface-badge);
    border-radius: var(--radius-sm);
    padding: 0 var(--space-1);
  }
  .account-header .folder-count {
    margin-left: auto;
  }
  /* The desktop header's marks: a lost sign-in or a block (filled, danger),
     changes waiting (a hollow ring in muted ink). Never colour alone: the
     mark carries its text as its name. */
  .account-mark {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    box-sizing: border-box;
  }
  .account-mark[data-mark='alert'] {
    background: var(--danger);
  }
  .account-mark[data-mark='waiting'] {
    border: 1.5px solid var(--text-muted);
  }
  .account-lock {
    display: inline-flex;
    align-items: center;
    flex: 0 0 auto;
    color: var(--text-muted);
  }
  .account-notice {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-4) var(--space-2) var(--touch-target);
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

  /* The Agents group's label, as desktop's .group-header. */
  .group-label {
    margin: var(--space-2) 0 0;
    padding: var(--space-1) var(--space-4) var(--space-1) var(--touch-target);
    font-size: var(--size-xs);
    font-weight: var(--weight-bold);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    line-height: var(--leading-tight);
    color: var(--text-muted);
  }

  .folder-item {
    display: flex;
    align-items: center;
    box-sizing: border-box;
    width: 100%;
    min-height: var(--touch-target);
    margin: 0;
    gap: var(--space-2);
    padding: 0 var(--space-4) 0 0;
    background: none;
    border: none;
    border-radius: 0;
    font: inherit;
    color: var(--text);
    text-align: left;
    cursor: pointer;
    position: relative;
  }
  /* One step in per level. The header's chevron and a depth-0 folder's
     toggle share the first 48px column, so the account name, the fold
     labels and the top folders' icons line up. */
  .folder-item[data-depth]:not(.account-header) {
    padding-left: calc(var(--depth, 0) * var(--space-4));
  }
  .folder-item:hover,
  .phone-view-row:hover {
    background: var(--surface-hover);
  }
  .folder-item.active,
  .phone-view-row.active {
    background: var(--surface-active);
  }
  /* The lamp (DESIGN.md, The Lamp Marks Here Rule): the open row carries the
     Lamp Mark rail, as on desktop. */
  .folder-item.active::before,
  .phone-view-row.active::before {
    content: '';
    position: absolute;
    left: var(--space-0-5);
    top: var(--space-3);
    bottom: var(--space-3);
    width: 3px;
    border-radius: var(--radius-sm);
    background: var(--accent-mark);
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
    flex-shrink: 0;
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
