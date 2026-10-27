<script lang="ts">
  // The editor's ⋯ menu (spec 2026-10-20 §6.1, §6.3; the phone sheet's item
  // list is §5.6). An anchored Popover on desktop and tablet, a bottom sheet
  // on phone (Popover's own gate).
  //
  // Every note action here is the note-list context menu's own function from
  // controllers/noteActions.ts, gated by the same capability checks
  // (NoteContextMenu.svelte) — a second implementation that drifts from the
  // context menu is the anti-goal. "Move to…" lists the context menu's own
  // targets (MoveToTargets.svelte). What this component owns is only the
  // surface: which items show, closing itself, and the permanent-delete
  // ConfirmDialog. The editor-only items (Find, Graph, Keyboard shortcuts,
  // Tags, Mark reviewed) call the editor's existing handlers.
  import { tick, untrack } from 'svelte';
  import Popover from './Popover.svelte';
  import ConfirmDialog from './ConfirmDialog.svelte';
  import { createConfirmGate } from '../controllers/confirmGate';
  import MoveToTargets, { loadFolderTargets } from './MoveToTargets.svelte';
  import Icon from './Icon.svelte';
  import * as actions from '../controllers/noteActions';
  import { activeAccounts, currentAccount, capabilitiesByAccount, canWrite, backendLabel, error } from '../stores/notes';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { showToast } from '../stores/toast';
  import { noteSlug } from '../noteSlug';
  import { copyText } from '../clipboard';
  import { openAskWith } from '../stores/ui';
  import { hiddenFromAi, aiHiddenFor, AI_HIDDEN_HINT } from '../stores/aiScope';
  import { noteScope } from './askScope';
  import type { Note } from '../types';

  interface Props {
    note: Note;
    anchor: HTMLElement;
    open: boolean;
    onclose: () => void;
    onFind: () => void;
    onShowGraph: () => void;
    onShortcuts: () => void;
    /** The metadata line's trust status is showing (§6.3). */
    trustPending: boolean;
    /** The status popover's own Mark reviewed handler. */
    onMarkReviewed: () => void | Promise<unknown>;
    /** Mark reviewed's disabled rule, as in the status popover. */
    reviewBlocked?: boolean;
    /** Interim "Tags…" (Task 5.3 replaces it): the metadata line's #N handler. */
    onTags: () => void;
    /** Shown exactly when the metadata line shows its #N button. */
    showTags: boolean;
  }

  let {
    note,
    anchor,
    open,
    onclose,
    onFind,
    onShowGraph,
    onShortcuts,
    trustPending,
    onMarkReviewed,
    reviewBlocked = false,
    onTags,
    showTags,
  }: Props = $props();

  // The context menu's gates, from the same capability record: Pin is
  // `sidecars`, Duplicate is `notes`, Move to and Delete are `relocate`.
  // Each item is hidden rather than shown-and-failing; `refuse_write` in
  // lib.rs is the enforcement.
  const writeAccountId = $derived(note.account_id || $currentAccount);
  const caps = $derived(writeAccountId ? $capabilitiesByAccount[writeAccountId] : undefined);
  const canWriteNotes = $derived(canWrite(caps, 'notes'));
  const canRelocate = $derived(canWrite(caps, 'relocate'));
  const canWriteSidecars = $derived(canWrite(caps, 'sidecars'));

  // Same wording as the context menu's Refetch item.
  const refetchSourceName = $derived.by(() => {
    const kind = $activeAccounts.find((a) => a.id === (note.account_id || $currentAccount || ''))?.backend_kind;
    return kind ? backendLabel(kind) : 'the server';
  });

  const isTmp = $derived(!note.uuid || note.uuid.startsWith('tmp:'));
  // "Desktop only": the cheat sheet lists keyboard shortcuts.
  const showShortcuts = $derived(!$isAndroid);
  const phone = $derived($isAndroid && $androidLayoutMode === 'phone');

  // Move to…: a submenu beside the item on desktop and tablet, a second level
  // inside the same sheet on phone (as the context menu's sheet does).
  let level: 'root' | 'move' = $state('root');
  let moveOpen = $state(false);
  let moveBtn: HTMLButtonElement | undefined = $state();
  // The Move to list's folders, fetched once per opening of the menu — not
  // each time the submenu or the sheet's Move level (re)mounts the list.
  let moveFolders: Map<string, string[]> = $state(new Map());
  let wasOpen = false;
  $effect(() => {
    if (open && !wasOpen && canRelocate) {
      const ids = untrack(() => $activeAccounts.map((a) => a.id));
      void loadFolderTargets(ids).then((m) => (moveFolders = m));
    }
    wasOpen = open;
    if (!open) {
      level = 'root';
      moveOpen = false;
    }
  });

  // Closing because an item was chosen: focus goes back to ⋯ first (as the
  // Popover's own Esc/outside close does), so an item that moves focus —
  // Find, Tags, a dialog — does so from there, and one that does not leaves
  // it somewhere sensible rather than on a removed row.
  function close() {
    closeAll();
    anchor.focus();
  }

  // An editor item (Find, Graph, Keyboard shortcuts, Tags, Mark reviewed)
  // runs once the menu's close has rendered. On phone the menu is a sheet
  // with its own history entry: a dialog opened in the same pass would push
  // its entry BEFORE the sheet pops its own, and that pop would then remove
  // the dialog's entry instead (measured: paneHeader.test.ts, graph → folder).
  function runAfterClose(fn: () => unknown) {
    close();
    void tick().then(() => fn());
  }

  // A note action runs first, then the menu closes — the context menu's
  // order: its optimistic write and IPC send happen before the close.
  function act(fn: () => unknown) {
    void fn();
    close();
  }

  // Duplicating selects the copy. The menu closes first, as the context
  // menu's sheet does, so a phone pane push for the copy cannot land above
  // the sheet's history entry.
  function duplicate() {
    const source = note;
    close();
    void actions.duplicateNote(source);
  }

  function pickFolder(path: string) {
    void actions.moveNoteTo(note, path);
    close();
  }

  // Ask Jodd locked to this note (spec 2026-10-23; owner asked for it here
  // too, 2026-10-24). Read-only, so not write-gated; disabled only when the
  // note's folder is hidden from AI, as in the note-list context menu. Opened
  // after the menu's close renders, so on phone the sheet's history entry
  // pops before the Ask modal pushes its own (see runAfterClose).
  const aiHidden = $derived(aiHiddenFor($hiddenFromAi, writeAccountId, note.label));
  function askAboutNote() {
    if (!writeAccountId) return;
    const scope = noteScope({ account_id: writeAccountId, uuid: note.uuid, title: note.title });
    runAfterClose(() => openAskWith(scope));
  }

  // Long slugs are cut in the toast only; the clipboard gets the whole link.
  const TOAST_SLUG_MAX = 40;
  function copyWikiLink() {
    const slug = noteSlug(note.title, note.uuid);
    const shown = slug.length > TOAST_SLUG_MAX ? `${slug.slice(0, TOAST_SLUG_MAX - 1)}…` : slug;
    // Started inside the click, so the clipboard sees the user's gesture
    // (`copyText` calls the clipboard before its first await). The menu is
    // gone by the time it settles, so a copy that failed both ways says so.
    const write = copyText(`[[${slug}]]`);
    close();
    void write.then((ok) => {
      if (ok) showToast(`Copied [[${shown}]]`);
      else error.set("Couldn't copy the wiki link.");
    });
  }

  // Any close of the menu takes the Move to submenu with it — first, so the
  // submenu's own close (Esc reaches both popovers) finds it already shut and
  // does not pull focus onto a row that is about to go.
  function closeAll() {
    moveOpen = false;
    onclose();
  }

  // Esc inside the menu closes one level: the submenu if it is open.
  function onMenuKey(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !moveOpen) return;
    e.preventDefault();
    e.stopPropagation();
    moveOpen = false;
    moveBtn?.focus();
  }

  // Permanent-delete confirm, rendered outside the Popover so that closing
  // the menu can never unmount it with the decision pending. The state is
  // the shared confirm gate NoteContextMenu uses too.
  const permDelete = createConfirmGate();
  const permDeletePending = permDelete.pending;

  // The action says when the menu may close (R6): after a cancel, right
  // after the optimistic removal began, or after a failure.
  function deleteNote() {
    void actions.deleteNoteWithConfirm(note, permDelete.ask, close);
  }
