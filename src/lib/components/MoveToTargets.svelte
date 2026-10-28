<script lang="ts" module>
  import { invoke as invokeIpc } from '@tauri-apps/api/core';

  /**
   * Every signed-in account's folder paths from SQLite (`list_folders`), in
   * parallel. A surface that mounts the list more than once per opening — a
   * sheet's Move level, entered, left and entered again — calls this once
   * when it opens and passes the result as `folders`, so the fetch is not
   * repeated on every toggle.
   */
  export async function loadFolderTargets(accountIds: string[]): Promise<Map<string, string[]>> {
    // Each call hits SQLite only (cached_label_map is read-only here) so it's
    // cheap; doing them in parallel keeps the menu responsive even with
    // several accounts.
    const results = await Promise.all(
      accountIds.map(async (id) => {
        try {
          const paths = await invokeIpc<string[]>('list_folders', { accountId: id });
          return [id, paths] as const;
        } catch (e) {
          console.warn(`list_folders failed for ${id}:`, e);
          // `['Notes'] as const` would infer `readonly ['Notes']`, which does not
          // assign to the `string[]` the success branch yields — so the union of
          // the two branches failed to satisfy `new Map<string, string[]>` below.
          return [id, ['Notes'] as string[]] as const;
        }
      }),
    );
    return new Map(results);
  }
</script>

