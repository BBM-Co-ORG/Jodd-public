<script lang="ts">
  // The phone's Tags pane (spec 2026-10-20 §5.2): one account's tags, between
  // Folders and the list. The account is the one whose "Tags" row was tapped
  // (phonePaneAccount) — tag selection is per account in today's stores.
  //
  // A tap opens that one tag's list. The header's Select turns on
  // multi-select: rows toggle a check mark, AND/OR appears from two checked
  // tags, and "Show N notes" opens the list. While selecting, the checks and
  // the AND/OR choice are this pane's own draft — nothing reaches the
  // selection stores (and so the list) until Show. Done, or Back, drops the
  // draft and leaves the list's selection exactly as it was.
  //
  // Long-press keeps the tag menu (Rename tag / Delete tag) as a sheet; the
  // operations are the Sidebar's own (controllers/tagActions.ts).
  import { get } from 'svelte/store';
  import PaneHeader from './PaneHeader.svelte';
  import Icon from './Icon.svelte';
  import Modal from './Modal.svelte';
  import PromptDialogs from './PromptDialogs.svelte';
  import { longpress } from '../longpress';
  import { phonePaneAccount } from '../stores/phonePanes';
  import { backLabel, navigateToPane, type Pane } from '../stores/phoneNav';
  import {
    activeAccounts, currentAccount, tagMatchMode, tagsByAccount, noteTagsByAccount,
    capabilitiesByAccount, canWrite,
  } from '../stores/notes';
  import { selectOnlyTags } from '../controllers/viewSelection';
  import { accountShortName, currentListTitle } from '../stores/listView';
  import { renameTag, deleteTag } from '../controllers/tagActions';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';

  const accountId = $derived($phonePaneAccount ?? $currentAccount ?? '');
  const account = $derived($activeAccounts.find((a) => a.id === accountId));
  const subtitle = $derived(
    account && $activeAccounts.length > 1 ? accountShortName(account, $activeAccounts) : undefined,
  );
  const tags = $derived($tagsByAccount.get(accountId) ?? []);

  // ─── Select mode: a draft, applied only by Show ─────────────────────────
  let selecting = $state(false);
  /** Checked tags in the order they were checked — the list title's order. */
  let draft = $state<string[]>([]);
  let draftMode = $state<'AND' | 'OR'>('AND');

  function toggleSelecting() {
    draft = [];
    draftMode = get(tagMatchMode);
    selecting = !selecting;
  }

  function toggleDraft(tag: string) {
    draft = draft.includes(tag) ? draft.filter((t) => t !== tag) : [...draft, tag];
  }

  // How many of the account's notes the draft matches, by the same rule
  // NoteList filters with: AND = every checked tag, OR = any of them.
  const draftCount = $derived.by(() => {
    const byNote = $noteTagsByAccount.get(accountId);
    if (!byNote || draft.length === 0) return 0;
    let n = 0;
    for (const noteTags of byNote.values()) {
      const has = (t: string) => noteTags.includes(t);
      if (draftMode === 'AND' ? draft.every(has) : draft.some(has)) n++;
    }
    return n;
  });

  // ─── Opening the list ───────────────────────────────────────────────────
  // The tag view (controllers/viewSelection.ts, one synchronous step), then
  // the list push. The push is
  // made here rather than left to App's tag watcher: that watcher is keyed on
  // a CHANGE of the selected set, so re-tapping the tag already shown would
  // otherwise do nothing (ruling R21). On a change the watcher's own push
  // lands on the list this one opened and only renames it.
  function openTags(chosen: string[], mode: 'AND' | 'OR') {
    if (!accountId || chosen.length === 0) return;
    selectOnlyTags(accountId, chosen, mode);
    navigateToPane('list', currentListTitle());
  }

  function tapRow(tag: string) {
    if (selecting) toggleDraft(tag);
    else openTags([tag], get(tagMatchMode));
  }

  function show() {
    const chosen = draft;
    const mode = draftMode;
    selecting = false;
    draft = [];
    openTags(chosen, mode);
  }

  // ─── Tag menu (long-press) ──────────────────────────────────────────────
  type SheetModal = { requestClose(): Promise<boolean>; closeInto(pane: Pane, title?: string): boolean };
  let menuTag = $state<string | null>(null);
  let sheet: SheetModal | undefined = $state();
  let prompts: PromptDialogs | undefined = $state();
  const canWriteSidecars = $derived(canWrite($capabilitiesByAccount[accountId], 'sidecars'));

  // App unmounts this pane with the phone branch when the layout leaves the
  // phone; should it outlive that, the menu still closes rather than turn
  // into a card at no anchor, and rotating back does not reopen it (the same
  // rule as the Sidebar's trackPhoneSheet).
  const onPhone = $derived($isAndroid && $androidLayoutMode === 'phone');
  $effect(() => {
    if (!onPhone) menuTag = null;
  });

  function openMenu(e: MouseEvent, tag: string) {
    e.preventDefault();
    e.stopPropagation();
    if (selecting) return;
    menuTag = tag;
  }
  function closeMenu() {
    if (sheet) void sheet.requestClose();
    else menuTag = null;
  }
  function rename(tag: string) {
    closeMenu();
    void renameTag(accountId, tag, (title, value) => prompts!.askName(title, value));
  }
  function remove(tag: string) {
    closeMenu();
    void deleteTag(accountId, tag, (title, message, options) => prompts!.askConfirm(title, message, null, options));
  }
