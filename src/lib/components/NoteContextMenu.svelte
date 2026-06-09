<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';
  import { accounts, notes, selectedNote, selectedFolder, refreshNotes, currentAccount, indexRemoveOnDelete, indexUpsertOnSave } from '../stores/notes';
  import type { Note } from '../types';
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';

  // Position is in viewport coords from the contextmenu event.
  export let x: number;
  export let y: number;
  export let note: Note;
  // Optional multi-note batch. When length > 1, the menu renders in
  // bulk mode: single-only items (New here, Duplicate, Refetch) hide,
  // Move-to + Delete iterate over every entry. When empty or length === 1,
  // the menu behaves exactly like before (single-note actions on `note`).
  export let selection: Note[] = [];
  export let onClose: () => void;

  $: isMulti = selection.length > 1;
  $: multiCount = selection.length;
  // For Move-to: in multi-mode all selected notes must be in the same
  // account (cross-account batch move would need separate plumbing).
  // We pick that account from the first entry; if any others differ we
  // disable the move-to submenu.
  $: multiAccountId = isMulti ? (selection[0].account_id ?? $currentAccount) : null;
  $: multiAccountUniform = isMulti
    ? selection.every((n) => (n.account_id ?? $currentAccount) === multiAccountId)
    : true;

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
  type FolderRow = { path: string; name: string; depth: number };
  let foldersFromBackend: Map<string, string[]> = new Map();

  onMount(async () => {
    // Fetch folders for every signed-in account in parallel. Each call hits
    // SQLite only (cached_label_map is read-only here) so it's cheap; doing
    // them in parallel keeps the menu responsive even with several accounts.
    const list = $accounts;
    const results = await Promise.all(
      list.map(async (a) => {
        try {
          const paths = await invoke<string[]>('list_folders', { accountId: a.id });
          return [a.id, paths] as const;
        } catch (e) {
          console.warn(`list_folders failed for ${a.id}:`, e);
          return [a.id, ['Notes']] as const;
        }
      }),
    );
    foldersFromBackend = new Map(results);
  });

  $: foldersByAccount = (() => {
    const byAcct = new Map<string, Set<string>>();
    // Seed each signed-in account with its DB folders (if loaded yet) so
    // the submenu shows the real tree even for accounts whose notes haven't
    // been hydrated into $notes.
    for (const a of $accounts) {
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
          return { path: p, name: segs[segs.length - 1], depth: segs.length - 1 };
        });
      result.set(acct, rows);
    }
    return result;
  })();

  $: noteAccountId = note.account_id || $currentAccount || '';

  // Ordered list of (account, folders) for the submenu. The note's current
  // account floats to the top so the most likely move target is one hover
  // away, not buried at the bottom of the account list.
  $: accountEntries = (() => {
    const entries = Array.from(foldersByAccount.entries());
    entries.sort(([a], [b]) => {
      if (a === noteAccountId) return -1;
      if (b === noteAccountId) return 1;
      return a.localeCompare(b);
    });
    return entries;
  })();

  function newNoteHere() {
    const blank: Note = {
      id: '', uuid: '',
      title: 'New Note',
      body_html: '<html><head></head><body></body></html>',
      date: new Date().toISOString(),
      label: note.label, // same folder as the clicked note
      x_mail_created_date: null,
    };
    selectedNote.set(blank);
    onClose();
  }

  async function moveTo(target: string) {
    if (target === note.label) {
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
      return;
    }

    // Snapshot EVERY field we'll need before calling onClose(). After
    // onClose() destroys this menu component, Svelte 5's prop reactivity
    // means `note` resolves to null — touching `note.title` (etc.) throws
    // `null is not an object (evaluating 'note().title')`. Save_note then
    // never runs and the move silently fails.
    const prevLabel = note.label;
    const prevId = note.id;
    const targetUuid = note.uuid;
    const noteTitle = note.title;
    const noteBodyHtml = note.body_html;
    const noteXMailCreated = note.x_mail_created_date ?? null;

    notes.update((ns) => {
      const idx = ns.findIndex((n) => n.uuid === targetUuid);
      if (idx >= 0) ns[idx] = { ...ns[idx], label: target };
      return ns;
    });
    if ($selectedNote?.uuid === targetUuid) {
      selectedNote.update((n) => (n ? { ...n, label: target } : n));
    }
    onClose();

    try {
      const saved = await invoke<{ id: string; uuid: string }>('save_note', {
        accountId,
        title: noteTitle,
        bodyHtml: noteBodyHtml,
        existingGmailId: prevId,
        existingUuid: targetUuid,
        existingXMailCreatedDate: noteXMailCreated,
        label: target,
      });
      // Backfill the new Gmail id once it's known. Label was already updated.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === saved.uuid);
        if (idx >= 0) ns[idx] = { ...ns[idx], id: saved.id };
        return ns;
      });
      if ($selectedNote?.uuid === saved.uuid) {
        selectedNote.update((n) => (n ? { ...n, id: saved.id } : n));
      }
      // Index: remove the source-label stub, add destination-label stub.
      indexUpsertOnSave(accountId, prevId || null, saved.id, target);
    } catch (e) {
      // Rollback the label to where it was. The user sees the note snap back.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === targetUuid);
        if (idx >= 0) ns[idx] = { ...ns[idx], label: prevLabel };
        return ns;
      });
      if ($selectedNote?.uuid === targetUuid) {
        selectedNote.update((n) => (n ? { ...n, label: prevLabel } : n));
      }
      console.error('move failed', e);
      alert(`Failed to move note: ${e}`);
    }
  }

  async function duplicateNote() {
    if (!note.id) {
      // Unsaved blank — nothing to duplicate from Gmail. Just close.
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
      return;
    }
    try {
      // Save without existingUuid/existingGmailId → Rust generates a fresh
      // UUID and inserts a brand-new message. Same title/body/label as the
      // source. The original note is untouched.
      const newTitle = `${note.title} copy`;
      const saved = await invoke<{ id: string; uuid: string }>('save_note', {
        accountId,
        title: newTitle,
        bodyHtml: note.body_html,
        existingGmailId: null,
        existingUuid: null,
        existingXMailCreatedDate: null,
        label: note.label,
      });
      const dup: Note = {
        id: saved.id,
        uuid: saved.uuid,
        title: newTitle,
        body_html: note.body_html,
        date: new Date().toISOString(),
        label: note.label,
        x_mail_created_date: null,
        account_id: accountId,
      };
      notes.update((ns) => [dup, ...ns]);
      selectedNote.set(dup);
      // Duplicate is a brand-new message — no previous id to clear.
      indexUpsertOnSave(accountId, null, saved.id, note.label);
    } catch (e) {
      console.error('duplicate failed', e);
      alert(`Failed to duplicate: ${e}`);
    }
    onClose();
  }

  async function refetchFromGmail() {
    if (!note.id) {
      // No remote version yet — nothing to refetch.
      onClose();
      return;
    }
    const accountId = note.account_id || $currentAccount;
    if (!accountId) {
      onClose();
      return;
    }
    try {
      const fresh = await invoke<Note>('refetch_note', { accountId, id: note.id });
      // Replace the in-memory copy. By matching on uuid (not id — a refetch
      // may surface that Apple appended a new message id and our local id
      // is now stale), we update the correct row even if the id rotated.
      notes.update((ns) => {
        const idx = ns.findIndex((n) => n.uuid === fresh.uuid);
        if (idx >= 0) ns[idx] = fresh;
        return ns;
      });
      if ($selectedNote?.uuid === fresh.uuid) selectedNote.set(fresh);
    } catch (e) {
      console.error('refetch failed', e);
      alert(`Refetch failed: ${e}`);
    }
    onClose();
  }

  // ─── Pin (single-note path) ──────────────────────────────────────────
  //
  // Doctrine-compliant: snapshot, flip the in-memory pinned flag and let
  // the reactive sort in NoteList float the row to the top, fire the
  // local-first set_pin command (SQLite UPDATE, no Gmail), roll back on
  // failure. The backend never waits on Gmail because pin doesn't
  // round-trip through the email backend.
  async function togglePinOne() {
    const accountId = note.account_id || $currentAccount;
    if (!accountId || !note.uuid) {
      onClose();
      return;
    }
    const targetUuid = note.uuid;
    const nextPinned = !note.pinned;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);
    onClose();

    notes.update((ns) =>
      ns.map((n) => (n.uuid === targetUuid ? { ...n, pinned: nextPinned } : n)),
    );
    selectedNote.update((cur) =>
      cur && cur.uuid === targetUuid ? { ...cur, pinned: nextPinned } : cur,
    );

    try {
      await invoke('set_pin', { accountId, uuid: targetUuid, pinned: nextPinned });
    } catch (e) {
      console.error('pin failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to ${nextPinned ? 'pin' : 'unpin'} note: ${e}`);
    }
  }

  // ─── Pin (batch path) ────────────────────────────────────────────────
  //
  // Same shape as deleteBatch / moveBatchTo: snapshot $notes + $selectedNote,
  // flip every selected uuid in one optimistic store update, fire the
  // atomic backend call, roll back the entire batch on failure.
  async function setPinBatch(nextPinned: boolean) {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const uuids = selection.map((n) => n.uuid).filter((u) => !!u);
    onClose();
    if (uuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    const uuidSet = new Set(uuids);
    notes.update((ns) =>
      ns.map((n) => (uuidSet.has(n.uuid) ? { ...n, pinned: nextPinned } : n)),
    );
    selectedNote.update((cur) =>
      cur && uuidSet.has(cur.uuid) ? { ...cur, pinned: nextPinned } : cur,
    );

    try {
      await invoke<number>('set_pin_batch', { accountId, uuids, pinned: nextPinned });
    } catch (e) {
      console.error('batch pin failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to ${nextPinned ? 'pin' : 'unpin'} ${uuids.length} note(s): ${e}`);
    }
  }

  // For multi-select, the menu shows either "Pin all" or "Unpin all" when
  // the selection is uniform, OR both items (no single combined toggle)
  // when the selection is mixed. Mixed state would be ambiguous — does
  // "Pin all" mean "pin every unpinned one" or "make every one pinned"?
  // The spec says: when uniformly pinned/unpinned, show one toggle;
  // otherwise show "Pin all" + "Unpin all" as two distinct items.
  $: multiAllPinned = isMulti && selection.every((n) => !!n.pinned);
  $: multiAllUnpinned = isMulti && selection.every((n) => !n.pinned);
  $: multiMixedPin = isMulti && !multiAllPinned && !multiAllUnpinned;

  async function deleteNote() {
    // No confirm() — WKWebView eats it silently in some configurations.
    // The Delete menu item is already an explicit action; trash is recoverable
    // via Gmail's "Trash" view for 30 days.
    // No `if (!note.id)` guard — local-only notes (never synced, id='') are
    // still deletable; Rust's delete_note path uses uuid + checks remote_version.
    try {
      const accountId = note.account_id || $currentAccount;
      if (!accountId) return;
      await invoke('delete_note', { accountId, id: note.id, uuid: note.uuid });
      notes.update((ns) => ns.filter((n) => n.uuid !== note.uuid));
      indexRemoveOnDelete(accountId, note.id);
      if ($selectedNote?.uuid === note.uuid) selectedNote.set(null);
    } catch (e) {
      console.error('delete failed', e);
    }
    onClose();
  }

  // ─── Batch (multi-select) actions ────────────────────────────────────
  //
  // Both batch paths close the menu first and run the network work in the
  // background. The UI updates optimistically and per-item; partial failures
  // get rolled back individually (move) or just logged (delete).

  // Doctrine-compliant batch delete: optimistic removal from the store
  // BEFORE the awaited invoke, single atomic backend call instead of N
  // sequential ones, rollback on failure. The user no longer sees the
  // batch land row-by-row — every selected note vanishes in one frame,
  // and either the backend confirms (in which case nothing changes) or
  // it fails (in which case every removed row comes back).
  //
  // All selected notes must belong to the same account for this to be
  // a single backend call — the menu's multiAccountUniform check
  // guarantees that's the case before this is reachable.
  async function deleteBatch() {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const uuids = batch.map((n) => n.uuid).filter((u) => !!u);
    onClose();

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    notes.update((ns) => ns.filter((x) => !uuids.includes(x.uuid)));
    if ($selectedNote && uuids.includes($selectedNote.uuid)) {
      selectedNote.set(null);
    }
    for (const n of batch) indexRemoveOnDelete(accountId, n.id);

    try {
      await invoke<number>('delete_notes_batch', { accountId, uuids });
    } catch (e) {
      console.error('batch delete failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to delete ${batch.length} note(s): ${e}`);
    }
  }

  // Doctrine-compliant batch move: optimistic label rewrite for every
  // selected note BEFORE the awaited invoke, single atomic backend call,
  // rollback the entire batch on failure. The sync worker pushes each
  // moved row to Gmail in the background — the user sees the move land
  // instantly and never waits on N round-trips.
  async function moveBatchTo(target: string) {
    if (!multiAccountUniform || !multiAccountId) {
      onClose();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const movingUuids = batch.filter((n) => n.label !== target).map((n) => n.uuid);
    onClose();
    if (movingUuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    notes.update((ns) =>
      ns.map((n) =>
        movingUuids.includes(n.uuid) ? { ...n, label: target } : n,
      ),
    );
    selectedNote.update((cur) =>
      cur && movingUuids.includes(cur.uuid) ? { ...cur, label: target } : cur,
    );

    try {
      await invoke<number>('move_notes_batch', {
        accountId,
        uuids: movingUuids,
        targetLabel: target,
      });
      // Keep the per-folder index in sync. The Gmail message id doesn't
      // change here — the move is a pure label rewrite in SQLite — so we
      // map source-label stub → destination-label stub by re-using the
      // same id. (Gmail-side, the sync worker will insert-new + trash-old
      // for each row, which will rotate the id; the next index refresh
      // catches that, but until then the count remains correct.)
      for (const n of batch) {
        if (movingUuids.includes(n.uuid)) {
          indexUpsertOnSave(accountId, n.id || null, n.id || '', target);
        }
      }
    } catch (e) {
      console.error('batch move failed', e);
      notes.set(prevNotes);
      selectedNote.set(prevSelectedNote);
      alert(`Failed to move ${batch.length} note(s): ${e}`);
    }
  }

  // Close on outside click or Esc. Pointerdown fires before click, so we catch
  // it earlier — avoids the "menu closes on the click that should trigger an item"
  // race when items use onclick.
  function onPointerDown(e: PointerEvent) {
    const target = e.target as HTMLElement;
    if (!target.closest('.context-menu')) onClose();
  }
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') onClose();
  }

  onMount(() => {
    window.addEventListener('pointerdown', onPointerDown, true);
    window.addEventListener('keydown', onKey);
  });
  onDestroy(() => {
    window.removeEventListener('pointerdown', onPointerDown, true);
    window.removeEventListener('keydown', onKey);
  });

  // Keep the menu within the viewport — flip up/left if it would overflow.
  let menuEl: HTMLDivElement;
  let adjustedX = x;
  let adjustedY = y;
  $: if (menuEl) {
    const rect = menuEl.getBoundingClientRect();
    if (x + rect.width > window.innerWidth) adjustedX = window.innerWidth - rect.width - 8;
    if (y + rect.height > window.innerHeight) adjustedY = window.innerHeight - rect.height - 8;
  }
</script>


<div
  bind:this={menuEl}
  class="context-menu"
  style="left: {adjustedX}px; top: {adjustedY}px"
  role="menu"
>
  {#if isMulti}
    <div class="multi-header" title="Acts on every selected note">
      {multiCount} notes selected
    </div>
    <div class="sep"></div>
    {#if multiAllPinned}
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon">📌</span>
        <span class="label">Unpin all</span>
      </button>
    {:else if multiAllUnpinned}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon">📌</span>
        <span class="label">Pin all</span>
      </button>
    {:else if multiMixedPin}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon">📌</span>
        <span class="label">Pin all</span>
      </button>
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon">📌</span>
        <span class="label">Unpin all</span>
      </button>
    {/if}
    <div class="sep"></div>
  {/if}

  {#if !isMulti}
    <button class="item" onclick={togglePinOne}>
      <span class="icon">📌</span>
      <span class="label">{note.pinned ? 'Unpin' : 'Pin'}</span>
    </button>
    <div class="sep"></div>
    <button class="item" onclick={newNoteHere}>
      <span class="icon">＋</span>
      <span class="label">New Note</span>
    </button>
    <button class="item" onclick={duplicateNote} disabled={!note.id}>
      <span class="icon">⎘</span>
      <span class="label">Duplicate</span>
    </button>
    <button
      class="item"
      onclick={refetchFromGmail}
      disabled={!note.id}
      title="Bypass cache and re-pull this note's content from Gmail"
    >
      <span class="icon">↻</span>
      <span class="label">Refetch from Gmail</span>
    </button>
    <div class="sep"></div>
  {/if}

  <div class="submenu-anchor">
    <button class="item has-submenu" type="button">
      <span class="icon">📁</span>
      <span class="label">Move to</span>
      <span class="chevron">▸</span>
    </button>
    <div class="submenu">
      {#each accountEntries as [acct, folders] (acct)}
        {@const allowedAcct = isMulti ? multiAccountId : noteAccountId}
        {@const acctEnabled = acct === allowedAcct && (!isMulti || multiAccountUniform)}
        <div class="submenu-anchor">
          <button
            class="item has-submenu"
            class:disabled-account={!acctEnabled}
            disabled={!acctEnabled}
            title={!acctEnabled
              ? (isMulti && !multiAccountUniform
                  ? 'Selected notes span multiple accounts — not yet supported'
                  : 'Cross-account move not supported yet')
              : acct}
            type="button"
          >
            <span class="icon">👤</span>
            <span class="label">{acct}</span>
            <span class="chevron">▸</span>
          </button>
          {#if acctEnabled}
            <div class="submenu folder-list">
              {#each folders as row (row.path)}
                {@const isCurrentFolder = !isMulti && row.path === note.label}
                <button
                  class="item folder"
                  class:current={isCurrentFolder}
                  style="padding-left: {28 + row.depth * 14}px"
                  onclick={() => (isMulti ? moveBatchTo(row.path) : moveTo(row.path))}
                  disabled={isCurrentFolder}
                  title={row.path}
                  type="button"
                >
                  <span class="label">{row.name}</span>
                  {#if isCurrentFolder}
                    <span class="check">✓</span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>

  <div class="sep"></div>
  <button class="item danger" onclick={() => (isMulti ? deleteBatch() : deleteNote())}>
    <span class="icon">🗑</span>
    <span class="label">{isMulti ? `Delete ${multiCount} notes` : 'Delete'}</span>
  </button>
</div>

<style>
  .context-menu {
    position: fixed;
    min-width: 220px;
    background: white;
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.16);
    padding: 4px;
    z-index: 1000;
    font-size: 13px;
  }

  /* Cascading submenu container. The anchor stays in normal flow; the
     .submenu inside it is absolutely positioned to the right of the
     parent button and only appears on hover. Nesting works recursively
     because every level uses the same .submenu-anchor structure. */
  .submenu-anchor {
    position: relative;
  }
  .submenu-anchor > .submenu {
    display: none;
    position: absolute;
    left: 100%;
    top: -4px;            /* align with parent's first item, accounting for menu padding */
    min-width: 220px;
    background: white;
    border: 1px solid rgba(0, 0, 0, 0.12);
    border-radius: 8px;
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.16);
    padding: 4px;
    z-index: 1001;        /* above the parent menu */
    /* NO overflow here — overflow on a parent .submenu would clip its
       absolutely-positioned grandchild .submenu (the folder list living
       to the right). Apply overflow only on the leaf list below. */
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
  .chevron {
    margin-left: auto;
    color: rgba(0, 0, 0, 0.4);
    font-size: 11px;
  }
  .item.has-submenu {
    /* Visual cue that this item leads somewhere — and prevents the row
       from feeling like a dead-end since the click handler will still
       fire (moving to the folder), but the hover-only submenu is the
       primary affordance. */
    cursor: default;
  }

  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 7px 12px;
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: #222;
    border-radius: 4px;
    transition: background 0.1s;
  }

  .item:hover:not(:disabled):not(.header) {
    background: rgba(0, 0, 0, 0.06);
  }

  .item:disabled {
    cursor: default;
    opacity: 0.4;
    /* Defeat the .item:hover background so disabled rows look inert even
       when the cursor is over them. Without this, a disabled cross-account
       folder still highlighted blue on hover and looked clickable. */
    background: transparent !important;
  }

  .item.header {
    color: #888;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    cursor: default;
    padding-top: 6px;
    padding-bottom: 4px;
  }

  .item.folder {
    padding-top: 5px;
    padding-bottom: 5px;
  }

  .item.folder.current {
    color: #888;
  }

  .item.danger {
    color: #c0392b;
  }

  .icon {
    width: 16px;
    text-align: center;
    flex-shrink: 0;
  }

  .label {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .check {
    color: #888;
    font-size: 11px;
  }

  .sep {
    height: 1px;
    background: rgba(0, 0, 0, 0.08);
    margin: 4px 8px;
  }

  .multi-header {
    font-size: 11px;
    font-weight: 600;
    color: #8a6a2a;
    background: rgba(201, 124, 31, 0.08);
    padding: 6px 12px;
    border-radius: 4px;
    margin: 2px 4px 0;
  }
</style>