<script lang="ts">
  // The "Move to" targets — every signed-in account and its folder tree —
  // shared by every surface that moves a note, so the listing cannot drift:
  // NoteContextMenu (the note list's menu, single and batch) and NoteMoreMenu
  // (the editor's ⋯). Moved out of NoteContextMenu unchanged.
  //
  //   variant="cascade" — the anchored desktop menu's "Move to" row, with
  //                       hover submenus: account → folder.
  //   variant="list"    — one flat panel: an account heading, then its
  //                       folders. A phone sheet's second level, and the ⋯
  //                       menu's Move to submenu.
  //
  // The caller decides what a pick does (single move, batch move) and closes
  // its own menu; this component only lists and reports the path.
  import { onMount } from 'svelte';
  import { activeAccounts, notes, currentAccount, accountDisplay, backendLabel } from '../stores/notes';
  import Icon from './Icon.svelte';
  import { stripWorkflowMarkers, isAgentLeaf } from './sidebar/folderTree';
  import type { Note } from '../types';

  interface Props {
    note: Note;
    variant: 'cascade' | 'list';
    /** Batch mode: every selected note moves; only their one account is enabled. */
    isMulti?: boolean;
    multiAccountId?: string | null;
    multiAccountUniform?: boolean;
    /** Folders from `loadFolderTargets`, fetched once by the caller. Omitted:
     *  this component fetches them itself when it mounts. */
    folders?: Map<string, string[]>;
    onPick: (path: string) => void;
  }

  let {
    note,
    variant,
    isMulti = false,
    multiAccountId = null,
    multiAccountUniform = true,
    folders = undefined,
    onPick,
  }: Props = $props();

  // Group folders by account. Each account's Notes tree lives in its own
  // Gmail mailbox, so the cascade reads "Move to → account → folder".
  //
  // Folder source per account, in priority order:
  //   1. The backend's `list_folders` (queries that account's DB rows) —
  //      authoritative even when only one account's notes are loaded in
  //      memory. Fetched once on mount per signed-in account.
  //   2. Paths reconstructed from $notes (live, includes any folders
  //      created in this session that haven't been flushed to DB yet).
  // Union of the two so newly-created folders show up immediately AND
  // accounts whose notes aren't in $notes still appear with their tree.
  // `agent`: an agent folder (spec 2026-10-28 §6.4) — a note may still be
  // moved there, a deliberate act, and the row says where it is going.
  type FolderRow = { path: string; name: string; depth: number; agent: boolean };
  let fetched: Map<string, string[]> = $state(new Map());
  const foldersFromBackend = $derived(folders ?? fetched);

  onMount(async () => {
    if (folders) return; // the caller fetched them once for this opening
    fetched = await loadFolderTargets($activeAccounts.map((a) => a.id));
  });

  const foldersByAccount = $derived.by(() => {
    const byAcct = new Map<string, Set<string>>();
    // Seed each signed-in account with its DB folders (if loaded yet) so
    // the submenu shows the real tree even for accounts whose notes haven't
    // been hydrated into $notes.
    for (const a of $activeAccounts) {
      const seed = foldersFromBackend.get(a.id) ?? ['Notes'];
      byAcct.set(a.id, new Set<string>(seed));
    }
    // Defensive: ensure the note's own account exists in the map.
    const noteAcct = note.account_id || $currentAccount;
    if (noteAcct && !byAcct.has(noteAcct)) {
      byAcct.set(noteAcct, new Set<string>(['Notes']));
    }
    // Also union live $notes folders — catches folders created this session
    // that haven't reached the folders table yet.
    for (const n of $notes) {
      const acct = n.account_id || $currentAccount;
      if (!acct) continue;
      let set = byAcct.get(acct);
      if (!set) {
        set = new Set<string>(['Notes']);
        byAcct.set(acct, set);
      }
      const segs = n.label.split('/');
      for (let i = 1; i <= segs.length; i++) {
        set.add(segs.slice(0, i).join('/'));
      }
    }
    // Convert each set to a sorted folderRows[] for rendering.
    const result = new Map<string, FolderRow[]>();
    for (const [acct, paths] of byAcct) {
      const rows = Array.from(paths)
        .sort((a, b) => a.localeCompare(b))
        .map((p) => {
          const segs = p.split('/');
          const leaf = segs[segs.length - 1];
          return { path: p, name: stripWorkflowMarkers(leaf), depth: segs.length - 1, agent: isAgentLeaf(leaf) };
        });
      result.set(acct, rows);
    }
    return result;
  });

  const noteAccountId = $derived(note.account_id || $currentAccount || '');

  // Ordered list of (account, folders) for the submenu. The note's current
  // account floats to the top so the most likely move target is one hover
  // away, not buried at the bottom of the account list.
  const accountEntries = $derived.by(() => {
    const entries = Array.from(foldersByAccount.entries());
    entries.sort(([a], [b]) => {
      if (a === noteAccountId) return -1;
      if (b === noteAccountId) return 1;
      return a.localeCompare(b);
    });
    return entries;
  });

  // One account's row state, for both variants.
  function accountRow(acct: string) {
    const allowedAcct = isMulti ? multiAccountId : noteAccountId;
    const enabled = acct === allowedAcct && (!isMulti || multiAccountUniform);
    const info = $activeAccounts.find((a) => a.id === acct);
    return {
      enabled,
      info,
      backend: info?.backend_kind ? backendLabel(info.backend_kind) : '',
      reason: !enabled
        ? (isMulti && !multiAccountUniform
            ? 'Selected notes span multiple accounts — not yet supported'
            : 'Cross-account move not supported yet')
        : '',
    };
  }

  // Submenu viewport-fit: when the user hovers a submenu-anchor, measure whether
  // the child .submenu would overflow the right or bottom edge and toggle CSS
  // classes to flip it. Mirrors the Sidebar folder-menu adjustedX/adjustedY approach
  // but applied to nested CSS-hover submenus where absolute positioning is inherited.
  function fitSubmenu(e: MouseEvent) {
    const anchor = (e.currentTarget as HTMLElement);
    const sub = anchor.querySelector(':scope > .submenu') as HTMLElement | null;
    if (!sub) return;
    // Temporarily make it visible (without display:block, getBoundingClientRect returns 0).
    const prevDisplay = sub.style.display;
    sub.style.display = 'block';
    sub.style.visibility = 'hidden';
    const rect = sub.getBoundingClientRect();
    sub.style.display = prevDisplay;
    sub.style.visibility = '';
    // Flip horizontally if it would overflow the right edge.
    if (rect.right > window.innerWidth - 8) {
      sub.classList.add('flip-left');
    } else {
      sub.classList.remove('flip-left');
    }
    // Clamp vertically if it would overflow the bottom edge.
    const overflow = rect.bottom - (window.innerHeight - 8);
    if (overflow > 0) {
      const newTop = Math.max(-(rect.height - 40), -overflow);
      sub.style.top = `${newTop}px`;
    } else {
      sub.style.top = '';
    }
  }
</script>

