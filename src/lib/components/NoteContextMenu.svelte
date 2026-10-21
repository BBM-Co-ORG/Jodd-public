<script lang="ts">
  import { syncBlocks, forgetPinBlocks } from '../syncBlocks';
  import { beginNoteDeletion, deletingNotes } from '../deleteNote';
  import { noteKey, sameNote } from '../noteIdentity';
  import { invoke } from '@tauri-apps/api/core';
  import { pendingDeletedNoteKeys, noteMutationRevision, error, activeAccounts, notes, selectedNote, currentAccount, indexRemoveOnDelete, indexUpsertOnSave, noteTagsByAccount, getNoteTags, setNoteTags, backendLabel, capabilitiesByAccount, canWrite, needsPermanentDeleteConfirm } from '../stores/notes';
  import * as actions from '../controllers/noteActions';
  import { hiddenFromAi, aiHiddenFor, AI_HIDDEN_HINT } from '../stores/aiScope';
  import type { Note } from '../types';
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import Icon from './Icon.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { createConfirmGate } from '../controllers/confirmGate';
  import Modal from './Modal.svelte';
  import MoveToTargets, { loadFolderTargets } from './MoveToTargets.svelte';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { displayTitle } from '../textDisplay';
  import { isPhoneNavActive, type Pane } from '../stores/phoneNav';
  import { listAsShown, selectionAfterDeleting } from '../controllers/selection';

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
  // #2b retroactive linking: the review modal for `proposed_appends` is
  // owned by the parent (NoteList.svelte) so it survives past this menu's
  // own onClose()-triggered unmount. See linkIntoWiki() below.
  export let onLinkSuggestions: (
    accountId: string,
    proposedAppends: { uuid: string; title: string; addition_text: string }[],
  ) => void;

  // Per-area write gates, matching what each item below actually invokes:
  // pin is `Write::Sidecars` (`set_pin`/`set_pin_batch`); new note,
  // duplicate, re-extract's content half, link into wiki are `Write::Notes`
  // (`save_note`/`re_extract_note`/`apply_wiki_link_appends`); delete, move
  // and suggest-folder are `Write::Relocate` (`delete_note`/
  // `delete_notes_batch`/`move_notes_batch`; accepting a folder suggestion
  // is an ordinary relocation, and the ask itself is gated the same way so
  // the menu never offers a suggestion it can't act on) — split out from
  // `notes` because iCloud can do these without being able to edit content
  // (M2 relocation live pass, 2026-08-24: move/trash/restore never send the
  // document, confirmed on three independent Apple surfaces). Re-extract no
  // longer needs `Write::Folders`: it files the new note beside its source
  // (or Notes/Inbox/root, never a folder the backend can't already write),
  // so it doesn't create a folder the way the old __Extracts__ destination
  // could. Using one coarse gate here would show Pin as available on
  // Microsoft, where `notes` is true but `sidecars` is not — exactly the
  // frontend/backend divergence this split exists to describe.
  // Every mutating item is hidden rather than shown-and-failing; the real
  // enforcement is `refuse_write` in lib.rs — a write that reached SQLite
  // here would leave a row the worker can never push, which permanently
  // blocks draining the account.
  $: writeAccountId = note.account_id || $currentAccount;
  $: aiHidden = aiHiddenFor($hiddenFromAi, writeAccountId, note.label);
  $: canWriteNotes = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'notes',
  );
  $: canRelocate = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'relocate',
  );
  $: canWriteSidecars = canWrite(
    writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined,
    'sidecars',
  );

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

  $: noteAccountId = note.account_id || $currentAccount || '';
  // The refetch command is backend-neutral — it goes through the account's
  // vertical like every other read. Only the WORDING was Gmail's, which on an
  // iCloud account offered to re-pull the note "from Gmail".
  $: refetchSourceName = (() => {
    const kind = $activeAccounts.find((a) => a.id === noteAccountId)?.backend_kind;
    return kind ? backendLabel(kind) : 'the server';
  })();

  // ─── Phone: the same menu as a bottom sheet ───────────────────────────
  // Gate order is global: $isAndroid first, then the layout mode. A narrow
  // desktop window keeps the anchored menu; tablet does too.
  $: phoneSheet = $isAndroid && $androidLayoutMode === 'phone';
  // The layout can leave "phone" (rotation) while the sheet is open. Its Modal
  // then unmounts without onclose, and the anchored menu would appear at the
  // stale touch coordinates: close the menu instead.
  let wasPhoneSheet = false;
  function trackPhoneSheet(now: boolean) {
    if (wasPhoneSheet && !now) onClose();
    // Becoming the sheet — at open, or a tablet → phone flip while the
    // anchored menu was up — fetches the Move level's folders.
    if (!wasPhoneSheet && now) loadSheetFolders();
    wasPhoneSheet = now;
  }
  $: trackPhoneSheet(phoneSheet);
  $: sheetTitle = isMulti ? `${multiCount} notes` : displayTitle(note.title) || 'Untitled';
  // Second level of the sheet ("Move to"), rendered inside the same sheet
  // with a Back row rather than as a second floating menu.
  let sheetLevel: 'root' | 'move' = 'root';
  // The Move level's folders, fetched once per opening of the sheet (the
  // level remounts its list each time it is entered). The anchored menu's
  // cascade fetches its own, once, when it mounts with the menu. Undefined
  // until the fetch lands: MoveToTargets then fetches for itself, where an
  // empty Map would have read as "fetched, nothing there".
  let sheetFolders: Map<string, string[]> | undefined = undefined;
  function loadSheetFolders() {
    void loadFolderTargets(get(activeAccounts).map((a) => a.id)).then((m) => (sheetFolders = m));
  }
  let sheet: { requestClose(): Promise<boolean>; closeInto(pane: Pane): boolean } | undefined;

  // Every item closes the menu through here: on the sheet, through the
  // Modal's one close path (which pops its history entry and then calls
  // onClose); on the anchored menu, straight to onClose.
  function dismiss() {
    if (sheet) void sheet.requestClose();
    else onClose();
  }

  // An item that navigates closes the sheet INTO that pane, so the sheet's
  // history entry becomes the destination's and Back lands beneath it.
  function dismissInto(pane: Pane) {
    if (sheet) {
      if (!sheet.closeInto(pane)) void sheet.requestClose();
    } else {
      onClose();
    }
  }

  function newNoteHere() {
    const blank: Note = {
      id: '', uuid: '',
      title: 'New Note',
      body_html: '<html><head></head><body></body></html>',
      date: new Date().toISOString(),
      label: note.label, // same folder as the clicked note
      x_mail_created_date: null,
    };
    // Close first on the sheet: the selection change below opens the note
    // pane, and that push must not land above the sheet's history entry.
    dismissInto('note');
    selectedNote.set(blank);
  }

  // ─── Single-note actions ─────────────────────────────────────────────
  // The bodies live in controllers/noteActions.ts, shared with every other
  // surface that offers them, so they cannot drift. This menu only closes
  // itself: it calls the action, then onClose(). Each action's optimistic
  // write and IPC send run before its first await, so they still land
  // before the menu closes, as when the bodies lived here.
  function moveTo(target: string) {
    void actions.moveNoteTo(note, target);
    dismiss();
  }

  // Duplicate and Refetch closed the menu only once their work finished;
  // they still do.
  async function duplicateNote() {
    const source = note; // the prop reads back as null once the menu has closed
    if (sheet) {
      // Duplicating selects the copy, which opens the note pane. That push
      // must not land above the sheet's history entry, so the sheet closes
      // first (the desktop menu closes after the work, as before).
      dismiss();
      await actions.duplicateNote(source);
      return;
    }
    await actions.duplicateNote(source);
    dismiss();
  }

  async function refetchFromRemote() {
    await actions.refetchNote(note);
    dismiss();
  }

  // Re-extract is only offered when the note carries the Source verbatim
  // block — the only shape the lesson extractor knows how to consume.
  function hasSourceBlock(body: string): boolean {
    return body.includes('<summary>Source (verbatim)</summary>');
  }

  function reExtractLessons() {
    void actions.reExtractLessons(note);
    dismiss();
  }

  // #2b: the review modal for `proposed_appends` is owned by NoteList, so it
  // survives this menu's onClose()-triggered unmount.
  function linkIntoWiki() {
    void actions.linkIntoWiki(note, onLinkSuggestions);
    dismiss();
  }

  function suggestFolder() {
    void actions.suggestFolder(note);
    dismiss();
  }

  function togglePinOne() {
    void actions.togglePin(note);
    dismiss();
  }

  // ─── Pin (batch path) ────────────────────────────────────────────────
  //
  // Same shape as deleteBatch / moveBatchTo: snapshot $notes + $selectedNote,
  // flip every selected uuid in one optimistic store update, fire the
  // atomic backend call, roll back the entire batch on failure.
  async function setPinBatch(nextPinned: boolean) {
    if (!multiAccountUniform || !multiAccountId) {
      dismiss();
      return;
    }
    const accountId = multiAccountId;
    const uuids = selection.map((n) => n.uuid).filter((u) => !!u);
    dismiss();
    if (uuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);
    const prevBlocks = get(syncBlocks);

    forgetPinBlocks(accountId, uuids);
    const uuidSet = new Set(uuids);
    notes.update((ns) =>
      ns.map((n) => (n.account_id === accountId && uuidSet.has(n.uuid) ? { ...n, pinned: nextPinned } : n)),
    );
    selectedNote.update((cur) =>
      cur && cur.account_id === accountId && uuidSet.has(cur.uuid) ? { ...cur, pinned: nextPinned } : cur,
    );

    try {
      await invoke<number>('set_pin_batch', { accountId, uuids, pinned: nextPinned });
    } catch (e) {
      console.error('batch pin failed', e);
      actions.rollbackFields(prevNotes, prevSelectedNote, ['pinned'], n => n.account_id === accountId && uuidSet.has(n.uuid));
      syncBlocks.set(prevBlocks);
      error.set(`Failed to ${nextPinned ? 'pin' : 'unpin'} ${uuids.length} note(s): ${e}`);
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

  // Inline confirm dialog (ConfirmDialog.svelte) for the permanent-delete
  // case — same reason as Sidebar.svelte's askConfirm: Tauri WKWebView's
  // native confirm() is unreliable, and an in-DOM dialog can be styled and
  // supports Enter/Esc. Only reachable when needsPermanentDeleteConfirm() is
  // true (Microsoft: no trash, hard delete) — Gmail/LocalFs keep the
  // one-click delete below. Shared between the single-note path (deleteNote)
  // and the batch path (deleteBatch) — only one of the two menu modes is
  // ever rendered at a time, so one set of state suffices for both. The
  // state is the shared confirm gate (controllers/confirmGate.ts), the same
  // one NoteMoreMenu uses.
  const permDelete = createConfirmGate();
  const permDeletePending = permDelete.pending;
  const askPermDeleteConfirm = permDelete.ask;

  // The confirm dialog renders inside this still-mounted menu, so the
  // action — not this wrapper — says when the menu may close: after a
  // cancel, or right after the optimistic removal began.
  function deleteNote() {
    void actions.deleteNoteWithConfirm(note, askPermDeleteConfirm, dismiss);
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
      dismiss();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const uuids = batch.map((n) => n.uuid).filter((u) => !!u);

    // Same hard-delete gate as the single-note delete (Task 10b,
    // actions.deleteNoteWithConfirm) — batch is the
    // most dangerous of the three delete surfaces: one click destroys every
    // selected note, with the least friction of the three. Must run BEFORE
    // onClose() below, same reasoning as deleteNote() — the confirm dialog
    // renders inside this still-mounted menu, and onClose() unmounts it.
    if (needsPermanentDeleteConfirm($capabilitiesByAccount[accountId])) {
      const ok = await askPermDeleteConfirm(
        `Delete ${batch.length} notes permanently?`,
        'Jodd has no Recently Deleted view for this account, so it cannot undo this.',
      );
      if (!ok) {
        dismiss();
        return;
      }
    }
    dismiss();

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);
    // Snapshot each note's tags so we can restore them if the backend rejects
    // the batch (the backend clears note_tags as part of the delete).
    const tagSnap = new Map(
      uuids.map((u) => [u, getNoteTags(get(noteTagsByAccount), accountId, u)] as const),
    );

    // The open note among them: the first note left below the deleted block
    // opens instead (else the nearest above it); the phone pops to the list
    // (selection.ts, spec §5.3). Read from the list before the rows leave it.
    const openDeleted = !!$selectedNote && $selectedNote.account_id === accountId && uuids.includes($selectedNote.uuid);
    const replacement = openDeleted ? selectionAfterDeleting(listAsShown(), new Set(batch.map(noteKey))) : null;
    batch.forEach(beginNoteDeletion);
    pendingDeletedNoteKeys.update(s => new Set([...s, ...batch.map(noteKey)]));
    notes.update((ns) => ns.filter((x) => !(x.account_id === accountId && uuids.includes(x.uuid))));
    if (openDeleted) selectedNote.set(replacement);
    // `batch`/`selection` entries can be stale snapshots (same reasoning as
    // the single-delete path in actions.deleteNoteWithConfirm) — look each id up from `prevNotes`
    // (captured live, before the filter) rather than trusting the prop.
    const liveById = new Map(prevNotes.map((n) => [noteKey(n), n.id] as const));
    for (const n of batch) indexRemoveOnDelete(accountId, liveById.get(noteKey(n)) || n.id);
    for (const u of uuids) setNoteTags(accountId, u, []);

    try {
      await invoke<number>('delete_notes_batch', { accountId, uuids });
    } catch (e) {
      console.error('batch delete failed', e);
      notes.update(ns => [...ns, ...prevNotes.filter(n => n.account_id === accountId && uuids.includes(n.uuid) && !ns.some(x => sameNote(x, n)))]);
      if (openDeleted && !isPhoneNavActive()) {
        const now = get(selectedNote);
        if (!now || (replacement && sameNote(now, replacement))) selectedNote.set(prevSelectedNote);
      }
      for (const [u, tags] of tagSnap) setNoteTags(accountId, u, tags);
      error.set(`Failed to delete ${batch.length} note(s): ${e}`);
    } finally {
      queueMicrotask(() => batch.forEach(n => deletingNotes.delete(noteKey(n))));
      noteMutationRevision.update(n => n + 1);
      pendingDeletedNoteKeys.update(s => new Set([...s].filter(k => !batch.some(n => noteKey(n) === k))));
    }
  }

  // Doctrine-compliant batch move: optimistic label rewrite for every
  // selected note BEFORE the awaited invoke, single atomic backend call,
  // rollback the entire batch on failure. The sync worker pushes each
  // moved row to Gmail in the background — the user sees the move land
  // instantly and never waits on N round-trips.
  async function moveBatchTo(target: string) {
    if (!multiAccountUniform || !multiAccountId) {
      dismiss();
      return;
    }
    const accountId = multiAccountId;
    const batch = selection.slice();
    const movingUuids = batch.filter((n) => n.label !== target).map((n) => n.uuid);
    dismiss();
    if (movingUuids.length === 0) return;

    const prevNotes = get(notes);
    const prevSelectedNote = get(selectedNote);

    notes.update((ns) =>
      ns.map((n) =>
        n.account_id === accountId && movingUuids.includes(n.uuid) ? { ...n, label: target } : n,
      ),
    );
    selectedNote.update((cur) =>
      cur && cur.account_id === accountId && movingUuids.includes(cur.uuid) ? { ...cur, label: target } : cur,
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
        if (n.account_id === accountId && movingUuids.includes(n.uuid)) {
          indexUpsertOnSave(accountId, n.id || null, n.id || '', target);
        }
      }
    } catch (e) {
      console.error('batch move failed', e);
      actions.rollbackFields(prevNotes, prevSelectedNote, ['label'], n => n.account_id === accountId && movingUuids.includes(n.uuid));
      error.set(`Failed to move ${batch.length} note(s): ${e}`);
    }
  }

  // Close on outside click or Esc. Pointerdown fires before click, so we catch
  // it earlier — avoids the "menu closes on the click that should trigger an item"
  // race when items use onclick.
  // While the permanent-delete confirm dialog is open, it — not this
  // handler — owns outside-click/Esc: it renders on top of `.context-menu`
  // (this component stays mounted so the dialog has somewhere to render),
  // and letting this handler fire `onClose()` here would unmount the whole
  // component (NoteList's `menuNote = null`) with the confirm Promise still
  // pending, wedging the delete on a promise nothing will ever resolve.
  function onPointerDown(e: PointerEvent) {
    if ($permDeletePending || phoneSheet) return; // the sheet's Modal owns outside taps
    const target = e.target as HTMLElement;
    if (!target.closest('.context-menu')) onClose();
  }
  function onKey(e: KeyboardEvent) {
    if ($permDeletePending || phoneSheet) return; // the sheet's Modal owns Esc
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


{#snippet directItems()}
  {#if isMulti}
    <div class="multi-header" title="Acts on every selected note">
      {multiCount} notes selected
    </div>
    <div class="sep"></div>
    {#if !canWriteSidecars}
      <div class="multi-header">Pin isn't available on this account yet</div>
    {:else if multiAllPinned}
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Unpin all</span>
      </button>
    {:else if multiAllUnpinned}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Pin all</span>
      </button>
    {:else if multiMixedPin}
      <button class="item" onclick={() => setPinBatch(true)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Pin all</span>
      </button>
      <button class="item" onclick={() => setPinBatch(false)}>
        <span class="icon"><Icon name="pin" /></span>
        <span class="label">Unpin all</span>
      </button>
    {/if}
  {/if}

  {#if !isMulti}
    {#if canWriteSidecars}
    <button class="item" onclick={togglePinOne}>
      <span class="icon"><Icon name="pin" /></span>
      <span class="label">{note.pinned ? 'Unpin' : 'Pin'}</span>
    </button>
    {/if}
    {#if canWriteSidecars && canWriteNotes}
    <div class="sep"></div>
    {/if}
    {#if canWriteNotes}
    <button class="item" onclick={newNoteHere}>
      <span class="icon">＋</span>
      <span class="label">New Note</span>
    </button>
    <button class="item" onclick={duplicateNote} disabled={!note.id}>
      <span class="icon"><Icon name="copy" /></span>
      <span class="label">Duplicate</span>
    </button>
    {/if}
    <button
      class="item"
      onclick={refetchFromRemote}
      disabled={!note.id}
      title="Bypass cache and re-pull this note's content from {refetchSourceName}"
    >
      <span class="icon"><Icon name="refresh" /></span>
      <span class="label">Refetch from {refetchSourceName}</span>
    </button>
    {#if canWriteNotes && hasSourceBlock(note.body_html ?? '')}
      <button
        class="item"
        onclick={reExtractLessons}
        disabled={aiHidden}
        title={aiHidden ? AI_HIDDEN_HINT : "Re-run the lesson extractor over this note's Source block"}
      >
        <span class="icon"><Icon name="bulb" /></span>
        <span class="label">Re-extract</span>
      </button>
    {/if}
    {#if canWriteNotes}
      <button class="item" onclick={linkIntoWiki} disabled={aiHidden} title={aiHidden ? AI_HIDDEN_HINT : 'Find and link related notes'}>
        <span class="icon"><Icon name="graph" /></span>
        <span class="label">Link into wiki</span>
      </button>
    {/if}
    {#if canRelocate}
      <button class="item" onclick={suggestFolder} disabled={aiHidden} title={aiHidden ? AI_HIDDEN_HINT : 'Ask the LLM which existing folder this note belongs in'}>
        <span class="icon"><Icon name="folder" /></span>
        <span class="label">Suggest folder</span>
      </button>
    {/if}
  {/if}

{/snippet}

{#snippet deleteItem()}
  <!-- Delete grouped with the other direct actions above, rather than
       isolated after "Move to" — a browsable folder list, not a single
       action, so it sits last. Delete + Move to are both `Write::Relocate`
       (delete_note/delete_notes_batch, move_notes_batch) — gated
       separately from `canWriteNotes` since iCloud can do these while
       content editing stays refused. On the phone sheet Delete is rendered
       after "Move to" instead (ruling R17: Delete is the last row). -->
  {#if canRelocate}
  <div class="sep"></div>
  <button class="item danger" onclick={() => (isMulti ? deleteBatch() : deleteNote())}>
    <span class="icon"><Icon name="trash" /></span>
    <span class="label">{isMulti ? `Delete ${multiCount} notes` : 'Delete'}</span>
  </button>
  {/if}
{/snippet}

{#if phoneSheet}
  <!-- Phone: the same items, in the same order, as a bottom sheet with
       touch-sized rows. "Move to" opens a second level inside this sheet. -->
  <Modal
    bind:this={sheet}
    phone="sheet"
    title={sheetTitle}
    onbeforeclose={() => !$permDeletePending}
    onclose={onClose}
  >
    <div class="sheet-menu" role="menu">
      {#if sheetLevel === 'move'}
        <button class="item back-row" type="button" onclick={() => (sheetLevel = 'root')}>
          <span class="icon" aria-hidden="true">‹</span>
          <span class="label">Back</span>
        </button>
        <div class="sep"></div>
        <MoveToTargets
          {note}
          variant="list"
          folders={sheetFolders}
          {isMulti}
          {multiAccountId}
          {multiAccountUniform}
          onPick={(path) => (isMulti ? moveBatchTo(path) : moveTo(path))}
        />
      {:else}
        {@render directItems()}
        {#if canRelocate}
          <button class="item has-submenu" type="button" onclick={() => (sheetLevel = 'move')}>
            <span class="icon"><Icon name="folder" /></span>
            <span class="label">Move to</span>
            <span class="chevron"><Icon name="chevron-right" size={12} /></span>
          </button>
        {/if}
        <!-- Ruling R17: on the phone sheet Delete is the last row. -->
        {@render deleteItem()}
      {/if}
    </div>
  </Modal>
{:else}
<div
  bind:this={menuEl}
  class="context-menu"
  style="left: {adjustedX}px; top: {adjustedY}px"
  role="menu"
>
  {@render directItems()}
  {@render deleteItem()}
  {#if canRelocate}
  <div class="sep"></div>
  <MoveToTargets
    {note}
    variant="cascade"
    {isMulti}
    {multiAccountId}
    {multiAccountUniform}
    onPick={(path) => (isMulti ? moveBatchTo(path) : moveTo(path))}
  />
  {/if}
</div>
{/if}

{#if $permDeletePending}
  <ConfirmDialog
    title={$permDeletePending.title}
    message={$permDeletePending.message}
    confirmLabel="Delete permanently"
    destructive={true}
    onConfirm={() => permDelete.decide(true)}
    onCancel={() => permDelete.decide(false)}
  />
{/if}

<style>
  .context-menu {
    position: fixed;
    min-width: 220px;
    background: var(--surface-panel);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: var(--shadow-menu);
    padding: 4px;
    z-index: 1000;
    font-size: var(--size);
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
  .item.has-submenu {
    /* The phone sheet's "Move to" row opens a second level. */
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
    color: var(--text);
    border-radius: 4px;
    transition: background 0.1s;
  }

  .item:hover:not(:disabled):not(.header) {
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

  .item.danger {
    color: var(--danger);
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

  .sep {
    height: 1px;
    background: var(--surface-active);
    margin: 4px 8px;
  }

  .multi-header {
    font-size: var(--size-xs);
    font-weight: 600;
    color: var(--accent-action);
    background: var(--accent-wash);
    padding: 6px 12px;
    border-radius: 4px;
    margin: 2px 4px 0;
  }

  /* ─── Phone bottom sheet ─────────────────────────────────────────────
     The rows are src/styles/sheet-menu.css, shared by every sheet menu; only
     this menu's own row kinds are here (the Move to level's are in
     MoveToTargets.svelte). Tokens only. */
  .sheet-menu .multi-header {
    margin: 0 0 var(--space-1);
    padding: var(--space-2) var(--space-3);
  }

  /* Permanent-delete confirm dialog now lives in ConfirmDialog.svelte — see
     the {#if $permDeletePending} block above. */
</style>
