<script lang="ts">
  // The editor's one-line metadata header (spec 2026-10-20 §6.1, §6.3):
  //
  //   [account ›] Folder › Sub · <date>   [● status]          [#N]
  //
  // It replaces the breadcrumb row, the centred date, the trust chip, the
  // [[slug]] chip and the push/pin banners. The caller (NoteEditor) derives
  // every prop from the open note in ONE reactive pass (gotcha #28); this
  // component keys its own state — the open popover — by the same identity,
  // in a $derived, so a popover opened on one note is never shown for the next.
  //
  // Phone (global gate: $isAndroid, then $androidLayoutMode): the Back label
  // already names the place, so there is no account prefix, and the folder is
  // dropped when it is what Back says.
  import { tick, untrack } from 'svelte';
  import { isAndroid } from '../stores/platform';
  import { androidLayoutMode } from '../stores/viewport';
  import { backLabel } from '../stores/phoneNav';
  import { calendarPref, formatNoteDateTime, formatNoteMetaDate } from '../format/dates';
  import { noteKey } from '../noteIdentity';
  import { viewName } from '../viewName';
  import type { MetaStatus } from '../editor/metaStatus';
  import type { Note } from '../types';
  import Popover from './Popover.svelte';
  import Icon from './Icon.svelte';

  interface Props {
    note: Note;
    /** null when one account (or none) is active. */
    accountLabel: string | null;
    folderPath: string;
    onRevealFolder: () => void;
    status: MetaStatus | null;
    tagCount: number;
    tagsEditable: boolean;
    onOpenTags: (anchor: HTMLElement) => void;
    /** The #N button, for a caller that anchors the tags popover from
     *  elsewhere (⋯ › Tags…); null while it is hidden. */
    tagsButton?: HTMLButtonElement | null;
  }

  let {
    note,
    accountLabel,
    folderPath,
    onRevealFolder,
    status,
    tagCount,
    tagsEditable,
    onOpenTags,
    tagsButton = $bindable(null),
  }: Props = $props();

  const isPhone = $derived($isAndroid && $androidLayoutMode === 'phone');
  const shownAccount = $derived(isPhone ? null : accountLabel);
  const folder = $derived(folderPath || 'Notes');
  // Back names a folder by its leaf (viewName), the same form compared here.
  const showFolder = $derived(!(isPhone && $backLabel != null && viewName(null, folder, '') === $backLabel));
  // The path below the root "Notes", which every folder shares and so says
  // nothing (S23 FE, landscape: "…tes › Inbox" left-truncated). The root
  // alone still names a place; the tooltip keeps the full path.
  const folderText = $derived.by(() => {
    const parts = folder.split('/').filter(Boolean);
    const shown = parts.length > 1 && parts[0] === 'Notes' ? parts.slice(1) : parts;
    return shown.join(' › ');
  });
  // Depending on $calendarPref re-formats when the setting changes.
  // Relative, like the list ("Today 00:58"); the full timestamp is the
  // tooltip and the aria-label (live pass 2026-10-21: "21 Oct 2569 BE, 00:58").
  const dateText = $derived((void $calendarPref, formatNoteMetaDate(note.date)));
  const dateFull = $derived((void $calendarPref, `Last updated ${formatNoteDateTime(note.date)}`));
  const showTags = $derived(tagCount > 0 || tagsEditable);

  const key = $derived(noteKey(note));
  // The popover opens only on a click. `openFor` is the note it was opened
  // on; anything that takes it away — another note, the status going away on
  // its own, Esc or an outside click — clears it, so it never comes back
  // unasked (on phone it is a sheet that would take focus mid-typing).
  //
  // The one exception is the popover's own action. Mark reviewed and Try
  // again clear their state optimistically, so the status vanishes the moment
  // the button is pressed. While that action runs (`acting`), nothing shows;
  // when it settles, the popover stays open only if the same kind of status
  // is back carrying an error — a failed action shows why, without another
  // click. A success, or a status that returns without an error, closes it.
  let openFor = $state<string | null>(null);
  let popoverOpen = $state(false);
  let acting = $state<{ key: string; kind: MetaStatus['kind'] } | null>(null);
  // Derived in the same pass as `key` (gotcha #28): a switch hides it at once.
  const statusShown = $derived(
    !!status && openFor === key && (!acting || (status.kind === acting.kind && !!status.error)),
  );
  let statusAnchor: HTMLButtonElement | null = $state(null);

  function closeStatus(): void {
    openFor = null;
    popoverOpen = false;
    acting = null;
  }

  function toggleStatus(): void {
    if (statusShown) {
      closeStatus();
    } else {
      openFor = key;
      popoverOpen = true;
      acting = null;
    }
  }

  // Housekeeping for what `statusShown` already hides: drop the open state
  // when the note changes, or when the status goes away with no action of
  // the popover's in flight.
  $effect(() => {
    const k = key;
    const present = !!status;
    untrack(() => {
      if (openFor === null) return;
      if (openFor !== k || (!present && !acting)) closeStatus();
    });
  });

  async function runAction(s: MetaStatus): Promise<void> {
    if (!s.action || s.action.disabled) return;
    const k = key;
    acting = { key: k, kind: s.kind };
    try {
      await s.action.run();
    } finally {
      // Let the caller's state from the settled action (its error) render.
      await tick();
      if (acting?.key === k) {
        const now = status;
        if (key === k && openFor === k && now && now.kind === s.kind && now.error) acting = null;
        else closeStatus();
      }
    }
  }

  function statusAria(s: MetaStatus): string {
    return s.error ? `${s.label}: ${s.error}` : s.label;
  }