{#if variant === 'cascade'}
  <!-- role="none" suppresses a11y lint: onmouseenter is for viewport-fit measurement only, not interaction -->
  <div class="submenu-anchor" role="none" onmouseenter={fitSubmenu}>
    <button class="item has-submenu" type="button">
      <span class="icon"><Icon name="folder" /></span>
      <span class="label">Move to</span>
      <span class="chevron"><Icon name="chevron-right" size={12} /></span>
    </button>
    <div class="submenu">
      {#each accountEntries as [acct, folders] (acct)}
        {@const row = accountRow(acct)}
        <div class="submenu-anchor" role="none" onmouseenter={fitSubmenu}>
          <button
            class="item has-submenu"
            class:disabled-account={!row.enabled}
            disabled={!row.enabled}
            title={!row.enabled ? row.reason : accountDisplay(row.info, acct)}
            type="button"
          >
            <span class="icon"><Icon name="person" /></span>
            <span class="label">{accountDisplay(row.info, acct)}</span>
            <!-- Account.id is now `{backend}:{email}`, so two accounts can
                 share an address on different backends (e.g. gmail:a@b.com
                 and microsoft:a@b.com) — this picker is exactly where a
                 cross-account move target must be disambiguated. Shown
                 unconditionally whenever known, matching Sidebar.svelte and
                 settings/AccountPage.svelte; omitted (not an empty pill) when
                 backend_kind is absent, e.g. a legacy/unmigrated account. -->
            {#if row.backend}
              <span class="account-backend-tag">{row.backend}</span>
            {/if}
            <span class="chevron"><Icon name="chevron-right" size={12} /></span>
          </button>
          {#if row.enabled}
            <div class="submenu folder-list">
              {#each folders as f (f.path)}
                {@const isCurrentFolder = !isMulti && f.path === note.label}
                <button
                  class="item folder"
                  class:current={isCurrentFolder}
                  style="padding-left: calc(var(--space-6) + var(--space-1) + {f.depth} * (var(--space-3) + var(--space-0-5)))"
                  onclick={() => onPick(f.path)}
                  disabled={isCurrentFolder}
                  title={f.path}
                  type="button"
                >
                  <span class="label">{f.name}</span>
                  {#if f.agent}<span class="agent-tag">Agents</span>{/if}
                  {#if isCurrentFolder}
                    <span class="check" role="img" aria-label="Current folder"><Icon name="check" size={12} /></span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{:else}
  {#each accountEntries as [acct, folders] (acct)}
    {@const row = accountRow(acct)}
    <div class="account-group">
      <div
        class="item account-row"
        class:disabled-account={!row.enabled}
        aria-disabled={!row.enabled}
        title={row.enabled ? accountDisplay(row.info, acct) : row.reason}
      >
        <span class="icon"><Icon name="person" /></span>
        <span class="label">{accountDisplay(row.info, acct)}</span>
        {#if row.backend}
          <span class="account-backend-tag">{row.backend}</span>
        {/if}
      </div>
      <!-- A tooltip never shows on touch: say why, in text. -->
      {#if row.reason}
        <div class="row-note">{row.reason}</div>
      {/if}
    </div>
    {#if row.enabled}
      {#each folders as f (f.path)}
        {@const isCurrentFolder = !isMulti && f.path === note.label}
        <button
          class="item folder"
          class:current={isCurrentFolder}
          style="padding-left: calc(var(--space-6) + {f.depth} * var(--space-4))"
          onclick={() => onPick(f.path)}
          disabled={isCurrentFolder}
          title={f.path}
          type="button"
        >
          <span class="label">{f.name}</span>
          {#if f.agent}<span class="agent-tag">Agents</span>{/if}
          {#if isCurrentFolder}
            <span class="check" role="img" aria-label="Current folder"><Icon name="check" size={12} /></span>
          {/if}
        </button>
      {/each}
    {/if}
  {/each}
{/if}

<style>
  /* Rows match the menus that host this list (NoteContextMenu, NoteMoreMenu).
     Inside a phone sheet, src/styles/sheet-menu.css restyles them as touch
     rows; the sheet-only rules at the bottom are this list's own row kinds. */
  .item {
    display: flex;
    align-items: center;
    gap: calc(var(--space-2) + var(--space-0-5));
    width: 100%;
    /* 7px, the context menu's own row padding, so its "Move to" row lines
       up with its siblings (tokens only in a new file). */
    padding: calc(var(--space-1-5) + var(--space-0-5) / 2) var(--space-3);
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text);
    border-radius: var(--radius-sm);
    transition: background 0.1s;
  }

  .item:hover:not(:disabled):not(.account-row) {
    background: var(--surface-hover);
  }

  .item:disabled {
    cursor: default;
    opacity: 0.4;
    /* Defeat the .item:hover background so disabled rows look inert even
       when the cursor is over them. Without this, a disabled cross-account
       folder still highlighted blue on hover and looked clickable. */
    background: transparent !important;
  }

  .item.folder {
    padding-top: calc(var(--space-1) + var(--space-0-5) / 2);
    padding-bottom: calc(var(--space-1) + var(--space-0-5) / 2);
  }

  .item.folder.current {
    color: var(--text-muted);
  }

  .icon {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    text-align: center;
    flex-shrink: 0;
  }

  .label {
    flex: 1;
    line-height: var(--leading);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .check {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: var(--size-xs);
  }

  .chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--text-disabled);
    font-size: var(--size-xs);
  }

  /* Matches .account-backend-tag in Sidebar.svelte / .account-line-backend
     in settings/AccountPage.svelte: same muted-pill language for naming an
     account's backend, informational not interactive. Styles aren't shared
     across components (Svelte scopes each component's CSS), so this is a
     deliberate, minimal duplicate. */
  .account-backend-tag {
    font-size: var(--size-xs);
    color: var(--text-muted);
    background: var(--surface-badge);
    border-radius: 8px;
    padding: calc(var(--space-0-5) / 2) var(--space-1-5); /* 1px 6px, as before the move */
    flex: 0 0 auto;
    white-space: nowrap;
  }

  /* ─── Cascade (anchored desktop menu) ──────────────────────────────────
     The anchor stays in normal flow; the .submenu inside it is absolutely
     positioned to the right of the parent button and only appears on hover.
     Nesting works recursively because every level uses the same
     .submenu-anchor structure. */
  .submenu-anchor {
    position: relative;
  }
  .submenu-anchor > .submenu {
    display: none;
    position: absolute;
    left: 100%;
    top: calc(-1 * var(--space-1)); /* align with parent's first item, accounting for menu padding */
    min-width: 220px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: var(--space-1);
    /* Above the parent menu's own rows. The menu is positioned with its own
       z-index, so this stacks inside the menu's stacking context: the layer
       token for a popover says what it is without competing app-wide. */
    z-index: var(--z-popover);
    /* NO overflow here — overflow on a parent .submenu would clip its
       absolutely-positioned grandchild .submenu (the folder list living
       to the right). Apply overflow only on the leaf list below. */
  }
  /* Viewport-fit: flip submenu to the left when it would overflow the right edge.
     The class is added by fitSubmenu() at runtime, so use :global to prevent
     Svelte's unused-selector warning from removing it. */
  :global(.submenu-anchor > .submenu.flip-left) {
    left: auto;
    right: 100%;
  }
  /* The actual long list (folders) — this is the only level that can
     exceed the viewport, so this is the only one that needs scroll.
     max-height must be small enough that the box always fits within the
     viewport regardless of where the submenu anchor opens. `calc(100vh -
     24px)` was a bug: when the anchor is partway down the screen, the
     box's bottom extends past the viewport edge, making the bottom items
     unreachable. 60vh keeps the box compact enough to fit even when the
     anchor is at the middle of the screen, while still showing ~15 rows. */
  .submenu-anchor > .submenu.folder-list {
    max-height: 60vh;
    overflow-y: auto;
  }
  /* Reveal one level at a time. Without :hover on the anchor, the
     submenu stays hidden even if the parent menu is open. */
  .submenu-anchor:hover > .submenu {
    display: block;
  }
  .item.has-submenu {
    /* Visual cue that this item leads somewhere — the hover-only submenu is
       the primary affordance. */
    cursor: default;
  }

  /* ─── List (a flat panel: sheet level or ⋯ submenu) ─────────────────── */
  /* Account rows are headings, not buttons: no hover or pressed highlight. */
  .item.account-row,
  .item.account-row:hover,
  .item.account-row:active {
    cursor: default;
    color: var(--text-muted);
    background: transparent;
  }
  .item.account-row.disabled-account {
    opacity: 0.6;
  }
  /* An agent folder's tag: agents write there (spec 2026-10-28 §6.4). */
  .agent-tag {
    flex: 0 0 auto;
    margin-left: auto;
    font-size: var(--size-xs);
    color: var(--text-muted);
  }
  .row-note {
    padding: 0 var(--space-3) var(--space-2) calc(var(--space-3) + var(--space-4) + var(--space-3));
    font-size: var(--size-xs);
    line-height: var(--leading);
    color: var(--text-muted);
  }

  /* Inside a phone sheet (sheet-menu.css owns the base touch rows). These
     outrank its rules: a :global ancestor plus this component's scoped
     classes beat `dialog[data-presentation='sheet'] .sheet-menu .item`. */
  :global(.sheet-menu) .item.folder {
    padding-top: var(--space-2);
    padding-bottom: var(--space-2);
  }
  :global(.sheet-menu) .item.account-row,
  :global(.sheet-menu) .item.account-row:hover,
  :global(.sheet-menu) .item.account-row:active {
    cursor: default;
    color: var(--text-muted);
    background: transparent;
  }
</style>