</script>

<div class="phone-pane">
  <PaneHeader title="Tags" {subtitle} back={$backLabel}>
    {#snippet actions()}
      {#if tags.length > 0}
        <button type="button" class="select-toggle" aria-pressed={selecting} onclick={toggleSelecting}>
          {selecting ? 'Done' : 'Select'}
        </button>
      {/if}
    {/snippet}
  </PaneHeader>

  <div class="rows">
    {#if tags.length === 0}
      <p class="empty">No tags in this account yet.</p>
    {/if}
    {#each tags as t (t.tag)}
      {@const checked = draft.includes(t.tag)}
      <button
        type="button"
        class="tag-row"
        class:checked={selecting && checked}
        data-tag={t.tag}
        aria-pressed={selecting ? checked : undefined}
        onclick={() => tapRow(t.tag)}
        oncontextmenu={(e) => openMenu(e, t.tag)}
        use:longpress
      >
        {#if selecting}
          <span class="check-box" aria-hidden="true">
            {#if checked}<span class="check"><Icon name="check" size={16} /></span>{/if}
          </span>
        {/if}
        <span class="row-icon"><Icon name="tag" size={20} /></span>
        <span class="row-name">#{t.tag}</span>
        <span class="tag-count">{t.count}</span>
      </button>
    {/each}
  </div>

  {#if selecting}
    <div class="select-bar">
      {#if draft.length >= 2}
        <div class="tag-mode" role="group" aria-label="Match notes with">
          <button
            type="button"
            aria-pressed={draftMode === 'AND'}
            title="Notes with every checked tag"
            onclick={() => (draftMode = 'AND')}
          >AND</button>
          <button
            type="button"
            aria-pressed={draftMode === 'OR'}
            title="Notes with any checked tag"
            onclick={() => (draftMode = 'OR')}
          >OR</button>
        </div>
      {/if}
      <button type="button" class="show-notes" disabled={draft.length === 0} onclick={show}>
        Show {draftCount} {draftCount === 1 ? 'note' : 'notes'}
      </button>
    </div>
  {/if}
</div>

<PromptDialogs bind:this={prompts} />

{#if menuTag !== null}
  <!-- The tag is the sheet's title, so no "#tag" row is repeated inside. -->
  <Modal bind:this={sheet} phone="sheet" title={`#${menuTag}`} onclose={() => (menuTag = null)}>
    <div class="sheet-menu" role="menu">
      {#if canWriteSidecars}
        <button class="item" type="button" onclick={() => rename(menuTag!)}>
          <span class="icon"><Icon name="pencil" /></span>
          <span class="label">Rename tag</span>
        </button>
        <button class="item danger" type="button" onclick={() => remove(menuTag!)}>
          <span class="icon"><Icon name="trash" /></span>
          <span class="label">Delete tag</span>
        </button>
      {:else}
        <div class="item header" aria-hidden="true">Tag changes aren't available yet</div>
      {/if}
    </div>
  </Modal>
{/if}

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
  .empty {
    margin: var(--space-4);
    color: var(--text-muted);
    font-size: var(--size-sm);
  }
  .select-toggle {
    padding: 0 var(--space-2);
    background: none;
    border: none;
    color: var(--accent-action);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .tag-row {
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
  .tag-row:hover {
    background: var(--surface-hover);
  }
  .tag-row.checked {
    background: var(--accent-wash);
  }
  .check-box {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: var(--space-6);
    height: var(--space-6);
    border: 1px solid var(--border);
    border-radius: 50%;
  }
  .tag-row.checked .check-box {
    background: var(--accent-action);
    border-color: var(--accent-action);
  }
  .check {
    display: inline-flex;
    color: var(--text-inverse);
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
  .tag-count {
    font-size: var(--size-xs);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }
  .select-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: 0 0 auto;
    padding: var(--space-2) var(--space-4);
    padding-bottom: calc(var(--space-2) + env(safe-area-inset-bottom));
    border-top: 1px solid var(--border);
    background: var(--surface-sidebar);
  }
  .tag-mode {
    display: flex;
    gap: var(--space-1);
  }
  .tag-mode button,
  .show-notes {
    min-height: var(--touch-target);
    min-width: var(--touch-target);
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-panel);
    color: var(--text-secondary);
    font: inherit;
    cursor: pointer;
  }
  .tag-mode button[aria-pressed='true'] {
    color: var(--accent-on-tint);
    background: var(--accent-tint);
    border-color: var(--accent-border);
    font-weight: 600;
  }
  .show-notes {
    flex: 1;
    margin-left: auto;
    background: var(--accent-action);
    border-color: var(--accent-action);
    color: var(--text-inverse);
    font-weight: 600;
  }
  .show-notes:disabled {
    opacity: 0.5;
    cursor: default;
  }
  /* The tag sheet's rows: src/styles/sheet-menu.css (shared by every sheet menu). */
</style>