</script>

{#snippet rootItems()}
  {#if canWriteSidecars}
    <button class="item" type="button" role="menuitem" onclick={() => act(() => actions.togglePin(note))}>
      <span class="icon"><Icon name="pin" /></span>
      <span class="label">{note.pinned ? 'Unpin' : 'Pin'}</span>
    </button>
  {/if}
  {#if canRelocate}
    <button
      bind:this={moveBtn}
      class="item has-submenu"
      type="button"
      role="menuitem"
      aria-haspopup="menu"
      aria-expanded={phone ? undefined : moveOpen}
      onclick={() => (phone ? (level = 'move') : (moveOpen = !moveOpen))}
    >
      <span class="icon"><Icon name="folder" /></span>
      <span class="label">Move to…</span>
      <span class="chevron"><Icon name="chevron-right" size={12} /></span>
    </button>
  {/if}
  {#if showTags}
    <button class="item" type="button" role="menuitem" onclick={() => runAfterClose(onTags)}>
      <span class="icon"><Icon name="tag" /></span>
      <span class="label">Tags…</span>
    </button>
  {/if}
  <button class="item" type="button" role="menuitem" onclick={() => runAfterClose(onFind)}>
    <span class="icon"><Icon name="search" /></span>
    <span class="label">Find in note</span>
  </button>
  {#if !isTmp}
    <button class="item" type="button" role="menuitem" onclick={copyWikiLink}>
      <span class="icon"><Icon name="link" /></span>
      <span class="label">Copy wiki link</span>
    </button>
  {/if}
  <button class="item" type="button" role="menuitem" onclick={() => runAfterClose(onShowGraph)}>
    <span class="icon"><Icon name="graph" /></span>
    <span class="label">Show graph</span>
  </button>
  {#if !isTmp && writeAccountId}
    <button class="item" type="button" role="menuitem" disabled={aiHidden}
      title={aiHidden ? AI_HIDDEN_HINT : 'Ask Jodd a question about this note'} onclick={askAboutNote}>
      <span class="icon"><Icon name="chat" /></span>
      <span class="label">Ask Jodd about this note</span>
    </button>
  {/if}
  {#if showShortcuts}
    <button class="item" type="button" role="menuitem" onclick={() => runAfterClose(onShortcuts)}>
      <span class="icon" aria-hidden="true">ⓘ</span>
      <span class="label">Keyboard shortcuts</span>
    </button>
  {/if}
  {#if canWriteNotes}
    <button class="item" type="button" role="menuitem" disabled={!note.id} onclick={duplicate}>
      <span class="icon"><Icon name="copy" /></span>
      <span class="label">Duplicate</span>
    </button>
  {/if}
  <button
    class="item"
    type="button"
    role="menuitem"
    disabled={!note.id}
    title="Bypass cache and re-pull this note's content from {refetchSourceName}"
    onclick={() => act(() => actions.refetchNote(note))}
  >
    <span class="icon"><Icon name="refresh" /></span>
    <span class="label">Refetch from {refetchSourceName}</span>
  </button>
  {#if trustPending}
    <button
      class="item"
      type="button"
      role="menuitem"
      disabled={reviewBlocked}
      title={reviewBlocked ? 'Load the latest version of this note first' : ''}
      onclick={() => runAfterClose(onMarkReviewed)}
    >
      <span class="icon"><Icon name="eye" /></span>
      <span class="label">Mark reviewed</span>
    </button>
  {/if}
  {#if canRelocate}
    <div class="sep" role="separator"></div>
    <button class="item danger" type="button" role="menuitem" onclick={deleteNote}>
      <span class="icon"><Icon name="trash" /></span>
      <span class="label">Delete</span>
    </button>
  {/if}
{/snippet}

<Popover {anchor} {open} onclose={closeAll} placement="bottom-end">
  <!-- svelte-ignore a11y_interactive_supports_focus -->
  <div
    class="more-menu"
    class:sheet-menu={phone}
    role="menu"
    aria-label="Note actions"
    data-testid="note-more-menu"
    onkeydown={onMenuKey}
  >
    {#if phone && level === 'move'}
      <button class="item back-row" type="button" onclick={() => (level = 'root')}>
        <span class="icon" aria-hidden="true">‹</span>
        <span class="label">Back</span>
      </button>
      <div class="sep"></div>
      <MoveToTargets {note} variant="list" folders={moveFolders} onPick={pickFolder} />
    {:else}
      {@render rootItems()}
    {/if}
    {#if !phone && moveOpen && moveBtn}
      <Popover anchor={moveBtn} bind:open={moveOpen} onclose={() => (moveOpen = false)} placement="right-start">
        <div class="more-menu move-menu" role="menu" aria-label="Move to" data-testid="note-move-menu">
          <MoveToTargets {note} variant="list" folders={moveFolders} onPick={pickFolder} />
        </div>
      </Popover>
    {/if}
  </div>
</Popover>

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
  /* Rows match the note-list context menu's. Inside a phone sheet,
     src/styles/sheet-menu.css restyles `.sheet-menu` rows as touch rows. */
  .more-menu {
    display: flex;
    flex-direction: column;
    min-width: 220px;
    font-size: var(--size);
  }
  .move-menu {
    max-height: 60vh;
    overflow-y: auto;
  }

  .item {
    display: flex;
    align-items: center;
    gap: calc(var(--space-2) + var(--space-0-5));
    width: 100%;
    padding: var(--space-1-5) var(--space-3);
    background: none;
    border: none;
    cursor: pointer;
    text-align: left;
    color: var(--text);
    border-radius: var(--radius-sm);
    transition: background 0.1s;
  }
  .item:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .item:disabled {
    cursor: default;
    opacity: 0.4;
    background: transparent !important;
  }
  .item.danger {
    color: var(--danger);
  }
  .item.has-submenu[aria-expanded='true'] {
    background: var(--surface-hover);
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

  .chevron {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--text-disabled);
    font-size: var(--size-xs);
  }

  .sep {
    height: 1px;
    background: var(--surface-active);
    margin: var(--space-1) var(--space-2);
  }
</style>