</script>

<div class="meta-line" class:touch={$isAndroid}>
  <span class="meta-where">
    {#if shownAccount}
      <span class="meta-account" title={accountLabel ?? ''}>{shownAccount}</span>
      {#if showFolder}<span class="meta-sep" aria-hidden="true">›</span>{/if}
    {/if}
    {#if showFolder}
      <!-- Truncates from the LEFT: the leaf is the part worth keeping. -->
      <button
        type="button"
        class="ctx-folder"
        title="Reveal folder in sidebar: {folder}"
        onclick={onRevealFolder}
      ><span class="ctx-folder-text">{folderText}</span></button>
    {/if}
  </span>
  <!-- Outside .meta-where, which shrinks and clips: the date never does
       (live pass 2026-10-21: a long status chip cut it to "00:5"). -->
  {#if shownAccount || showFolder}<span class="meta-sep" aria-hidden="true">·</span>{/if}
  <span class="meta-date" title={dateFull} aria-label={dateFull}>{dateText}</span>

  {#if status}
    <button
      bind:this={statusAnchor}
      type="button"
      class="meta-status"
      class:has-error={!!status.error}
      data-kind={status.kind}
      data-testid={status.kind === 'trust' ? 'trust-chip' : 'meta-status'}
      title={status.detail}
      aria-label={statusAria(status)}
      aria-haspopup="dialog"
      aria-expanded={statusShown}
      onclick={toggleStatus}
    >
      {#if status.icon}<Icon name={status.icon} size={12} />{:else}<span class="meta-dot" aria-hidden="true"></span>{/if}
      <span class="meta-status-label">{status.label}</span>
      {#if status.error}<span class="meta-status-alert" aria-hidden="true">!</span>{/if}
    </button>
  {/if}

  <span class="meta-spacer"></span>

  {#if showTags}
    <button
      bind:this={tagsButton}
      type="button"
      class="meta-tags"
      aria-label="Tags ({tagCount})"
      title="Tags"
      onclick={() => tagsButton && onOpenTags(tagsButton)}
    >#{tagCount}</button>
  {/if}
</div>

{#if status && statusShown && statusAnchor}
  <Popover anchor={statusAnchor} bind:open={popoverOpen} onclose={closeStatus}>
    <!-- A dialog on desktop and tablet; on phone Popover is already a Modal
         sheet, so the role would nest. -->
    <div
      class="meta-pop"
      data-testid="meta-status-popover"
      data-kind={status.kind}
      role={isPhone ? undefined : 'dialog'}
      aria-label={isPhone ? undefined : status.label}
    >
      <p class="meta-pop-detail">{status.detail}</p>
      {#if status.note}<p class="meta-pop-note">{status.note}</p>{/if}
      {#if status.action}
        <button
          type="button"
          class={status.kind === 'trust' ? 'mark-reviewed-btn' : 'retry-sync-btn'}
          disabled={status.action.disabled}
          title={status.action.title ?? ''}
          onclick={() => status && runAction(status)}
        >{status.action.label}</button>
      {/if}
      {#if status.error}<p class="meta-pop-error" role="alert">{status.error}</p>{/if}
    </div>
  </Popover>
{/if}

<style>
  .meta-line {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    /* 28px: the title and body inset. */
    padding: var(--space-2) calc(var(--space-6) + var(--space-1)) 0;
    font-size: var(--size-sm);
    color: var(--text-secondary);
    line-height: var(--leading);
    white-space: nowrap;
  }
  .meta-where {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    overflow: hidden;
  }
  .meta-account {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta-sep {
    flex: none;
    opacity: 0.6;
  }
  .ctx-folder {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    /* Ellipsis on the left: the path is laid out right-to-left as a box, while
       its text keeps its own direction. */
    direction: rtl;
    text-align: left;
    background: none;
    border: none;
    padding: 0;
    font: inherit;
    color: var(--accent-action);
    cursor: pointer;
  }
  .ctx-folder-text {
    unicode-bidi: plaintext;
  }
  .ctx-folder:hover {
    text-decoration: underline;
  }
  .meta-date {
    flex: none;
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }
  .meta-status {
    flex: 0 1 auto;
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    /* Capped, with its own ellipsis, so it never squeezes the date. */
    max-width: min(40%, 12em);
    padding: 0 var(--space-1-5);
    border: none;
    border-radius: var(--radius-sm);
    background: var(--surface-hover);
    color: var(--text-secondary);
    font: inherit;
    cursor: pointer;
  }
  .meta-status:hover {
    color: var(--text);
  }
  .meta-status-label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta-status[data-kind='notSynced'],
  .meta-status[data-kind='pinBlocked'],
  .meta-status.has-error {
    color: var(--danger);
  }
  .meta-dot {
    flex: none;
    width: var(--space-1-5);
    height: var(--space-1-5);
    border-radius: 50%;
    background: currentColor;
  }
  .meta-status-alert {
    flex: none;
    font-weight: 700;
    color: var(--danger);
  }
  .meta-spacer {
    flex: 1 1 0;
  }
  .meta-tags {
    flex: none;
    padding: 0 var(--space-1-5);
    border: none;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--text-secondary);
    font: inherit;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  .meta-tags:hover {
    background: var(--surface-hover);
    color: var(--accent-action);
  }
  /* Android: every control on the line gets a finger-sized hit area. The
     desktop line is untouched. */
  .meta-line.touch {
    min-height: var(--touch-target);
    padding-top: 0;
  }
  .meta-line.touch .ctx-folder,
  .meta-line.touch .meta-status,
  .meta-line.touch .meta-tags {
    min-height: var(--touch-target);
  }
  .meta-line.touch .meta-status,
  .meta-line.touch .meta-tags {
    min-width: var(--touch-target);
    justify-content: center;
  }

  .meta-pop {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-2);
    max-width: min(360px, calc(100vw - 2 * var(--space-4)));
    padding: var(--space-2);
    font-size: var(--size-sm);
    white-space: normal;
  }
  .meta-pop p {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .meta-pop-note {
    color: var(--text-muted);
  }
  .meta-pop-error {
    color: var(--danger);
  }
  .meta-pop button {
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface-editor);
    color: var(--text);
    font: inherit;
    cursor: pointer;
  }
  .meta-pop button:hover:not(:disabled) {
    background: var(--surface-hover);
  }
  .meta-pop button:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
